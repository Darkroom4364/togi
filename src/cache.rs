//! Incremental mutation cache.
//!
//! Caches mutation test results keyed on (Togi version, cache schema version,
//! file content hash, mutation identity, mutation description, test command
//! hash) so unchanged mutations can be skipped on subsequent runs. Results are
//! stored as JSON files in `.togi-cache/`.

use crate::MutationResult;
use serde::{Deserialize, Serialize};
use std::fs;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Directory where cache entries are stored.
const CACHE_DIR: &str = ".togi-cache";

/// Bump when mutation/operator/cache semantics change without a package version bump.
const CACHE_SCHEMA_VERSION: &str = "3";

const HISTORY_FILE: &str = "history.json";
const HISTORY_SCHEMA_VERSION: u32 = 2;

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
        let mut hasher = Fnv64Hasher::default();
        update_hash_str(&mut hasher, cache_schema_version);
        update_hash_str(&mut hasher, togi_version);
        update_hash_bytes(&mut hasher, &self.file_content_hash.to_le_bytes());
        update_hash_str(&mut hasher, &self.mutation_identity);
        update_hash_str(&mut hasher, &self.mutation_description);
        update_hash_bytes(&mut hasher, &self.test_command_hash.to_le_bytes());
        let hash = hasher.finish();
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalHistoryQuery {
    pub mutation_identity: String,
    pub mutation_description: String,
    pub source_hash: u64,
    pub command_hash: u64,
    pub relevant_test_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncrementalHistoryEntry {
    pub mutation_identity: String,
    pub mutation_description: String,
    pub result: MutationResult,
    pub source_hash: u64,
    pub command_hash: u64,
    pub relevant_test_hash: u64,
    #[serde(default)]
    pub covering_tests: Vec<String>,
    #[serde(default)]
    pub killer_test: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IncrementalHistoryFile {
    schema_version: u32,
    entries: Vec<IncrementalHistoryEntry>,
}

pub struct IncrementalHistoryStore {
    project_root: PathBuf,
    state: Mutex<IncrementalHistoryFile>,
}

impl IncrementalHistoryStore {
    pub fn load(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            state: Mutex::new(load_history_file(project_root)),
        }
    }

    pub fn lookup(&self, query: &IncrementalHistoryQuery) -> Option<MutationResult> {
        let history = self.state.lock().ok()?;
        history
            .entries
            .iter()
            .rev()
            .find(|entry| history_entry_matches(entry, query))
            .and_then(|entry| reusable_history_result(entry.result))
    }

    pub fn preferred_killer_test(
        &self,
        mutation_identity: &str,
        mutation_description: &str,
        tests: &[String],
    ) -> Option<String> {
        let history = self.state.lock().ok()?;
        history
            .entries
            .iter()
            .rev()
            .filter(|entry| {
                entry.mutation_identity == mutation_identity
                    && entry.mutation_description == mutation_description
            })
            .filter_map(|entry| entry.killer_test.as_ref())
            .find(|killer| tests.iter().any(|test| test == *killer))
            .cloned()
    }

    /// Killer test from the latest `Killed` history entry for this mutation
    /// whose source and command hashes still match the current run — the
    /// evidence learned selection clusters on. Returns `None` when there is
    /// no such entry, the verdict was not `Killed`, or no killer test was
    /// recorded.
    pub fn learned_killer_test(
        &self,
        mutation_identity: &str,
        mutation_description: &str,
        source_hash: u64,
        command_hash: u64,
    ) -> Option<String> {
        let history = self.state.lock().ok()?;
        history
            .entries
            .iter()
            .rev()
            .find(|entry| {
                entry.mutation_identity == mutation_identity
                    && entry.mutation_description == mutation_description
                    && entry.source_hash == source_hash
                    && entry.command_hash == command_hash
                    && entry.result == MutationResult::Killed
            })
            .and_then(|entry| entry.killer_test.clone())
    }

    pub fn record(&self, entry: IncrementalHistoryEntry) {
        let Ok(mut history) = self.state.lock() else {
            eprintln!("warning: incremental history mutex poisoned");
            return;
        };
        history.schema_version = HISTORY_SCHEMA_VERSION;
        if let Some(existing) = history.entries.iter_mut().find(|existing| {
            existing.mutation_identity == entry.mutation_identity
                && existing.mutation_description == entry.mutation_description
        }) {
            *existing = entry;
        } else {
            history.entries.push(entry);
        }
        save_history_file(&self.project_root, &history);
    }
}

fn history_entry_matches(entry: &IncrementalHistoryEntry, query: &IncrementalHistoryQuery) -> bool {
    entry.mutation_identity == query.mutation_identity
        && entry.mutation_description == query.mutation_description
        && entry.source_hash == query.source_hash
        && entry.command_hash == query.command_hash
        && entry.relevant_test_hash == query.relevant_test_hash
}

fn reusable_history_result(result: MutationResult) -> Option<MutationResult> {
    matches!(result, MutationResult::Killed | MutationResult::Survived).then_some(result)
}

fn load_history_file(project_root: &Path) -> IncrementalHistoryFile {
    let path = history_path(project_root);
    let Ok(data) = fs::read_to_string(path) else {
        return IncrementalHistoryFile {
            schema_version: HISTORY_SCHEMA_VERSION,
            entries: Vec::new(),
        };
    };
    let Ok(history) = serde_json::from_str::<IncrementalHistoryFile>(&data) else {
        return IncrementalHistoryFile {
            schema_version: HISTORY_SCHEMA_VERSION,
            entries: Vec::new(),
        };
    };
    if history.schema_version == HISTORY_SCHEMA_VERSION {
        history
    } else {
        IncrementalHistoryFile {
            schema_version: HISTORY_SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

fn save_history_file(project_root: &Path, history: &IncrementalHistoryFile) {
    let path = history_path(project_root);
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(data) = serde_json::to_vec(history) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, data).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

/// Return the cache directory path under the given project root.
fn cache_dir(project_root: &Path) -> PathBuf {
    project_root.join(CACHE_DIR)
}

fn history_path(project_root: &Path) -> PathBuf {
    cache_dir(project_root).join(HISTORY_FILE)
}

/// Return the file path for a given cache key.
fn entry_path(project_root: &Path, key: &CacheKey) -> PathBuf {
    cache_dir(project_root).join(key.digest())
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

struct Fnv64Hasher {
    hash: u64,
}

impl Default for Fnv64Hasher {
    fn default() -> Self {
        Self { hash: FNV_OFFSET }
    }
}

impl Hasher for Fnv64Hasher {
    fn finish(&self) -> u64 {
        self.hash
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.hash ^= u64::from(*byte);
            self.hash = self.hash.wrapping_mul(FNV_PRIME);
        }
    }
}

pub(crate) fn hash_bytes(data: &[u8]) -> u64 {
    let mut hasher = Fnv64Hasher::default();
    hasher.write(data);
    hasher.finish()
}

pub(crate) fn hash_str(s: &str) -> u64 {
    hash_bytes(s.as_bytes())
}

fn update_hash_str(hasher: &mut impl Hasher, value: &str) {
    update_hash_bytes(hasher, &(value.len() as u64).to_le_bytes());
    update_hash_bytes(hasher, value.as_bytes());
}

fn update_hash_bytes(hasher: &mut impl Hasher, bytes: &[u8]) {
    hasher.write(bytes);
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
    fn history_with_older_schema_version_loads_empty() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let entry = IncrementalHistoryEntry {
            mutation_identity: "src/lib.rs:0..1:op".into(),
            mutation_description: "desc".into(),
            result: MutationResult::Survived,
            source_hash: 1,
            command_hash: 2,
            relevant_test_hash: 3,
            covering_tests: vec![],
            killer_test: None,
        };
        let stale = IncrementalHistoryFile {
            schema_version: HISTORY_SCHEMA_VERSION - 1,
            entries: vec![entry],
        };
        fs::create_dir_all(cache_dir(tmp.path())).unwrap();
        fs::write(
            history_path(tmp.path()),
            serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();

        let store = IncrementalHistoryStore::load(tmp.path());
        let query = IncrementalHistoryQuery {
            mutation_identity: "src/lib.rs:0..1:op".into(),
            mutation_description: "desc".into(),
            source_hash: 1,
            command_hash: 2,
            relevant_test_hash: 3,
        };
        assert_eq!(store.lookup(&query), None);
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
            MutationResult::Uncovered,
            MutationResult::Subsumed,
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

    #[test]
    fn incremental_history_reuses_killed_result_when_inputs_match() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = IncrementalHistoryStore::load(tmp.path());
        let query = IncrementalHistoryQuery {
            mutation_identity: "src/lib.rs:0..1:op".into(),
            mutation_description: "desc".into(),
            source_hash: hash_bytes(b"source"),
            command_hash: hash_str("cargo test"),
            relevant_test_hash: hash_str("tests"),
        };
        store.record(IncrementalHistoryEntry {
            mutation_identity: query.mutation_identity.clone(),
            mutation_description: query.mutation_description.clone(),
            result: MutationResult::Killed,
            source_hash: query.source_hash,
            command_hash: query.command_hash,
            relevant_test_hash: query.relevant_test_hash,
            covering_tests: vec!["test_add".into()],
            killer_test: Some("test_add".into()),
        });

        let reloaded = IncrementalHistoryStore::load(tmp.path());

        assert_eq!(reloaded.lookup(&query), Some(MutationResult::Killed));
        Ok(())
    }

    #[test]
    fn incremental_history_rejects_changed_relevant_tests() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = IncrementalHistoryStore::load(tmp.path());
        store.record(IncrementalHistoryEntry {
            mutation_identity: "src/lib.rs:0..1:op".into(),
            mutation_description: "desc".into(),
            result: MutationResult::Survived,
            source_hash: 1,
            command_hash: 2,
            relevant_test_hash: 3,
            covering_tests: vec!["test_add".into()],
            killer_test: None,
        });
        let changed_tests = IncrementalHistoryQuery {
            mutation_identity: "src/lib.rs:0..1:op".into(),
            mutation_description: "desc".into(),
            source_hash: 1,
            command_hash: 2,
            relevant_test_hash: 4,
        };

        assert_eq!(store.lookup(&changed_tests), None);
        Ok(())
    }

    #[test]
    fn incremental_history_does_not_reuse_timeout_or_build_error() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = IncrementalHistoryStore::load(tmp.path());
        let query = IncrementalHistoryQuery {
            mutation_identity: "src/lib.rs:0..1:op".into(),
            mutation_description: "desc".into(),
            source_hash: 1,
            command_hash: 2,
            relevant_test_hash: 3,
        };
        for result in [MutationResult::Timeout, MutationResult::BuildError] {
            store.record(IncrementalHistoryEntry {
                mutation_identity: query.mutation_identity.clone(),
                mutation_description: query.mutation_description.clone(),
                result,
                source_hash: query.source_hash,
                command_hash: query.command_hash,
                relevant_test_hash: query.relevant_test_hash,
                covering_tests: vec![],
                killer_test: None,
            });

            assert_eq!(store.lookup(&query), None);
        }
        Ok(())
    }

    #[test]
    fn incremental_history_prefers_recorded_killer_test() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = IncrementalHistoryStore::load(tmp.path());
        store.record(IncrementalHistoryEntry {
            mutation_identity: "src/lib.rs:0..1:op".into(),
            mutation_description: "desc".into(),
            result: MutationResult::Killed,
            source_hash: 1,
            command_hash: 2,
            relevant_test_hash: 3,
            covering_tests: vec!["test_add".into(), "test_max".into()],
            killer_test: Some("test_max".into()),
        });

        assert_eq!(
            store.preferred_killer_test(
                "src/lib.rs:0..1:op",
                "desc",
                &["test_add".into(), "test_max".into()]
            ),
            Some("test_max".into())
        );
        Ok(())
    }

    fn killed_entry(killer_test: Option<&str>) -> IncrementalHistoryEntry {
        IncrementalHistoryEntry {
            mutation_identity: "src/lib.rs:0..1:op".into(),
            mutation_description: "desc".into(),
            result: MutationResult::Killed,
            source_hash: 1,
            command_hash: 2,
            relevant_test_hash: 3,
            covering_tests: vec!["test_add".into()],
            killer_test: killer_test.map(str::to_string),
        }
    }

    #[test]
    fn learned_killer_test_matches_killed_entry_with_current_hashes() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = IncrementalHistoryStore::load(tmp.path());
        store.record(killed_entry(Some("test_add")));

        let reloaded = IncrementalHistoryStore::load(tmp.path());
        assert_eq!(
            reloaded.learned_killer_test("src/lib.rs:0..1:op", "desc", 1, 2),
            Some("test_add".into())
        );
        Ok(())
    }

    #[test]
    fn learned_killer_test_rejects_mismatched_hashes() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = IncrementalHistoryStore::load(tmp.path());
        store.record(killed_entry(Some("test_add")));

        assert_eq!(
            store.learned_killer_test("src/lib.rs:0..1:op", "desc", 9, 2),
            None
        );
        assert_eq!(
            store.learned_killer_test("src/lib.rs:0..1:op", "desc", 1, 9),
            None
        );
        Ok(())
    }

    #[test]
    fn learned_killer_test_rejects_non_killed_verdict() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = IncrementalHistoryStore::load(tmp.path());
        let mut entry = killed_entry(Some("test_add"));
        entry.result = MutationResult::Survived;
        store.record(entry);

        assert_eq!(
            store.learned_killer_test("src/lib.rs:0..1:op", "desc", 1, 2),
            None
        );
        Ok(())
    }

    #[test]
    fn learned_killer_test_rejects_missing_killer() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = IncrementalHistoryStore::load(tmp.path());
        store.record(killed_entry(None));

        assert_eq!(
            store.learned_killer_test("src/lib.rs:0..1:op", "desc", 1, 2),
            None
        );
        Ok(())
    }
}
