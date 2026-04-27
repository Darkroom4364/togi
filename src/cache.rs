//! Incremental mutation cache.
//!
//! Caches mutation test results keyed on (file content hash, mutation
//! description, test command hash) so unchanged mutations can be skipped
//! on subsequent runs. Results are stored as JSON files in `.togi-cache/`.

use crate::MutationResult;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use siphasher::sip::SipHasher;

/// Directory where cache entries are stored.
const CACHE_DIR: &str = ".togi-cache";

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
        let mut h = SipHasher::new();
        self.file_content_hash.hash(&mut h);
        self.mutation_description.hash(&mut h);
        self.test_command_hash.hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

/// Look up a previously cached result.
///
/// Returns `None` on cache miss or any I/O / deserialization error.
pub fn lookup(project_root: &Path, key: &CacheKey) -> Option<MutationResult> {
    let path = entry_path(project_root, key);
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Store a mutation result in the cache.
///
/// Creates the cache directory if it doesn't exist. Silently ignores
/// write errors so cache failures never break the main pipeline.
pub fn store(project_root: &Path, key: &CacheKey, result: MutationResult) {
    let path = entry_path(project_root, key);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, serde_json::to_string(&result).unwrap_or_default());
}

/// Delete all cached results.
pub fn clear(project_root: &Path) -> std::io::Result<()> {
    let dir = cache_dir(project_root);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Return the cache directory path under the given project root.
fn cache_dir(project_root: &Path) -> PathBuf {
    project_root.join(CACHE_DIR)
}

/// Return the file path for a given cache key.
fn entry_path(project_root: &Path, key: &CacheKey) -> PathBuf {
    cache_dir(project_root).join(key.digest())
}

fn hash_bytes(data: &[u8]) -> u64 {
    let mut h = SipHasher::new();
    data.hash(&mut h);
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = SipHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_lookup() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let key = CacheKey::new(b"fn main() {}", "replace + with -", "cargo test");
        store(tmp.path(), &key, MutationResult::Killed);
        assert_eq!(lookup(tmp.path(), &key), Some(MutationResult::Killed));
    }

    #[test]
    fn lookup_miss_returns_none() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let key = CacheKey::new(b"code", "desc", "cmd");
        assert_eq!(lookup(tmp.path(), &key), None);
    }

    #[test]
    fn different_content_different_key() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let k1 = CacheKey::new(b"v1", "desc", "cmd");
        let k2 = CacheKey::new(b"v2", "desc", "cmd");
        store(tmp.path(), &k1, MutationResult::Survived);
        store(tmp.path(), &k2, MutationResult::Killed);
        assert_eq!(lookup(tmp.path(), &k1), Some(MutationResult::Survived));
        assert_eq!(lookup(tmp.path(), &k2), Some(MutationResult::Killed));
    }

    #[test]
    fn clear_removes_cache() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let key = CacheKey::new(b"code", "desc", "cmd");
        store(tmp.path(), &key, MutationResult::Timeout);
        assert!(cache_dir(tmp.path()).exists());
        let _ = clear(tmp.path());
        assert!(!cache_dir(tmp.path()).exists());
        assert_eq!(lookup(tmp.path(), &key), None);
    }

    #[test]
    fn all_result_variants_roundtrip() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        for (i, result) in [
            MutationResult::Killed,
            MutationResult::Survived,
            MutationResult::Timeout,
            MutationResult::BuildError,
        ]
        .iter()
        .enumerate()
        {
            let key = CacheKey::new(format!("file{i}").as_bytes(), "desc", "cmd");
            store(tmp.path(), &key, *result);
            assert_eq!(lookup(tmp.path(), &key), Some(*result));
        }
    }
}
