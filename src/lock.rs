//! File-based lock to prevent concurrent togi runs on the same repo.

use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

const LOCK_FILE: &str = ".togi.lock";

/// RAII lock guard. Keeps the lock file open to hold the advisory lock.
/// Holds the open file to keep the `flock` advisory lock active.
pub struct LockGuard {
    _path: PathBuf,
    _file: File,
}

/// Try to acquire an exclusive non-blocking flock. Returns true on success.
#[cfg(unix)]
fn try_flock(file: &File) -> bool {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[cfg(windows)]
fn try_flock(file: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    unsafe {
        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = std::mem::zeroed();
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        ) != 0
    }
}

#[cfg(not(any(unix, windows)))]
fn try_flock(_file: &File) -> bool {
    true
}

/// Acquire a lock for the given project root directory.
/// Returns an error if another togi process is already running.
pub fn acquire(project_root: &Path) -> Result<LockGuard> {
    let path = project_root.join(LOCK_FILE);
    let pid = std::process::id();

    // Try create_new first (fast path), fall back to open if file exists.
    // Retry once on NotFound to handle the race where the holder drops
    // between our create_new and open.
    let mut file = 'acquire: {
        for _ in 0..2 {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    if !try_flock(&f) {
                        let _ = fs::remove_file(&path);
                        bail!(
                            "failed to acquire advisory lock on new file {}; \
                             possible NFS or resource issue",
                            path.display()
                        );
                    }
                    f.write_all(pid.to_string().as_bytes()).with_context(|| {
                        format!("failed to write lock file: {}", path.display())
                    })?;
                    return Ok(LockGuard {
                        _path: path,
                        _file: f,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("failed to create lock file: {}", path.display())
                    });
                }
            }

            match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(f) => break 'acquire f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("failed to open lock file: {}", path.display()));
                }
            }
        }
        bail!(
            "failed to acquire lock file after retries: {}",
            path.display()
        );
    };

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

    Ok(LockGuard {
        _path: path,
        _file: file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn lock_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK_TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK_TEST_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap()
    }

    #[test]
    fn acquire_and_release() {
        let _serial = lock_test_guard();
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(LOCK_FILE);

        {
            let _guard = acquire(dir.path()).unwrap();
            assert!(lock_path.exists());
        }
        // The file stays behind; the advisory lock is released when the guard drops.
        assert!(lock_path.exists());
        let _guard = acquire(dir.path()).unwrap();
    }

    #[test]
    fn rejects_concurrent_lock() {
        let _serial = lock_test_guard();
        let dir = TempDir::new().unwrap();

        let _guard = acquire(dir.path()).unwrap();
        let result = acquire(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn acquires_lock_on_orphaned_file() {
        let _serial = lock_test_guard();
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(LOCK_FILE);

        // Simulate an orphaned lock file with no active flock holder
        fs::write(&lock_path, "0").unwrap();

        let _guard = acquire(dir.path()).unwrap();
        assert!(lock_path.exists());
    }
}
