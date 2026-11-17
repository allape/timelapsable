//! Crash-safe file replacement helpers.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes a file through a temporary sibling and renames it into place only
/// after the complete payload has been flushed and synced.
///
/// This guarantees readers never observe a half-written analysis cache or
/// output image, even if the process is interrupted.
pub fn atomic_write<F>(path: &Path, write: F) -> io::Result<()>
where
    F: FnOnce(&mut File) -> io::Result<()>,
{
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path '{}' has no parent directory", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = parent.join(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        sequence
    ));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        // Best-effort directory sync makes the rename durable on Unix. Some
        // filesystems reject directory fsync, so do not turn that into failure.
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}
