//! With the `embed` feature (on by default), packages language servers into the binary from
//! `embed/` (see `embed/README.md`):
//!
//! ```text
//! embed/<tech>/<version>/<os>-<arch>/build.sh     fills binaries/ (download, build, ...)
//! embed/<tech>/<version>/<os>-<arch>/binaries/    what gets embedded
//! ```
//!
//! For each technology the build uses, in order of precedence:
//!
//! - `MULTILSP_EMBED_<TECH>=off`: nothing; the backend runs from PATH;
//! - `MULTILSP_EMBED_<TECH>=/path/to/executable`: that binary;
//! - the target platform's leaf at `MULTILSP_EMBED_<TECH>_VERSION`, or else the highest version
//!   that has one. Its `binaries/` is used as-is when present; otherwise `build.sh` creates it.
//!
//! Every embedded executable is checked against the build target, and macOS universal binaries
//! are thinned to the target's slice.

#[allow(dead_code)]
#[path = "src/bundle/format.rs"]
mod format;

use std::cmp::Ordering;
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use format::{BackendEntry, FileEntry, Manifest};

type Result<T> = std::result::Result<T, String>;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=embed");
    println!("cargo::rerun-if-changed=src/bundle/format.rs");
    println!("cargo::rerun-if-env-changed=MULTILSP_BUNDLE_CACHE");
    if env::var_os("CARGO_FEATURE_EMBED").is_none() {
        return;
    }
    if let Err(e) = build_bundle() {
        eprintln!("error: building the embedded language-server bundle failed:\n  {e}");
        eprintln!(
            "hint: MULTILSP_EMBED_<TECH>=off skips one technology; \
             `--no-default-features` builds without embedded servers"
        );
        std::process::exit(1);
    }
}

fn build_bundle() -> Result<()> {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
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

    let embed = manifest_dir.join("embed");
    let platform = platform(&target);
    let mut backends = Vec::new();
    for tech in subdirectories(&embed)? {
        let variable = format!("MULTILSP_EMBED_{}", tech.to_uppercase());
        println!("cargo::rerun-if-env-changed={variable}");
        println!("cargo::rerun-if-env-changed={variable}_VERSION");
        let dest = tree.join(&tech);
        let choice = env::var_os(&variable).filter(|v| !v.is_empty());
        if choice.as_deref() == Some("off".as_ref()) {
            continue;
        }
        let version = env::var(format!("{variable}_VERSION")).ok();
        let leaf = select(&embed, &tech, platform.as_deref(), version.as_deref())?;
        let license = leaf
            .as_ref()
            .and_then(|(_, dir)| header(&dir.join("build.sh"), "license"))
            .unwrap_or_else(|| "unknown".into());

        if let Some(path) = choice {
            let path = PathBuf::from(path);
            println!("cargo::rerun-if-changed={}", path.display());
            let program = embed_local_binary(&path, &target, &dest)
                .map_err(|e| format!("{variable}: {e}"))?;
            backends.push(BackendEntry {
                name: tech.clone(),
                version: "local".into(),
                program: format!("{tech}/{program}"),
                license,
                source: path.display().to_string(),
            });
            continue;
        }
        let Some((version, dir)) = leaf else {
            println!(
                "cargo::warning={tech} has nothing for {target} and will run from PATH; \
                 add embed/{tech}/<version>/{}/build.sh or set {variable}=/path/to/{tech}",
                platform.as_deref().unwrap_or("<os>-<arch>")
            );
            continue;
        };
        let binaries = prepare(&dir, &target, platform.as_deref().unwrap(), &cache)?;
        let program = match header(&dir.join("build.sh"), "program") {
            Some(program) => program,
            None => [tech.clone(), format!("bin/{tech}")]
                .into_iter()
                .find(|p| binaries.join(p).is_file())
                .ok_or_else(|| {
                    format!(
                        "{}: no `# program:` line and no {tech} executable",
                        dir.display()
                    )
                })?,
        };
        copy_tree(&binaries, &dest)?;
        backends.push(BackendEntry {
            name: tech.clone(),
            version,
            program: format!("{tech}/{program}"),
            license,
            source: dir
                .strip_prefix(&manifest_dir)
                .unwrap_or(&dir)
                .display()
                .to_string(),
        });
    }
    if target.contains("apple") {
        thin_universal_binaries(&tree, &target)?;
    }
    for backend in &backends {
        let program = tree.join(&backend.program);
        if !program.is_file() {
            return Err(format!("{}: {} is missing", backend.name, backend.program));
        }
        check_executable_for(&program, &target).map_err(|e| format!("{}: {e}", backend.name))?;
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

/// The leaf for `tech` on `platform`: the requested version, or the highest one that has a leaf
/// for the platform. Returns the version and the leaf directory.
fn select(
    embed: &Path,
    tech: &str,
    platform: Option<&str>,
    version: Option<&str>,
) -> Result<Option<(String, PathBuf)>> {
    let Some(platform) = platform else {
        return Ok(None);
    };
    let mut versions: Vec<String> = subdirectories(&embed.join(tech))?
        .into_iter()
        .filter(|v| embed.join(tech).join(v).join(platform).is_dir())
        .collect();
    versions.sort_by(|a, b| compare_versions(a, b));
    let version = match version {
        Some(v) if versions.iter().any(|have| have == v) => v.to_string(),
        Some(v) => {
            return Err(format!(
                "{tech} {v} has nothing for {platform} (available: {})",
                versions.join(", ")
            ));
        }
        None => match versions.pop() {
            Some(v) => v,
            None => return Ok(None),
        },
    };
    let dir = embed.join(tech).join(&version).join(platform);
    Ok(Some((version, dir)))
}

/// Returns the leaf's `binaries/`, running its `build.sh` first if it does not exist. The script
/// writes into a staging directory that is renamed into place only when it succeeds.
fn prepare(leaf: &Path, target: &str, platform: &str, cache: &Path) -> Result<PathBuf> {
    let binaries = leaf.join("binaries");
    if binaries.is_dir() {
        return Ok(binaries);
    }
    let script = leaf.join("build.sh");
    if !script.is_file() {
        return Err(format!(
            "{}: neither binaries/ nor build.sh",
            leaf.display()
        ));
    }
    let staging = leaf.join("binaries.partial");
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let mut command = Command::new("bash");
    command
        .arg(&script)
        .current_dir(leaf)
        .env("OUT", &staging)
        .env("MULTILSP_CACHE", cache)
        .env("MULTILSP_TARGET", target)
        .env("MULTILSP_PLATFORM", platform)
        // Script output goes to the build log, never into the embedded files.
        .stdout(std::process::Stdio::inherit());
    if let Err(e) = run(command, &format!("{}", script.display())) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    fs::rename(&staging, &binaries).map_err(|e| format!("{}: {e}", binaries.display()))?;
    Ok(binaries)
}

/// The value of a `# key: value` comment line in a script's header.
fn header(script: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(script).ok()?;
    let prefix = format!("# {key}:");
    text.lines()
        .take_while(|line| line.starts_with('#') || line.trim().is_empty())
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().to_string())
}

/// Copies a user-supplied executable into the bundle and returns its file name. Refuses anything
/// that is not a native executable for `target`, so a wrong-platform binary fails the build
/// instead of failing on the user's machine.
fn embed_local_binary(path: &Path, target: &str, dest: &Path) -> Result<String> {
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    check_executable_for(path, target)?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy()
        .into_owned();
    fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let copy = dest.join(&name);
    fs::copy(path, &copy).map_err(|e| format!("copying {}: {e}", path.display()))?;
    set_executable(&copy, true)?;
    Ok(name)
}

/// Checks the ELF or Mach-O header against the target's OS and CPU. A macOS universal binary is
/// accepted if it has a slice for the target; it is thinned later.
fn check_executable_for(path: &Path, target: &str) -> Result<()> {
    let mut header = [0u8; 20];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut header))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let arm = target.starts_with("aarch64");
    let (wanted, found) = if target.contains("-linux-") {
        let machine = (n >= 20 && header[..4] == *b"\x7fELF")
            .then(|| u16::from_le_bytes([header[18], header[19]]));
        let found = match machine {
            Some(62) => "Linux x86_64",
            Some(183) => "Linux arm64",
            Some(_) => "Linux, another CPU",
            None => "not a Linux executable",
        };
        (if arm { "Linux arm64" } else { "Linux x86_64" }, found)
    } else if target.contains("-apple-darwin") {
        let be32 = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
        let le32 = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let found = if n < 8 {
            "not a macOS executable"
        } else if le32(0) == 0xfeed_facf {
            match le32(4) {
                0x0100_000c => "macOS arm64",
                0x0100_0007 => "macOS x86_64",
                _ => "macOS, another CPU",
            }
        } else if be32(0) == 0xcafe_babe {
            // Universal: the slice is checked when thinning.
            if arm { "macOS arm64" } else { "macOS x86_64" }
        } else {
            "not a macOS executable"
        };
        (if arm { "macOS arm64" } else { "macOS x86_64" }, found)
    } else {
        return Err(format!("embedding binaries is not supported for {target}"));
    };
    if wanted != found {
        return Err(format!(
            "{} is {found}, but the build target {target} needs {wanted}",
            path.display()
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

fn write(path: &Path, contents: &str) -> Result<()> {
    File::create(path)
        .and_then(|mut f| f.write_all(contents.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn subdirectories(dir: &Path) -> Result<Vec<String>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Copies a directory's files, except dotfiles, preserving permissions.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            fs::copy(&source, &target).map_err(|e| format!("copying {}: {e}", source.display()))?;
        }
    }
    Ok(())
}

/// Orders `v0.24.0`, `22.1.6`, `1.10` numerically component by component.
fn compare_versions(a: &str, b: &str) -> Ordering {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parts(a).cmp(&parts(b)).then_with(|| a.cmp(b))
}

/// The recipe directory name for a target, e.g. `macos-arm64`.
fn platform(target: &str) -> Option<String> {
    let arch = match target.split('-').next()? {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        _ => return None,
    };
    let os = if target.contains("-apple-darwin") {
        "macos"
    } else if target.contains("-linux-") {
        "linux"
    } else {
        return None;
    };
    Some(format!("{os}-{arch}"))
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
