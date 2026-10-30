//! The language servers embedded in this binary, and their installation into the application data
//! directory (`versions/<bundle-id>/`).
//!
//! Installation takes a cross-process lock, extracts into a staging directory next to the final
//! one, verifies every file against the manifest, and publishes it with a rename. Startup does a
//! quick check (completion marker, file sizes, executable bits); [`Bundle::verify`] hashes every
//! file and [`Bundle::repair`] reinstalls.

mod format;

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use format::{BackendEntry, FileEntry, Manifest};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/bundle.rs"));
}

/// Written last into an installation; holds the bundle id.
const COMPLETE_MARKER: &str = ".complete";

pub struct Bundle<'a> {
    id: String,
    manifest: Manifest,
    archive: &'a [u8],
}

/// An installed, checked bundle.
#[derive(Debug, Clone)]
pub struct Installation {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Installation {
    /// Absolute path of a backend's executable.
    pub fn program(&self, backend: &str) -> Option<PathBuf> {
        let entry = self.manifest.backends.iter().find(|b| b.name == backend)?;
        Some(self.root.join(&entry.program))
    }
}

impl Bundle<'static> {
    /// The bundle built into this binary.
    pub fn embedded() -> Self {
        let manifest =
            serde_json::from_str(embedded::MANIFEST).expect("embedded manifest is valid");
        Self {
            id: embedded::BUNDLE_ID.to_string(),
            manifest,
            archive: embedded::ARCHIVE,
        }
    }
}

impl<'a> Bundle<'a> {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// True when the build had nothing to embed for its target.
    pub fn is_empty(&self) -> bool {
        self.manifest.files.is_empty()
    }

    /// Where this bundle is (or would be) installed under `data_dir`.
    pub fn install_dir(&self, data_dir: &Path) -> PathBuf {
        data_dir.join("versions").join(&self.id)
    }

    /// Reuses a valid installation or installs the bundle.
    pub fn install(&self, data_dir: &Path) -> io::Result<Installation> {
        let root = self.install_dir(data_dir);
        if self.quick_check(&root) {
            return Ok(self.installation(root));
        }
        let _lock = self.lock(data_dir)?;
        // Another process may have finished installing while we waited for the lock.
        if self.quick_check(&root) {
            return Ok(self.installation(root));
        }
        self.install_locked(data_dir)
    }

    /// Reinstalls unconditionally.
    pub fn repair(&self, data_dir: &Path) -> io::Result<Installation> {
        let _lock = self.lock(data_dir)?;
        self.install_locked(data_dir)
    }

    /// Hashes every installed file. Returns the problems found; empty means intact.
    pub fn verify(&self, data_dir: &Path) -> io::Result<Vec<String>> {
        let root = self.install_dir(data_dir);
        if !root.exists() {
            return Ok(vec![format!("{} is not installed", root.display())]);
        }
        let mut problems = Vec::new();
        match fs::read_to_string(root.join(COMPLETE_MARKER)) {
            Ok(id) if id.trim() == self.id => {}
            _ => problems.push("installation was not completed".to_string()),
        }
        for file in &self.manifest.files {
            let path = root.join(&file.path);
            match format::sha256_file(&path) {
                Ok(hash) if hash == file.sha256 => {}
                Ok(_) => problems.push(format!("{}: content does not match", file.path)),
                Err(e) => problems.push(format!("{}: {e}", file.path)),
            }
        }
        Ok(problems)
    }

    fn installation(&self, root: PathBuf) -> Installation {
        Installation {
            root,
            manifest: self.manifest.clone(),
        }
    }

    /// Cheap enough for every startup: the marker must name this bundle and every file must have
    /// its recorded size and permissions. The marker alone is not trusted.
    fn quick_check(&self, root: &Path) -> bool {
        let marker_ok =
            fs::read_to_string(root.join(COMPLETE_MARKER)).is_ok_and(|id| id.trim() == self.id);
        marker_ok
            && self.manifest.files.iter().all(|file| {
                fs::symlink_metadata(root.join(&file.path)).is_ok_and(|m| {
                    m.is_file() && m.len() == file.size && (!file.executable || is_executable(&m))
                })
            })
    }

    fn lock(&self, data_dir: &Path) -> io::Result<File> {
        let locks = data_dir.join("locks");
        fs::create_dir_all(&locks)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(locks.join(format!("{}.lock", self.id)))?;
        file.lock()?;
        Ok(file)
    }

    /// Must be called with the lock held.
    fn install_locked(&self, data_dir: &Path) -> io::Result<Installation> {
        let versions = data_dir.join("versions");
        fs::create_dir_all(&versions)?;
        let root = self.install_dir(data_dir);
        let unique = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );

        // Leftovers of interrupted installs of this bundle; nobody else touches them while we
        // hold the lock.
        let staging_prefix = format!(".staging-{}-", self.id);
        for entry in fs::read_dir(&versions)?.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(&staging_prefix)
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }

        let staging = versions.join(format!("{staging_prefix}{unique}"));
        let result = self
            .extract(&staging)
            .and_then(|()| self.check_extracted(&staging));
        if let Err(e) = result {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
        let manifest_json =
            serde_json::to_string_pretty(&self.manifest).map_err(io::Error::other)?;
        fs::write(staging.join("manifest.json"), manifest_json)?;
        let mut marker = File::create(staging.join(COMPLETE_MARKER))?;
        marker.write_all(self.id.as_bytes())?;
        marker.sync_all()?;

        // Move a broken installation aside rather than deleting it in place, so the rename below
        // is the only moment the final path changes.
        if root.exists() {
            let trash = versions.join(format!(".trash-{}-{unique}", self.id));
            fs::rename(&root, &trash)?;
            let _ = fs::remove_dir_all(&trash);
        }
        fs::rename(&staging, &root)?;
        Ok(self.installation(root))
    }

    /// Unpacks the archive, accepting only regular files that the manifest lists.
    fn extract(&self, staging: &Path) -> io::Result<()> {
        fs::create_dir_all(staging)?;
        let decoder = ruzstd::decoding::StreamingDecoder::new(BufReader::new(self.archive))
            .map_err(|e| invalid(format!("bundle archive: {e}")))?;
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            let kind = entry.header().entry_type();
            let listed = self
                .manifest
                .files
                .iter()
                .find(|f| Path::new(&f.path) == path);
            let (true, true, Some(file)) = (
                kind == tar::EntryType::Regular,
                format::is_safe_relative(&path),
                listed,
            ) else {
                return Err(invalid(format!(
                    "unexpected archive entry {} ({kind:?})",
                    path.display()
                )));
            };
            let target = staging.join(&path);
            fs::create_dir_all(target.parent().expect("entry has a parent"))?;
            let mut out = File::create(&target)?;
            io::copy(&mut (&mut entry).take(file.size + 1), &mut out)?;
            set_executable(&target, file.executable)?;
        }
        Ok(())
    }

    fn check_extracted(&self, staging: &Path) -> io::Result<()> {
        for file in &self.manifest.files {
            let path = staging.join(&file.path);
            let hash =
                format::sha256_file(&path).map_err(|e| invalid(format!("{}: {e}", file.path)))?;
            if hash != file.sha256 {
                return Err(invalid(format!(
                    "{}: content does not match the manifest",
                    file.path
                )));
            }
        }
        Ok(())
    }
}

/// `MULTILSP_DATA_DIR`, else the platform's application data directory.
pub fn data_dir() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("MULTILSP_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Application Support/multilsp"));
    }
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    Ok(data_home.join("multilsp"))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

fn set_executable(path: &Path, executable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    struct TestBundle {
        id: String,
        manifest: Manifest,
        archive: Vec<u8>,
    }

    impl TestBundle {
        fn new(files: &[(&str, &[u8], bool)]) -> Self {
            let source = tempfile::tempdir().unwrap();
            let mut entries = Vec::new();
            for (path, contents, executable) in files {
                let full = source.path().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(&full, contents).unwrap();
                entries.push(FileEntry {
                    path: path.to_string(),
                    size: contents.len() as u64,
                    sha256: format::sha256_file(&full).unwrap(),
                    executable: *executable,
                });
            }
            let manifest = Manifest {
                target: "test".into(),
                backends: Vec::new(),
                files: entries,
            };
            let mut builder = tar::Builder::new(Vec::new());
            for file in &manifest.files {
                let mut header = tar::Header::new_gnu();
                header.set_size(file.size);
                header.set_mode(0o644);
                let contents = File::open(source.path().join(&file.path)).unwrap();
                builder
                    .append_data(&mut header, &file.path, contents)
                    .unwrap();
            }
            let archive = compress(&builder.into_inner().unwrap());
            let id = format::bundle_id(&serde_json::to_string(&manifest).unwrap());
            Self {
                id,
                manifest,
                archive,
            }
        }

        fn bundle(&self) -> Bundle<'_> {
            Bundle {
                id: self.id.clone(),
                manifest: self.manifest.clone(),
                archive: &self.archive,
            }
        }
    }

    fn compress(data: &[u8]) -> Vec<u8> {
        ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
    }

    fn sample() -> TestBundle {
        TestBundle::new(&[
            ("tool/bin/tool", b"#!/bin/sh\necho tool\n", true),
            ("tool/lib/include/a.h", b"int a;\n", false),
            ("README", b"hello\n", false),
        ])
    }

    #[test]
    fn installs_once_then_reuses() {
        let data = tempfile::tempdir().unwrap();
        let sample = sample();
        let bundle = sample.bundle();
        let installation = bundle.install(data.path()).unwrap();
        let tool = installation.root.join("tool/bin/tool");
        assert_eq!(fs::read(&tool).unwrap(), b"#!/bin/sh\necho tool\n");
        assert!(is_executable(&fs::metadata(&tool).unwrap()));
        assert!(!is_executable(
            &fs::metadata(installation.root.join("README")).unwrap()
        ));
        assert!(bundle.verify(data.path()).unwrap().is_empty());

        // A second install must not re-extract: a file we add survives.
        fs::write(installation.root.join("sentinel"), b"").unwrap();
        let again = bundle.install(data.path()).unwrap();
        assert_eq!(again.root, installation.root);
        assert!(again.root.join("sentinel").exists());
    }

    #[test]
    fn repairs_a_damaged_installation() {
        let data = tempfile::tempdir().unwrap();
        let sample = sample();
        let bundle = sample.bundle();
        let root = bundle.install(data.path()).unwrap().root;

        // Size change: caught by the startup check.
        fs::write(root.join("README"), b"tampered with\n").unwrap();
        bundle.install(data.path()).unwrap();
        assert_eq!(fs::read(root.join("README")).unwrap(), b"hello\n");

        // Missing executable bit: caught by the startup check.
        set_executable(&root.join("tool/bin/tool"), false).unwrap();
        bundle.install(data.path()).unwrap();
        assert!(is_executable(
            &fs::metadata(root.join("tool/bin/tool")).unwrap()
        ));

        // Same size, different content: only full verification sees it.
        fs::write(root.join("README"), b"HELLO\n").unwrap();
        assert_eq!(
            bundle.verify(data.path()).unwrap(),
            ["README: content does not match"]
        );
        bundle.repair(data.path()).unwrap();
        assert!(bundle.verify(data.path()).unwrap().is_empty());
    }

    #[test]
    fn recovers_from_an_interrupted_install() {
        let data = tempfile::tempdir().unwrap();
        let sample = sample();
        let bundle = sample.bundle();
        let root = bundle.install_dir(data.path());
        // A crash before publishing leaves staging, and a final directory without its marker
        // must not be trusted.
        let staging = data
            .path()
            .join(format!("versions/.staging-{}-1-1", bundle.id()));
        fs::create_dir_all(&staging).unwrap();
        fs::create_dir_all(root.join("tool/bin")).unwrap();
        fs::write(root.join("tool/bin/tool"), b"partial").unwrap();

        bundle.install(data.path()).unwrap();
        assert!(bundle.verify(data.path()).unwrap().is_empty());
        assert!(!staging.exists());
        let leftovers: Vec<_> = fs::read_dir(data.path().join("versions"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(leftovers, [bundle.id().to_string()]);
    }

    #[test]
    fn concurrent_installs_publish_one_complete_copy() {
        let data = Arc::new(tempfile::tempdir().unwrap());
        let sample = Arc::new(sample());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (data, sample) = (data.clone(), sample.clone());
                std::thread::spawn(move || sample.bundle().install(data.path()).unwrap().root)
            })
            .collect();
        let roots: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]));
        assert!(sample.bundle().verify(data.path()).unwrap().is_empty());
    }

    #[test]
    fn rejects_entries_outside_the_manifest() {
        let mut sample = sample();
        // Same archive, but the manifest no longer lists one of its files.
        sample.manifest.files.retain(|f| f.path != "README");
        let data = tempfile::tempdir().unwrap();
        let error = sample.bundle().install(data.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unexpected archive entry README"),
            "{error}"
        );
        assert!(!sample.bundle().install_dir(data.path()).exists());
    }

    #[test]
    fn rejects_path_traversal() {
        let mut header = tar::Header::new_old();
        header.as_old_mut().name[..9].copy_from_slice(b"../escape");
        header.set_size(1);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut tar = Vec::new();
        tar.extend_from_slice(header.as_bytes());
        tar.extend_from_slice(&[b'x'; 512]);
        tar.extend_from_slice(&[0; 1024]);
        let archive = compress(&tar);
        let manifest = Manifest {
            target: "test".into(),
            backends: Vec::new(),
            files: vec![FileEntry {
                path: "../escape".into(),
                size: 1,
                sha256: format::sha256_reader(&b"x"[..]).unwrap(),
                executable: false,
            }],
        };
        let bundle = Bundle {
            id: "evil".into(),
            manifest,
            archive: &archive,
        };
        let data = tempfile::tempdir().unwrap();
        let error = bundle.install(&data.path().join("inner")).unwrap_err();
        assert!(
            error.to_string().contains("unexpected archive entry"),
            "{error}"
        );
        assert!(!data.path().join("inner/versions/escape").exists());
        assert!(!data.path().join("escape").exists());
    }
}
