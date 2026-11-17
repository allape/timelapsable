//! Stable descriptions of input files, used to validate cached analysis.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Identity of a source file at the moment it was analyzed.
///
/// Size plus nanosecond modification time is intentionally cheap: reading and
/// hashing multi-megabyte ARW files on every run would defeat the cache.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceId {
    /// File name without any directory component.
    pub file: String,
    /// File length in bytes.
    pub size: u64,
    /// Modification time as nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
}

impl SourceId {
    /// Reads the current identity of `path`.
    pub fn from_path(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("cannot stat input file '{}'", path.display()))?;
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        Ok(Self {
            file,
            size: metadata.len(),
            mtime_ns,
        })
    }
}
