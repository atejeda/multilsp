//! The bundle format, shared by `build.rs` (which writes bundles) and the runtime (which installs
//! them): a manifest listing every file with its size and SHA-256, and a zstd-compressed tar
//! holding exactly those files. Writing archives lives in `build.rs`, which runs on the build
//! host, so the runtime only needs a pure-Rust decoder.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Rust target triple the bundle was built for.
    pub target: String,
    pub backends: Vec<BackendEntry>,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendEntry {
    /// Matches [`crate::BackendSpec::name`].
    pub name: String,
    pub version: String,
    /// Executable path relative to the installation root.
    pub program: String,
    /// SPDX license expression.
    pub license: String,
    /// Where the build obtained it.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path with `/` separators.
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

/// Content-addressed bundle id: the manifest lists every file's hash, so hashing the manifest
/// identifies the whole bundle.
// Used by build.rs and tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn bundle_id(manifest_json: &str) -> String {
    hex(&Sha256::digest(manifest_json.as_bytes()))[..32].to_string()
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    sha256_reader(File::open(path)?)
}

pub fn sha256_reader(mut reader: impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buffer[..n]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// True if `path` is relative and only descends: no root, `..`, `.` or prefix components.
pub fn is_safe_relative(path: &Path) -> bool {
    path.components().next().is_some()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}
