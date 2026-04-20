//! File-based lock to prevent concurrent togi runs on the same repo.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = ".togi.lock";

/// RAII lock guard. Removes the lock file on drop.
pub struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Acquire a lock for the given project root directory.
/// Returns an error if another togi process is already running.
pub fn acquire(project_root: &Path) -> Result<LockGuard> {
    let path = project_root.join(LOCK_FILE);
    let pid = std::process::id();

    if path.exists() {
        let contents = fs::read_to_string(&path).unwrap_or_default();
        if let Ok(existing_pid) = contents.trim().parse::<u32>()
            && process_alive(existing_pid)
        {
            bail!(
                "another togi process (PID {}) is already running in this directory.\n\
                 If this is stale, remove {} manually.",
                existing_pid,
                path.display()
            );
        }
        // Stale lock — previous process died without cleanup
        fs::remove_file(&path).ok();
    }

    fs::write(&path, pid.to_string())
        .with_context(|| format!("failed to create lock file: {}", path.display()))?;

    Ok(LockGuard { path })
}

fn process_alive(pid: u32) -> bool {
    // PID 0 and overflow values are not valid process IDs
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // On Unix, signal 0 checks if process exists without sending a signal
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
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
