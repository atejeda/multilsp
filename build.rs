//! With the `embed` feature (on by default), builds the language-server bundle for the target and
//! embeds it: clangd is downloaded from its pinned release and checked against the pinned SHA-256
//! (see `bundle/sources.json`). On a target with no pinned artifacts the bundle is empty and the
//! build warns.
//!
//! Downloads are cached in `MULTILSP_BUNDLE_CACHE` (default: `target/bundle-cache`).

#[allow(dead_code)]
#[path = "src/bundle/format.rs"]
mod format;

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use format::{BackendEntry, FileEntry, Manifest};
use serde::Deserialize;

type Result<T> = std::result::Result<T, String>;

#[derive(Deserialize)]
struct Sources {
    clangd: ClangdSource,
}

#[derive(Deserialize)]
struct ClangdSource {
    version: String,
    license: String,
    /// Keyed by Rust target triple.
    artifacts: BTreeMap<String, Artifact>,
}

#[derive(Deserialize)]
struct Artifact {
    url: String,
    sha256: String,
    /// Top-level directory inside the zip.
    strip_prefix: String,
    /// Paths to keep, relative to `strip_prefix`; entries ending in `/` keep a whole directory.
    include: Vec<String>,
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=bundle/sources.json");
    println!("cargo::rerun-if-changed=src/bundle/format.rs");
    println!("cargo::rerun-if-env-changed=MULTILSP_BUNDLE_CACHE");
    if env::var_os("CARGO_FEATURE_EMBED").is_none() {
        return;
    }
    if let Err(e) = build_bundle() {
        eprintln!("error: building the embedded language-server bundle failed:\n  {e}");
        eprintln!("hint: build with `--no-default-features` for a binary without embedded servers");
        std::process::exit(1);
    }
}

fn build_bundle() -> Result<()> {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let sources: Sources = serde_json::from_str(&read(&manifest_dir.join("bundle/sources.json"))?)
        .map_err(|e| format!("bundle/sources.json: {e}"))?;
    let cache = match env::var_os("MULTILSP_BUNDLE_CACHE") {
        Some(dir) => PathBuf::from(dir),
        None => env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("target"))
            .join("bundle-cache"),
    };
    fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;

    let tree = out.join("bundle-tree");
    if tree.exists() {
        fs::remove_dir_all(&tree).map_err(|e| format!("{}: {e}", tree.display()))?;
    }

    fs::create_dir_all(&tree).map_err(|e| format!("{}: {e}", tree.display()))?;

    let clangd = &sources.clangd;
    let mut backends = Vec::new();
    match clangd.artifacts.get(&target) {
        Some(artifact) => {
            let archive = fetch(&artifact.url, &artifact.sha256, &cache)?;
            extract(&archive, artifact, &tree.join("clangd"))?;
            backends.push(BackendEntry {
                name: "clangd".into(),
                version: clangd.version.clone(),
                program: "clangd/bin/clangd".into(),
                license: clangd.license.clone(),
                source: artifact.url.clone(),
            });
        }
        None => {
            let known: Vec<_> = clangd.artifacts.keys().map(String::as_str).collect();
            println!(
                "cargo::warning=no embedded language servers for {target} (supported: {}); \
                 the binary will use servers from PATH",
                known.join(", ")
            );
        }
    }
    if target.contains("apple") {
        thin_universal_binaries(&tree, &target)?;
    }

    let manifest = Manifest {
        target: target.clone(),
        backends,
        files: list_files(&tree)?,
    };
    let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
    let id = format::bundle_id(&manifest_json);

    // Maximum compression for release builds; fast builds otherwise.
    let release = env::var("PROFILE").as_deref() == Ok("release");
    let level = if release { 19 } else { 3 };
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let archive_path = out.join("bundle.tar.zst");
    let file =
        File::create(&archive_path).map_err(|e| format!("{}: {e}", archive_path.display()))?;
    write_archive(
        &tree,
        &manifest.files,
        io::BufWriter::new(file),
        level,
        workers,
    )
    .map_err(|e| format!("writing the bundle archive: {e}"))?;
    write(&out.join("manifest.json"), &manifest_json)?;
    write(
        &out.join("bundle.rs"),
        &format!(
            "pub const BUNDLE_ID: &str = {id:?};\n\
             pub const MANIFEST: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/manifest.json\"));\n\
             pub static ARCHIVE: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/bundle.tar.zst\"));\n"
        ),
    )?;
    fs::remove_dir_all(&tree).map_err(|e| format!("{}: {e}", tree.display()))?;
    Ok(())
}

/// Returns the cached download of `url`, fetching it with curl first if needed.
fn fetch(url: &str, sha256: &str, cache: &Path) -> Result<PathBuf> {
    let name = url.rsplit('/').next().unwrap_or("download");
    let downloads = cache.join("downloads");
    let path = downloads.join(format!("{sha256}-{name}"));
    if path.exists() && hash(&path)? == sha256 {
        return Ok(path);
    }
    fs::create_dir_all(&downloads).map_err(|e| format!("{}: {e}", downloads.display()))?;
    let partial = path.with_extension("partial");
    let mut curl = Command::new("curl");
    curl.args([
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--retry",
        "3",
        "--output",
    ])
    .arg(&partial)
    .arg(url);
    run(curl, &format!("downloading {url}"))?;
    let actual = hash(&partial)?;
    if actual != sha256 {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "checksum mismatch for {url}: expected {sha256}, got {actual}"
        ));
    }
    fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn extract(archive: &Path, artifact: &Artifact, dest: &Path) -> Result<()> {
    let open = || File::open(archive).map_err(|e| format!("{}: {e}", archive.display()));
    let keep = |name: &str| -> Option<PathBuf> {
        let relative = name
            .strip_prefix(&artifact.strip_prefix)?
            .strip_prefix('/')?;
        let wanted = artifact
            .include
            .iter()
            .any(|keep| match keep.strip_suffix('/') {
                Some(dir) => relative.starts_with(dir) && relative[dir.len()..].starts_with('/'),
                None => relative == keep,
            });
        let path = PathBuf::from(relative);
        (wanted && format::is_safe_relative(&path)).then_some(path)
    };
    let mut kept = 0;
    let mut save = |relative: &Path, reader: &mut dyn Read, mode: u32| -> Result<()> {
        let path = dest.join(relative);
        fs::create_dir_all(path.parent().unwrap())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        io::copy(reader, &mut file).map_err(|e| format!("{}: {e}", path.display()))?;
        set_executable(&path, mode & 0o111 != 0)?;
        kept += 1;
        Ok(())
    };
    let mut zip = zip::ZipArchive::new(BufReader::new(open()?))
        .map_err(|e| format!("{}: {e}", archive.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("{}: {e}", archive.display()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|e| format!("{}: {e}", archive.display()))?
            .into_owned();
        if let Some(relative) = keep(&name) {
            let mode = entry.unix_mode().unwrap_or(0o644);
            save(&relative, &mut entry, mode)?;
        }
    }
    if kept == 0 {
        return Err(format!(
            "{} contained none of the expected files",
            artifact.url
        ));
    }
    Ok(())
}

/// Replaces macOS universal binaries with the slice for the target, halving their size.
fn thin_universal_binaries(tree: &Path, target: &str) -> Result<()> {
    let cpu_type: u32 = if target.starts_with("aarch64") {
        0x0100_000c
    } else {
        0x0100_0007
    };
    for file in list_files(tree)? {
        let path = tree.join(&file.path);
        let mut magic = [0; 4];
        let mut f = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if f.read(&mut magic).map_err(|e| e.to_string())? < 4 || magic != [0xca, 0xfe, 0xba, 0xbe] {
            continue;
        }
        let data = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let be32 = |at: usize| {
            data.get(at..at + 4)
                .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        };
        let count = be32(4).unwrap_or(0);
        // Java class files share the magic; their "count" is a version number in the thousands.
        if count == 0 || count > 16 {
            continue;
        }
        let slice = (0..count as usize).find_map(|i| {
            let at = 8 + i * 20;
            (be32(at)? == cpu_type).then(|| {
                let (offset, size) = (be32(at + 8)? as usize, be32(at + 12)? as usize);
                data.get(offset..offset + size)
            })?
        });
        let slice = slice.ok_or_else(|| format!("{} has no slice for {target}", file.path))?;
        fs::write(&path, slice).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn list_files(root: &Path) -> Result<Vec<FileEntry>> {
    fn walk(root: &Path, dir: &Path, files: &mut Vec<FileEntry>) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .collect::<io::Result<_>>()
            .map_err(|e| e.to_string())?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = entry.metadata().map_err(|e| e.to_string())?;
            if metadata.is_dir() {
                walk(root, &path, files)?;
            } else {
                let relative = path.strip_prefix(root).unwrap();
                let parts: Vec<_> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect();
                files.push(FileEntry {
                    path: parts.join("/"),
                    size: metadata.len(),
                    sha256: hash(&path)?,
                    executable: is_executable(&metadata),
                });
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    Ok(files)
}

fn run(mut command: Command, what: &str) -> Result<String> {
    let output = command
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("{what}: cannot run {:?}: {e}", command.get_program()))?;
    if !output.status.success() {
        return Err(format!("{what} failed with {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn hash(path: &Path) -> Result<String> {
    format::sha256_file(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, contents: &str) -> Result<()> {
    File::create(path)
        .and_then(|mut f| f.write_all(contents.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes `files`, read from under `root`, as a deterministic zstd-compressed tar.
fn write_archive(
    root: &Path,
    files: &[FileEntry],
    out: impl Write,
    level: i32,
    workers: u32,
) -> io::Result<()> {
    let mut encoder = zstd::Encoder::new(out, level)?;
    if workers > 1 {
        encoder.multithread(workers)?;
    }
    let mut builder = tar::Builder::new(encoder);
    for file in files {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(file.size);
        header.set_mode(if file.executable { 0o755 } else { 0o644 });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        builder.append_data(&mut header, &file.path, File::open(root.join(&file.path))?)?;
    }
    builder.into_inner()?.finish()?.flush()
}
