mod mutations;
mod verify;

use std::sync::{Mutex, MutexGuard, OnceLock};

fn go_fixture_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("go fixture lock poisoned")
}
