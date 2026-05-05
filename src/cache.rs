//! Incremental mutation cache.
//!
//! Caches mutation test results keyed on (Togi version, cache schema version,
//! file content hash, mutation identity, mutation description, test command
//! hash) so unchanged mutations can be skipped on subsequent runs. Results are
//! stored as JSON files in `.togi-cache/`.

use crate::MutationResult;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hasher;
use std::path::{Path, PathBuf};

/// Directory where cache entries are stored.
const CACHE_DIR: &str = ".togi-cache";

/// Bump when mutation/operator/cache semantics change without a package version bump.
const CACHE_SCHEMA_VERSION: &str = "2";

const TOGI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Components that form a unique cache key.
pub struct CacheKey {
    /// Hash of the source file content.
    pub file_content_hash: u64,
    /// Stable mutation identity, including path/range/operator details.
    pub mutation_identity: String,
    /// The mutation description string (operator + what changed).
    pub mutation_description: String,
    /// Hash of the test command string.
    pub test_command_hash: u64,
}

impl CacheKey {
    /// Build a cache key from raw inputs.
    pub fn new(
        file_content: &[u8],
        mutation_identity: &str,
        mutation_description: &str,
        test_command: &str,
    ) -> Self {
        Self {
            file_content_hash: hash_bytes(file_content),
            mutation_identity: mutation_identity.to_string(),
            mutation_description: mutation_description.to_string(),
            test_command_hash: hash_str(test_command),
        }
    }

    /// Compute the hex digest used as the cache filename.
    fn digest(&self) -> String {
        self.digest_with_versions(CACHE_SCHEMA_VERSION, TOGI_VERSION)
    }

    fn digest_with_versions(&self, cache_schema_version: &str, togi_version: &str) -> String {
        let mut hash = 0;
        update_hash_str(&mut hash, cache_schema_version);
        update_hash_str(&mut hash, togi_version);
        update_hash_bytes(&mut hash, &self.file_content_hash.to_le_bytes());
        update_hash_str(&mut hash, &self.mutation_identity);
        update_hash_str(&mut hash, &self.mutation_description);
        update_hash_bytes(&mut hash, &self.test_command_hash.to_le_bytes());
        format!("{hash:016x}")
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
    let mut hasher = DefaultHasher::new();
    hasher.write(data);
    hasher.finish()
}

fn hash_str(s: &str) -> u64 {
    hash_bytes(s.as_bytes())
}

fn update_hash_str(hash: &mut u64, value: &str) {
    update_hash_bytes(hash, &(value.len() as u64).to_le_bytes());
    update_hash_bytes(hash, value.as_bytes());
}

fn update_hash_bytes(hash: &mut u64, bytes: &[u8]) {
    let mut hasher = DefaultHasher::new();
    hasher.write(&(*hash).to_le_bytes());
    hasher.write(bytes);
    *hash = hasher.finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_lookup() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let key = CacheKey::new(
            b"fn main() {}",
            "src/lib.rs:0..1:plus_to_minus",
            "replace + with -",
            "cargo test",
        );
        store(tmp.path(), &key, MutationResult::Killed);
        assert_eq!(lookup(tmp.path(), &key), Some(MutationResult::Killed));
    }

    #[test]
    fn lookup_miss_returns_none() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let key = CacheKey::new(b"code", "src/lib.rs:0..1:op", "desc", "cmd");
        assert_eq!(lookup(tmp.path(), &key), None);
    }

    #[test]
    fn different_content_different_key() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let k1 = CacheKey::new(b"v1", "src/lib.rs:0..1:op", "desc", "cmd");
        let k2 = CacheKey::new(b"v2", "src/lib.rs:0..1:op", "desc", "cmd");
        store(tmp.path(), &k1, MutationResult::Survived);
        store(tmp.path(), &k2, MutationResult::Killed);
        assert_eq!(lookup(tmp.path(), &k1), Some(MutationResult::Survived));
        assert_eq!(lookup(tmp.path(), &k2), Some(MutationResult::Killed));
    }

    #[test]
    fn different_mutation_identity_different_key() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let k1 = CacheKey::new(b"same content", "src/a.rs:0..1:op", "desc", "cmd");
        let k2 = CacheKey::new(b"same content", "src/b.rs:0..1:op", "desc", "cmd");
        store(tmp.path(), &k1, MutationResult::Survived);
        store(tmp.path(), &k2, MutationResult::Killed);
        assert_eq!(lookup(tmp.path(), &k1), Some(MutationResult::Survived));
        assert_eq!(lookup(tmp.path(), &k2), Some(MutationResult::Killed));
    }

    #[test]
    fn cache_schema_version_changes_key() {
        let key = CacheKey::new(b"code", "src/lib.rs:0..1:op", "desc", "cmd");

        assert_ne!(
            key.digest_with_versions("schema-1", TOGI_VERSION),
            key.digest_with_versions("schema-2", TOGI_VERSION)
        );
    }

    #[test]
    fn togi_version_changes_key() {
        let key = CacheKey::new(b"code", "src/lib.rs:0..1:op", "desc", "cmd");

        assert_ne!(
            key.digest_with_versions(CACHE_SCHEMA_VERSION, "0.1.0"),
            key.digest_with_versions(CACHE_SCHEMA_VERSION, "0.2.0")
        );
    }

    #[test]
    fn old_schema_entries_do_not_match_current_key() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let key = CacheKey::new(b"code", "src/lib.rs:0..1:op", "desc", "cmd");
        let old_digest = key.digest_with_versions("old-schema", TOGI_VERSION);
        let old_path = cache_dir(tmp.path()).join(old_digest);
        fs::create_dir_all(cache_dir(tmp.path())).unwrap();
        fs::write(
            old_path,
            serde_json::to_string(&MutationResult::Killed).unwrap(),
        )
        .unwrap();

        assert_eq!(lookup(tmp.path(), &key), None);
    }

    #[test]
    fn clear_removes_cache() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let key = CacheKey::new(b"code", "src/lib.rs:0..1:op", "desc", "cmd");
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
            let key = CacheKey::new(
                format!("file{i}").as_bytes(),
                &format!("src/lib.rs:{i}..{}:op", i + 1),
                "desc",
                "cmd",
            );
            store(tmp.path(), &key, *result);
            assert_eq!(lookup(tmp.path(), &key), Some(*result));
        }
    }
}
