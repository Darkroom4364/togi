//! File-based lock to prevent concurrent togi runs on the same repo.

use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
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

/// Try to acquire an exclusive non-blocking flock, returning the OS error on
/// failure so callers can report the real cause (contention vs resource
/// exhaustion such as ENOLCK).
#[cfg(unix)]
fn try_flock(file: &File) -> std::io::Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn try_flock(file: &File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    unsafe {
        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = std::mem::zeroed();
        if LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        ) != 0
        {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn try_flock(_file: &File) -> std::io::Result<()> {
    Ok(())
}

/// flock can fail transiently under load (ENOLCK, NFS hiccups), which used to
/// surface as a misleading "another togi process is already running" (#416).
/// Retry briefly; genuine contention still fails, just after ~250ms.
fn flock_with_retry(file: &File) -> std::io::Result<()> {
    let mut attempts = 0;
    loop {
        match try_flock(file) {
            Ok(()) => return Ok(()),
            Err(_) if attempts < 10 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(all(test, unix))]
mod test_hooks {
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{LazyLock, Mutex};

    struct AfterCreatePause {
        created: Sender<()>,
        resume: Receiver<()>,
    }

    static AFTER_CREATE_PAUSE: LazyLock<Mutex<Option<AfterCreatePause>>> =
        LazyLock::new(|| Mutex::new(None));

    pub(super) fn install_after_create_pause() -> (Receiver<()>, Sender<()>) {
        let (created, created_rx) = channel();
        let (resume, resume_rx) = channel();
        let mut pause = AFTER_CREATE_PAUSE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(pause.is_none(), "after-create pause already installed");
        *pause = Some(AfterCreatePause {
            created,
            resume: resume_rx,
        });
        (created_rx, resume)
    }

    pub(super) fn pause_after_create() {
        if let Some(pause) = AFTER_CREATE_PAUSE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            pause.created.send(()).unwrap();
            pause.resume.recv().unwrap();
        }
    }
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
                    #[cfg(all(test, unix))]
                    test_hooks::pause_after_create();
                    if let Err(e) = flock_with_retry(&f) {
                        bail!(
                            "failed to acquire advisory lock on new file {}: {e}",
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

    if let Err(e) = flock_with_retry(&file) {
        bail!(
            "could not lock {}: {e}. Another togi process may be running in this \
             directory; if this lock is stale, remove the file manually.",
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
    use std::fs;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::io::{BufRead, BufReader, Read};
    #[cfg(unix)]
    use std::process::{Command, Stdio};
    #[cfg(unix)]
    use std::thread;

    fn lock_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK_TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK_TEST_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn acquire_and_release() {
        let _serial = lock_test_guard();
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(LOCK_FILE);

        let guard = acquire(dir.path()).unwrap();
        assert!(lock_path.exists());
        drop(guard);

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

    #[cfg(unix)]
    #[test]
    fn fast_path_flock_failure_keeps_holder_inode_reachable() {
        let _serial = lock_test_guard();
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(LOCK_FILE);
        let (created, resume) = test_hooks::install_after_create_pause();

        let root = dir.path().to_path_buf();
        let creator = thread::spawn(move || acquire(&root));
        created.recv().unwrap();

        // A separate process owns the advisory lock on the inode A just created.
        let mut holder = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "lock::tests::fast_path_lock_race_holder",
                "--nocapture",
            ])
            .env("TOGI_LOCK_RACE_HOLDER_PATH", &lock_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut holder_stdout = BufReader::new(holder.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert_ne!(holder_stdout.read_line(&mut line).unwrap(), 0);
            if line.trim_end() == "TOGI_LOCK_RACE_HOLDER_READY" {
                break;
            }
        }

        resume.send(()).unwrap();
        assert!(creator.join().unwrap().is_err());
        assert!(lock_path.exists());
        assert!(acquire(dir.path()).is_err());

        std::io::Write::write_all(holder.stdin.as_mut().unwrap(), b"release").unwrap();
        drop(holder.stdin.take());
        let mut holder_output = String::new();
        holder_stdout.read_to_string(&mut holder_output).unwrap();
        assert!(holder.wait().unwrap().success(), "{holder_output}");

        let _guard = acquire(dir.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn fast_path_lock_race_holder() {
        let Some(lock_path) = std::env::var_os("TOGI_LOCK_RACE_HOLDER_PATH") else {
            return;
        };

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        try_flock(&file).unwrap();
        println!("TOGI_LOCK_RACE_HOLDER_READY");
        std::io::Write::flush(&mut std::io::stdout()).unwrap();

        let mut release = [0];
        std::io::Read::read_exact(&mut std::io::stdin(), &mut release).unwrap();
    }
}
