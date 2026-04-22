//! File-based lock to prevent concurrent togi runs on the same repo.

use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

const LOCK_FILE: &str = ".togi.lock";

/// RAII lock guard. Removes the lock file on drop.
/// Holds the open file to keep the `flock` advisory lock active.
pub struct LockGuard {
    path: PathBuf,
    _file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Try to acquire an exclusive non-blocking flock. Returns true on success.
#[cfg(unix)]
fn try_flock(file: &File) -> bool {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[cfg(not(unix))]
fn try_flock(_file: &File) -> bool {
    true
}

/// Acquire a lock for the given project root directory.
/// Returns an error if another togi process is already running.
pub fn acquire(project_root: &Path) -> Result<LockGuard> {
    let path = project_root.join(LOCK_FILE);
    let pid = std::process::id();

    // Fast path: no contention — create_new ensures atomicity
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            if !try_flock(&file) {
                bail!("failed to acquire advisory lock on {}", path.display());
            }
            file.write_all(pid.to_string().as_bytes())
                .with_context(|| format!("failed to write lock file: {}", path.display()))?;
            return Ok(LockGuard { path, _file: file });
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("failed to create lock file: {}", path.display()));
        }
    }

    // Lock file exists — open and try to acquire flock
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("failed to open lock file: {}", path.display()))?;

    if !try_flock(&file) {
        bail!(
            "another togi process is already running in this directory.\n\
             If this is stale, remove {} manually.",
            path.display()
        );
    }

    // We hold the flock — previous holder is gone. Overwrite with our PID.
    file.set_len(0)
        .with_context(|| format!("failed to truncate lock file: {}", path.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek lock file: {}", path.display()))?;
    file.write_all(pid.to_string().as_bytes())
        .with_context(|| format!("failed to write lock file: {}", path.display()))?;

    Ok(LockGuard { path, _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_and_release() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(LOCK_FILE);

        {
            let _guard = acquire(dir.path()).unwrap();
            assert!(lock_path.exists());
        }
        // Lock removed after drop
        assert!(!lock_path.exists());
    }

    #[test]
    fn rejects_concurrent_lock() {
        let dir = TempDir::new().unwrap();

        let _guard = acquire(dir.path()).unwrap();
        let result = acquire(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn cleans_stale_lock() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(LOCK_FILE);

        // Write a lock with a PID that (almost certainly) doesn't exist
        fs::write(&lock_path, "4294967295").unwrap();

        let _guard = acquire(dir.path()).unwrap();
        assert!(lock_path.exists());
    }
}
