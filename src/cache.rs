//! Incremental mutation cache.
//!
//! Caches mutation test results keyed on (file content hash, mutation
//! description, test command hash) so unchanged mutations can be skipped
//! on subsequent runs. Results are stored as JSON files in `.togi-cache/`.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Directory where cache entries are stored.
const CACHE_DIR: &str = ".togi-cache";

/// A cached mutation test result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CachedResult {
    Killed,
    Survived,
    Timeout,
    BuildError,
}

/// Components that form a unique cache key.
pub struct CacheKey {
    /// Hash of the source file content.
    pub file_content_hash: u64,
    /// The mutation description string (operator + what changed).
    pub mutation_description: String,
    /// Hash of the test command string.
    pub test_command_hash: u64,
}

impl CacheKey {
    /// Build a cache key from raw inputs.
    pub fn new(file_content: &[u8], mutation_description: &str, test_command: &str) -> Self {
        Self {
            file_content_hash: hash_bytes(file_content),
            mutation_description: mutation_description.to_string(),
            test_command_hash: hash_str(test_command),
        }
    }

    /// Compute the hex digest used as the cache filename.
    fn digest(&self) -> String {
        let mut h = DefaultHasher::new();
        self.file_content_hash.hash(&mut h);
        self.mutation_description.hash(&mut h);
        self.test_command_hash.hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

/// Look up a previously cached result.
///
/// Returns `None` on cache miss or any I/O / deserialization error.
pub fn lookup(key: &CacheKey) -> Option<CachedResult> {
    let path = entry_path(key);
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Store a mutation result in the cache.
///
/// Creates the cache directory if it doesn't exist. Silently ignores
/// write errors so cache failures never break the main pipeline.
pub fn store(key: &CacheKey, result: CachedResult) {
    let path = entry_path(key);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, serde_json::to_string(&result).unwrap_or_default());
}

/// Delete all cached results.
pub fn clear() {
    let dir = cache_dir();
    if dir.exists() {
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Return the cache directory path (relative to cwd).
fn cache_dir() -> PathBuf {
    PathBuf::from(CACHE_DIR)
}

/// Return the file path for a given cache key.
fn entry_path(key: &CacheKey) -> PathBuf {
    cache_dir().join(key.digest())
}

fn hash_bytes(data: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Run a test body inside a temporary directory so cache files don't
    /// pollute the repo.
    fn in_temp_dir(f: impl FnOnce()) {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let prev = env::current_dir().expect("cwd");
        env::set_current_dir(tmp.path()).expect("chdir");
        f();
        env::set_current_dir(prev).expect("restore cwd");
    }

    #[test]
    fn store_and_lookup() {
        in_temp_dir(|| {
            let key = CacheKey::new(b"fn main() {}", "replace + with -", "cargo test");
            store(&key, CachedResult::Killed);
            assert_eq!(lookup(&key), Some(CachedResult::Killed));
        });
    }

    #[test]
    fn lookup_miss_returns_none() {
        in_temp_dir(|| {
            let key = CacheKey::new(b"code", "desc", "cmd");
            assert_eq!(lookup(&key), None);
        });
    }

    #[test]
    fn different_content_different_key() {
        in_temp_dir(|| {
            let k1 = CacheKey::new(b"v1", "desc", "cmd");
            let k2 = CacheKey::new(b"v2", "desc", "cmd");
            store(&k1, CachedResult::Survived);
            store(&k2, CachedResult::Killed);
            assert_eq!(lookup(&k1), Some(CachedResult::Survived));
            assert_eq!(lookup(&k2), Some(CachedResult::Killed));
        });
    }

    #[test]
    fn clear_removes_cache() {
        in_temp_dir(|| {
            let key = CacheKey::new(b"code", "desc", "cmd");
            store(&key, CachedResult::Timeout);
            assert!(cache_dir().exists());
            clear();
            assert!(!cache_dir().exists());
            assert_eq!(lookup(&key), None);
        });
    }

    #[test]
    fn all_result_variants_roundtrip() {
        in_temp_dir(|| {
            for (i, result) in [
                CachedResult::Killed,
                CachedResult::Survived,
                CachedResult::Timeout,
                CachedResult::BuildError,
            ]
            .iter()
            .enumerate()
            {
                let key = CacheKey::new(format!("file{i}").as_bytes(), "desc", "cmd");
                store(&key, *result);
                assert_eq!(lookup(&key), Some(*result));
            }
        });
    }
}
