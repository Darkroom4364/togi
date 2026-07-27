use crate::source_identity::{
    is_normalized_project_relative_path, normalized_project_relative_path, range_matches,
    source_fingerprint,
};
use crate::{Mutation, MutationReport, MutationResult};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::Path;

const BASELINE_FILE: &str = ".togi-baseline";
const MUTANT_SNAPSHOT_VERSION: u32 = 1;

/// Per-file mutation score snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileScore {
    pub killed: usize,
    pub total: usize,
}

/// Baseline snapshot of mutation scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub files: BTreeMap<String, FileScore>,
    pub killed: usize,
    pub total: usize,
    /// Optional to keep aggregate-only baselines backward compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutant_snapshot: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MutantSnapshotV1 {
    version: u32,
    mutants: Vec<BaselineMutant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BaselineMutant {
    path: String,
    source_fingerprint: String,
    byte_start: usize,
    byte_end: usize,
    language: String,
    operator: String,
    original: String,
    replacement: String,
    result: BaselineMutantResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BaselineMutantResult {
    Killed,
    Survived,
    Timeout,
}

impl BaselineMutantResult {
    fn from_result(result: MutationResult) -> Option<Self> {
        match result {
            MutationResult::Killed => Some(Self::Killed),
            MutationResult::Survived => Some(Self::Survived),
            MutationResult::Timeout => Some(Self::Timeout),
            MutationResult::BuildError | MutationResult::Uncovered | MutationResult::Subsumed => {
                None
            }
        }
    }
}

/// A survivor's relationship to the loaded per-mutant baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurvivorBaselineStatus {
    Historic,
    New,
    NonComparable,
}

impl SurvivorBaselineStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Historic => "historic",
            Self::New => "new",
            Self::NonComparable => "non_comparable",
        }
    }
}

/// External comparison data for report renderers, keyed by mutation id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurvivorBaselineComparison {
    statuses: BTreeMap<u32, SurvivorBaselineStatus>,
}

impl SurvivorBaselineComparison {
    pub fn status_for(&self, mutation_id: u32) -> Option<SurvivorBaselineStatus> {
        self.statuses.get(&mutation_id).copied()
    }

    pub(crate) fn from_statuses(
        statuses: BTreeMap<u32, SurvivorBaselineStatus>,
    ) -> SurvivorBaselineComparison {
        SurvivorBaselineComparison { statuses }
    }
}

/// Whether a report contains any newly executed mutation-test evidence.
pub fn has_fresh_execution(report: &crate::MutationReport) -> bool {
    report.tested_count() > 0
}

/// Whether a report is complete fresh evidence suitable for a baseline.
///
/// A partial or mixed report cannot be compared to or persisted as a baseline:
/// restored verdicts are deliberately excluded from its counters.
pub fn is_baseline_eligible(report: &crate::MutationReport) -> bool {
    has_fresh_execution(report)
        && report.reused_count() == 0
        && report.total == report.planned_total
}

/// Build a baseline from a mutation report.
///
/// Only mutants whose test suites ran during this invocation contribute to
/// per-file and overall totals. Restored cache/history verdicts, build errors,
/// coverage-suppressed (uncovered), and learned-selection (subsumed) mutants
/// are excluded.
pub fn from_report(report: &MutationReport, project_root: &Path) -> Baseline {
    let mut files = BTreeMap::new();
    for (mutation, result) in &report.results {
        let rel = baseline_file_path(mutation, project_root);
        let execution = report.execution_for(mutation.id, *result);
        if !execution.is_tested() {
            continue;
        }
        let entry = files.entry(rel).or_insert(FileScore {
            killed: 0,
            total: 0,
        });
        entry.total += 1;
        if *result == MutationResult::Killed {
            entry.killed += 1;
        }
    }
    let execution_counts = report.execution_counts();
    Baseline {
        files,
        killed: execution_counts.executed_killed,
        total: execution_counts.executed,
        mutant_snapshot: snapshot_from_report(report, project_root),
    }
}

fn snapshot_from_report(report: &MutationReport, project_root: &Path) -> Option<serde_json::Value> {
    let mut source_cache = SourceCache::new(project_root);
    let mut mutants = report
        .results
        .iter()
        .filter_map(|(mutation, result)| {
            if !report.execution_for(mutation.id, *result).is_tested() {
                return None;
            }
            let result = BaselineMutantResult::from_result(*result)?;
            let source = source_cache.source_identity(mutation)?;
            Some(BaselineMutant {
                path: source.path.clone(),
                source_fingerprint: source.fingerprint.clone(),
                byte_start: mutation.byte_range.start,
                byte_end: mutation.byte_range.end,
                language: mutation.language.clone(),
                operator: mutation.operator.clone(),
                original: mutation.original.clone(),
                replacement: mutation.replacement.clone(),
                result,
            })
        })
        .collect::<Vec<_>>();
    mutants.sort_by(compare_baseline_mutants);
    serde_json::to_value(MutantSnapshotV1 {
        version: MUTANT_SNAPSHOT_VERSION,
        mutants,
    })
    .ok()
}

fn compare_baseline_mutants(left: &BaselineMutant, right: &BaselineMutant) -> std::cmp::Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| left.source_fingerprint.cmp(&right.source_fingerprint))
        .then_with(|| left.byte_start.cmp(&right.byte_start))
        .then_with(|| left.byte_end.cmp(&right.byte_end))
        .then_with(|| left.language.cmp(&right.language))
        .then_with(|| left.operator.cmp(&right.operator))
        .then_with(|| left.original.cmp(&right.original))
        .then_with(|| left.replacement.cmp(&right.replacement))
        .then_with(|| left.result.cmp(&right.result))
}

/// Persist a baseline snapshot to `.togi-baseline` inside `dir`.
pub fn save_baseline(baseline: &Baseline, dir: &Path) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(baseline)?;
    std::fs::write(dir.join(BASELINE_FILE), json)?;
    Ok(())
}

/// Build and persist a baseline only when a report is baseline-eligible.
///
/// `Ok(None)` leaves any existing baseline untouched when the report is
/// partial, contains a reused verdict, or has no mutated test-suite execution.
pub fn save_baseline_from_report(
    report: &crate::MutationReport,
    project_root: &Path,
) -> anyhow::Result<Option<Baseline>> {
    if !is_baseline_eligible(report) {
        return Ok(None);
    }

    let baseline = from_report(report, project_root);
    save_baseline(&baseline, project_root)?;
    Ok(Some(baseline))
}

/// Load a previously saved baseline from `dir`, returning `Ok(None)` if the file doesn't exist.
pub fn load_baseline(dir: &Path) -> anyhow::Result<Option<Baseline>> {
    let path = dir.join(BASELINE_FILE);
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    let mut baseline: Baseline = serde_json::from_str(&data)
        .with_context(|| format!("invalid baseline at {}", path.display()))?;
    canonicalize_loaded_file_paths(&mut baseline.files)?;
    Ok(Some(baseline))
}

fn canonicalize_loaded_file_paths(files: &mut BTreeMap<String, FileScore>) -> anyhow::Result<()> {
    let mut canonical = BTreeMap::new();
    for (path, score) in std::mem::take(files) {
        let slash_path = path.replace('\\', "/");
        if canonical.insert(slash_path.clone(), score).is_some() {
            anyhow::bail!(
                "baseline contains colliding file paths after slash normalization: {slash_path}"
            );
        }
    }
    *files = canonical;
    Ok(())
}

struct SourceIdentity {
    path: String,
    fingerprint: String,
    source: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CurrentMutationIdentity {
    path: String,
    source_fingerprint: String,
    byte_start: usize,
    byte_end: usize,
    language: String,
    operator: String,
    original: String,
    replacement: String,
}

fn baseline_file_path(mutation: &Mutation, project_root: &Path) -> String {
    normalized_project_relative_path(project_root, &mutation.file).unwrap_or_else(|| {
        mutation
            .file
            .strip_prefix(project_root)
            .unwrap_or(&mutation.file)
            .display()
            .to_string()
    })
}

struct SourceCache<'a> {
    project_root: &'a Path,
    sources: BTreeMap<String, Option<SourceIdentity>>,
}

impl<'a> SourceCache<'a> {
    fn new(project_root: &'a Path) -> Self {
        Self {
            project_root,
            sources: BTreeMap::new(),
        }
    }

    fn source_identity(&mut self, mutation: &Mutation) -> Option<&SourceIdentity> {
        let path = normalized_project_relative_path(self.project_root, &mutation.file)?;
        let project_root = self.project_root;
        let source = self
            .sources
            .entry(path.clone())
            .or_insert_with(|| {
                let source = std::fs::read(project_root.join(&path)).ok()?;
                Some(SourceIdentity {
                    path,
                    fingerprint: source_fingerprint(&source),
                    source,
                })
            })
            .as_ref()?;
        range_matches(
            &source.source,
            mutation.byte_range.start,
            mutation.byte_range.end,
            &mutation.original,
        )
        .then_some(source)
    }
}

fn current_identity(mutation: &Mutation, source: &SourceIdentity) -> CurrentMutationIdentity {
    CurrentMutationIdentity {
        path: source.path.clone(),
        source_fingerprint: source.fingerprint.clone(),
        byte_start: mutation.byte_range.start,
        byte_end: mutation.byte_range.end,
        language: mutation.language.clone(),
        operator: mutation.operator.clone(),
        original: mutation.original.clone(),
        replacement: mutation.replacement.clone(),
    }
}

/// Compare freshly executed current survivors with a loaded per-mutant baseline.
///
/// Any missing, malformed, renamed, changed, incomplete, or ambiguous source
/// evidence is deliberately non-comparable rather than inferred from scores.
pub fn compare_survivors(
    report: &MutationReport,
    baseline: &Baseline,
    project_root: &Path,
) -> SurvivorBaselineComparison {
    let mut id_counts = BTreeMap::<u32, usize>::new();
    let mut identity_counts = BTreeMap::<CurrentMutationIdentity, usize>::new();
    let mut source_cache = SourceCache::new(project_root);
    for (mutation, _) in &report.results {
        *id_counts.entry(mutation.id).or_default() += 1;
        if let Some(source) = source_cache.source_identity(mutation) {
            *identity_counts
                .entry(current_identity(mutation, source))
                .or_default() += 1;
        }
    }

    let snapshot = snapshot_v1(baseline);
    let mut statuses = BTreeMap::new();
    for (mutation, result) in &report.results {
        if *result != MutationResult::Survived
            || !report.execution_for(mutation.id, *result).is_tested()
        {
            continue;
        }

        let source = source_cache.source_identity(mutation);
        let identity = source.map(|source| current_identity(mutation, source));
        let status = if id_counts.get(&mutation.id).is_some_and(|count| *count > 1)
            || identity
                .as_ref()
                .and_then(|identity| identity_counts.get(identity))
                .is_some_and(|count| *count > 1)
        {
            SurvivorBaselineStatus::NonComparable
        } else if let (Some(snapshot), Some(source)) = (&snapshot, source) {
            classify_survivor(mutation, source, baseline, snapshot)
        } else {
            SurvivorBaselineStatus::NonComparable
        };
        statuses.insert(mutation.id, status);
    }
    SurvivorBaselineComparison::from_statuses(statuses)
}

fn snapshot_v1(baseline: &Baseline) -> Option<MutantSnapshotV1> {
    let snapshot = baseline.mutant_snapshot.as_ref()?;
    if snapshot.get("version")?.as_u64()? != u64::from(MUTANT_SNAPSHOT_VERSION) {
        return None;
    }
    let snapshot = serde_json::from_value::<MutantSnapshotV1>(snapshot.clone()).ok()?;
    (snapshot.version == MUTANT_SNAPSHOT_VERSION).then_some(snapshot)
}

fn classify_survivor(
    mutation: &Mutation,
    source: &SourceIdentity,
    baseline: &Baseline,
    snapshot: &MutantSnapshotV1,
) -> SurvivorBaselineStatus {
    let Some(path_entries) = complete_path_snapshot(baseline, snapshot, source) else {
        return SurvivorBaselineStatus::NonComparable;
    };
    let mut matching = path_entries
        .into_iter()
        .filter(|entry| same_identity(entry, mutation, source));
    let Some(entry) = matching.next() else {
        return SurvivorBaselineStatus::New;
    };
    if matching.next().is_some() {
        return SurvivorBaselineStatus::NonComparable;
    }
    match entry.result {
        BaselineMutantResult::Survived => SurvivorBaselineStatus::Historic,
        BaselineMutantResult::Killed | BaselineMutantResult::Timeout => SurvivorBaselineStatus::New,
    }
}

fn complete_path_snapshot<'a>(
    baseline: &Baseline,
    snapshot: &'a MutantSnapshotV1,
    source: &SourceIdentity,
) -> Option<Vec<&'a BaselineMutant>> {
    let file_score = baseline.files.get(&source.path)?;
    if file_score.killed > file_score.total {
        return None;
    }
    let entries = snapshot
        .mutants
        .iter()
        .filter(|entry| entry.path == source.path)
        .collect::<Vec<_>>();
    if entries.is_empty()
        || entries.len() != file_score.total
        || entries
            .iter()
            .any(|entry| !baseline_entry_is_valid(entry, source))
        || has_duplicate_identity(&entries)
        || entries
            .iter()
            .filter(|entry| entry.result == BaselineMutantResult::Killed)
            .count()
            != file_score.killed
    {
        return None;
    }
    Some(entries)
}

fn baseline_entry_is_valid(entry: &BaselineMutant, source: &SourceIdentity) -> bool {
    entry.path == source.path
        && is_normalized_project_relative_path(&entry.path)
        && entry.source_fingerprint == source.fingerprint
        && range_matches(
            &source.source,
            entry.byte_start,
            entry.byte_end,
            &entry.original,
        )
}

fn has_duplicate_identity(entries: &[&BaselineMutant]) -> bool {
    let mut identities = BTreeSet::new();
    entries.iter().any(|entry| {
        !identities.insert((
            entry.path.as_str(),
            entry.source_fingerprint.as_str(),
            entry.byte_start,
            entry.byte_end,
            entry.language.as_str(),
            entry.operator.as_str(),
            entry.original.as_str(),
            entry.replacement.as_str(),
        ))
    })
}

fn same_identity(entry: &BaselineMutant, mutation: &Mutation, source: &SourceIdentity) -> bool {
    entry.path == source.path
        && entry.source_fingerprint == source.fingerprint
        && entry.byte_start == mutation.byte_range.start
        && entry.byte_end == mutation.byte_range.end
        && entry.language == mutation.language
        && entry.operator == mutation.operator
        && entry.original == mutation.original
        && entry.replacement == mutation.replacement
}

/// Returns `true` if the current overall score is a regression compared to the baseline.
///
/// A regression means the current kill ratio is strictly lower than the baseline kill ratio.
/// If either run has zero total mutations, no regression is reported.
pub fn check_regression(current: &Baseline, baseline: &Baseline) -> bool {
    if current.total == 0 || baseline.total == 0 {
        return false;
    }
    let current_ratio = current.killed as f64 / current.total as f64;
    let baseline_ratio = baseline.killed as f64 / baseline.total as f64;
    current_ratio < baseline_ratio
}

/// A per-file regression: file path and the score drop.
#[derive(Debug)]
pub struct FileRegression {
    pub file: String,
    pub baseline_pct: f64,
    pub current_pct: f64,
}

/// Find files where the mutation score dropped compared to the baseline.
/// Only reports files present in both the current and baseline runs.
pub fn per_file_regressions(current: &Baseline, baseline: &Baseline) -> Vec<FileRegression> {
    let mut regressions = Vec::new();
    for (file, base_score) in &baseline.files {
        if base_score.total == 0 {
            continue;
        }
        if let Some(cur_score) = current.files.get(file) {
            if cur_score.total == 0 {
                continue;
            }
            let base_pct = base_score.killed as f64 / base_score.total as f64 * 100.0;
            let cur_pct = cur_score.killed as f64 / cur_score.total as f64 * 100.0;
            if cur_pct < base_pct {
                regressions.push(FileRegression {
                    file: file.clone(),
                    baseline_pct: base_pct,
                    current_pct: cur_pct,
                });
            }
        }
    }
    regressions.sort_by(|a, b| {
        let a_drop = a.baseline_pct - a.current_pct;
        let b_drop = b.baseline_pct - b.current_pct;
        b_drop
            .partial_cmp(&a_drop)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    regressions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

    fn make_baseline(killed: usize, total: usize) -> Baseline {
        Baseline {
            files: BTreeMap::new(),
            killed,
            total,
            mutant_snapshot: None,
        }
    }

    const FIXTURE_SOURCE: &str = "a < b\nc == d\nx > y\n";

    fn write_fixture_source(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/sample.rs"), FIXTURE_SOURCE).unwrap();
    }

    fn fixture_mutation(
        id: u32,
        byte_range: std::ops::Range<usize>,
        original: &str,
        line: usize,
        description: &str,
    ) -> Mutation {
        Mutation {
            id,
            file: PathBuf::from("src/sample.rs"),
            language: "rust".to_string(),
            line,
            column: byte_range.start + 1,
            operator: "binary".to_string(),
            description: description.to_string(),
            original: original.to_string(),
            replacement: format!("{original}_replacement"),
            byte_range,
        }
    }

    fn fixture_report(results: Vec<(Mutation, MutationResult)>) -> MutationReport {
        let total = results.len();
        MutationReport {
            killed: results
                .iter()
                .filter(|(_, result)| *result == MutationResult::Killed)
                .count(),
            survived: results
                .iter()
                .filter(|(_, result)| *result == MutationResult::Survived)
                .count(),
            timeout: results
                .iter()
                .filter(|(_, result)| *result == MutationResult::Timeout)
                .count(),
            build_errors: results
                .iter()
                .filter(|(_, result)| *result == MutationResult::BuildError)
                .count(),
            results,
            execution_provenance: BTreeMap::new(),
            build_error_diagnostics: vec![],
            schemata: None,
            baseline_timing: None,
            duration: Duration::ZERO,
            test_command: None,
            build_command: vec![],
            planned_total: total,
            early_stop_reason: None,
            total,
        }
    }

    #[test]
    fn no_regression_when_score_improves() {
        let baseline = make_baseline(5, 10);
        let current = make_baseline(7, 10);
        assert!(!check_regression(&current, &baseline));
    }

    #[test]
    fn no_regression_when_score_same() {
        let baseline = make_baseline(5, 10);
        let current = make_baseline(5, 10);
        assert!(!check_regression(&current, &baseline));
    }

    #[test]
    fn regression_when_score_drops() {
        let baseline = make_baseline(5, 10);
        let current = make_baseline(3, 10);
        assert!(check_regression(&current, &baseline));
    }

    #[test]
    fn no_regression_when_baseline_empty() {
        let baseline = make_baseline(0, 0);
        let current = make_baseline(3, 10);
        assert!(!check_regression(&current, &baseline));
    }

    #[test]
    fn no_regression_when_current_empty() {
        let baseline = make_baseline(5, 10);
        let current = make_baseline(0, 0);
        assert!(!check_regression(&current, &baseline));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();

        let mut files = BTreeMap::new();
        files.insert(
            "src/main.rs".to_string(),
            FileScore {
                killed: 3,
                total: 5,
            },
        );
        let baseline = Baseline {
            files,
            killed: 3,
            total: 5,
            mutant_snapshot: None,
        };

        save_baseline(&baseline, dir.path()).unwrap();
        let loaded = load_baseline(dir.path()).unwrap().unwrap();

        assert_eq!(loaded.killed, 3);
        assert_eq!(loaded.total, 5);
        assert_eq!(loaded.files["src/main.rs"].killed, 3);
    }

    #[test]
    fn load_normalizes_legacy_backslash_file_keys_for_per_file_regressions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(BASELINE_FILE),
            r#"{"files":{"src\\foo.rs":{"killed":10,"total":10}},"killed":10,"total":10}"#,
        )
        .unwrap();

        let baseline = load_baseline(dir.path()).unwrap().unwrap();
        let current = make_baseline_with_files(vec![("src/foo.rs", 5, 10)]);
        let regressions = per_file_regressions(&current, &baseline);

        assert_eq!(baseline.killed, 10);
        assert_eq!(baseline.total, 10);
        assert_eq!(regressions.len(), 1);
        assert_eq!(regressions[0].file, "src/foo.rs");
    }

    #[test]
    fn load_rejects_colliding_backslash_file_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(BASELINE_FILE),
            r#"{"files":{"src/foo.rs":{"killed":1,"total":1},"src\\foo.rs":{"killed":0,"total":1}},"killed":1,"total":2}"#,
        )
        .unwrap();

        assert!(
            load_baseline(dir.path())
                .unwrap_err()
                .to_string()
                .contains("colliding file paths")
        );
    }

    #[test]
    fn v1_snapshot_roundtrips_with_deterministic_order() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let report = fixture_report(vec![
            (
                fixture_mutation(8, 15..16, ">", 3, "late mutation"),
                MutationResult::Timeout,
            ),
            (
                fixture_mutation(3, 2..3, "<", 1, "early mutation"),
                MutationResult::Killed,
            ),
        ]);
        let baseline = from_report(&report, dir.path());
        let snapshot = snapshot_v1(&baseline).unwrap();

        assert_eq!(snapshot.version, MUTANT_SNAPSHOT_VERSION);
        assert_eq!(snapshot.mutants.len(), 2);
        assert_eq!(snapshot.mutants[0].byte_start, 2);
        assert_eq!(snapshot.mutants[1].byte_start, 15);
        assert_eq!(
            snapshot.mutants[0].source_fingerprint,
            source_fingerprint(FIXTURE_SOURCE.as_bytes())
        );

        save_baseline(&baseline, dir.path()).unwrap();
        let first_save = std::fs::read(dir.path().join(BASELINE_FILE)).unwrap();
        save_baseline(&baseline, dir.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(BASELINE_FILE)).unwrap(),
            first_save
        );

        let saved: serde_json::Value = serde_json::from_slice(&first_save).unwrap();
        assert_eq!(saved["mutant_snapshot"]["version"], MUTANT_SNAPSHOT_VERSION);
        let loaded = load_baseline(dir.path()).unwrap().unwrap();
        let loaded_snapshot = snapshot_v1(&loaded).unwrap();
        assert_eq!(loaded_snapshot.mutants[0].byte_start, 2);
        assert_eq!(loaded_snapshot.mutants[1].byte_start, 15);
    }

    #[test]
    fn snapshot_omits_reused_source_present_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());

        for execution in [
            crate::MutationExecution::ExactCache,
            crate::MutationExecution::IncrementalHistory,
        ] {
            let mut report = fixture_report(vec![
                (
                    fixture_mutation(1, 2..3, "<", 1, "reused survivor"),
                    MutationResult::Survived,
                ),
                (
                    fixture_mutation(2, 8..10, "==", 2, "fresh killed"),
                    MutationResult::Killed,
                ),
            ]);
            report.execution_provenance.insert(1, execution);

            let baseline = from_report(&report, dir.path());
            let serialized = serde_json::to_value(&baseline).unwrap();
            let mutants = serialized["mutant_snapshot"]["mutants"].as_array().unwrap();
            assert_eq!(mutants.len(), 1);
            assert_eq!(mutants[0]["byte_start"], 8);
            assert_eq!(baseline.files["src/sample.rs"].total, 1);
        }
    }

    #[test]
    fn complete_snapshot_with_unknown_version_preserves_gates_but_is_non_comparable() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let baseline_report = fixture_report(vec![(
            fixture_mutation(1, 2..3, "<", 1, "baseline killed"),
            MutationResult::Killed,
        )]);
        let current_report = fixture_report(vec![(
            fixture_mutation(2, 2..3, "<", 1, "current survivor"),
            MutationResult::Survived,
        )]);
        let mut unknown = from_report(&baseline_report, dir.path());
        let mut snapshot = snapshot_v1(&unknown).unwrap();
        snapshot.version = 99;
        unknown.mutant_snapshot = Some(serde_json::to_value(snapshot).unwrap());
        let current = from_report(&current_report, dir.path());

        assert!(check_regression(&current, &unknown));
        assert_eq!(per_file_regressions(&current, &unknown).len(), 1);
        assert_eq!(
            compare_survivors(&current_report, &unknown, dir.path()).status_for(2),
            Some(SurvivorBaselineStatus::NonComparable)
        );
    }

    #[test]
    fn incomplete_or_conflicting_path_snapshot_is_non_comparable() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let baseline = from_report(
            &fixture_report(vec![
                (
                    fixture_mutation(1, 2..3, "<", 1, "historic"),
                    MutationResult::Survived,
                ),
                (
                    fixture_mutation(2, 8..10, "==", 2, "sibling"),
                    MutationResult::Killed,
                ),
            ]),
            dir.path(),
        );
        let current = fixture_report(vec![(
            fixture_mutation(9, 2..3, "<", 1, "current"),
            MutationResult::Survived,
        )]);
        assert_eq!(
            compare_survivors(&current, &baseline, dir.path()).status_for(9),
            Some(SurvivorBaselineStatus::Historic)
        );

        let mut truncated = baseline.clone();
        let mut truncated_snapshot = snapshot_v1(&truncated).unwrap();
        truncated_snapshot.mutants.pop();
        truncated.mutant_snapshot = Some(serde_json::to_value(truncated_snapshot).unwrap());
        assert_eq!(
            compare_survivors(&current, &truncated, dir.path()).status_for(9),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        let mut mixed_fingerprint = baseline.clone();
        let mut mixed_snapshot = snapshot_v1(&mixed_fingerprint).unwrap();
        mixed_snapshot.mutants[1].source_fingerprint = "sha256:other".to_string();
        mixed_fingerprint.mutant_snapshot = Some(serde_json::to_value(mixed_snapshot).unwrap());
        assert_eq!(
            compare_survivors(&current, &mixed_fingerprint, dir.path()).status_for(9),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        let mut missing_file = baseline.clone();
        missing_file.files.remove("src/sample.rs");
        assert_eq!(
            compare_survivors(&current, &missing_file, dir.path()).status_for(9),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        let mut mismatched_total = baseline;
        mismatched_total
            .files
            .get_mut("src/sample.rs")
            .unwrap()
            .total = 1;
        assert_eq!(
            compare_survivors(&current, &mismatched_total, dir.path()).status_for(9),
            Some(SurvivorBaselineStatus::NonComparable)
        );
    }

    #[test]
    fn legacy_snapshots_preserve_gates_but_are_non_comparable() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let report = fixture_report(vec![(
            fixture_mutation(9, 2..3, "<", 1, "current survivor"),
            MutationResult::Survived,
        )]);
        let current = from_report(&report, dir.path());

        std::fs::write(
            dir.path().join(BASELINE_FILE),
            r#"{
  "files": { "src/sample.rs": { "killed": 1, "total": 1 } },
  "killed": 1,
  "total": 1
}"#,
        )
        .unwrap();
        let legacy = load_baseline(dir.path()).unwrap().unwrap();
        assert!(legacy.mutant_snapshot.is_none());
        assert!(check_regression(&current, &legacy));
        assert_eq!(per_file_regressions(&current, &legacy).len(), 1);
        assert_eq!(
            compare_survivors(&report, &legacy, dir.path()).status_for(9),
            Some(SurvivorBaselineStatus::NonComparable)
        );
    }

    #[test]
    fn comparison_classifies_historic_and_new_without_mutable_metadata() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let baseline = from_report(
            &fixture_report(vec![
                (
                    fixture_mutation(1, 2..3, "<", 1, "baseline historic"),
                    MutationResult::Survived,
                ),
                (
                    fixture_mutation(2, 8..10, "==", 2, "baseline killed"),
                    MutationResult::Killed,
                ),
            ]),
            dir.path(),
        );
        let current = fixture_report(vec![
            (
                fixture_mutation(91, 2..3, "<", 99, "renumbered historic"),
                MutationResult::Survived,
            ),
            (
                fixture_mutation(92, 8..10, "==", 98, "renumbered killed"),
                MutationResult::Survived,
            ),
            (
                fixture_mutation(93, 15..16, ">", 97, "new identity"),
                MutationResult::Survived,
            ),
        ]);

        let comparison = compare_survivors(&current, &baseline, dir.path());
        assert_eq!(
            comparison.status_for(91),
            Some(SurvivorBaselineStatus::Historic)
        );
        assert_eq!(comparison.status_for(92), Some(SurvivorBaselineStatus::New));
        assert_eq!(comparison.status_for(93), Some(SurvivorBaselineStatus::New));
    }

    #[test]
    fn comparison_fails_closed_for_ambiguous_current_identity_or_id() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let baseline = from_report(
            &fixture_report(vec![(
                fixture_mutation(1, 2..3, "<", 1, "baseline"),
                MutationResult::Survived,
            )]),
            dir.path(),
        );

        let same_identity = fixture_report(vec![
            (
                fixture_mutation(10, 2..3, "<", 1, "first duplicate"),
                MutationResult::Survived,
            ),
            (
                fixture_mutation(11, 2..3, "<", 2, "second duplicate"),
                MutationResult::Survived,
            ),
        ]);
        let comparison = compare_survivors(&same_identity, &baseline, dir.path());
        assert_eq!(
            comparison.status_for(10),
            Some(SurvivorBaselineStatus::NonComparable)
        );
        assert_eq!(
            comparison.status_for(11),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        let killed_and_survived = fixture_report(vec![
            (
                fixture_mutation(20, 2..3, "<", 1, "killed duplicate"),
                MutationResult::Killed,
            ),
            (
                fixture_mutation(21, 2..3, "<", 2, "survived duplicate"),
                MutationResult::Survived,
            ),
        ]);
        assert_eq!(
            compare_survivors(&killed_and_survived, &baseline, dir.path()).status_for(21),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        let duplicate_id = fixture_report(vec![
            (
                fixture_mutation(30, 2..3, "<", 1, "survivor"),
                MutationResult::Survived,
            ),
            (
                fixture_mutation(30, 8..10, "==", 2, "different identity"),
                MutationResult::Killed,
            ),
        ]);
        assert_eq!(
            compare_survivors(&duplicate_id, &baseline, dir.path()).status_for(30),
            Some(SurvivorBaselineStatus::NonComparable)
        );
    }

    #[test]
    fn changed_identity_and_timeout_to_survived_are_new() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let mut baseline_language = fixture_mutation(4, 2..3, "<", 4, "language");
        baseline_language.language = "go".to_string();
        let baseline = from_report(
            &fixture_report(vec![
                (
                    fixture_mutation(1, 2..3, "<", 1, "timed out"),
                    MutationResult::Timeout,
                ),
                (
                    fixture_mutation(2, 8..10, "==", 2, "operator"),
                    MutationResult::Killed,
                ),
                (
                    fixture_mutation(3, 15..16, ">", 3, "replacement"),
                    MutationResult::Survived,
                ),
                (baseline_language, MutationResult::Killed),
            ]),
            dir.path(),
        );
        let mut changed_operator = fixture_mutation(11, 8..10, "==", 2, "operator changed");
        changed_operator.operator = "other_binary".to_string();
        let mut changed_replacement = fixture_mutation(12, 15..16, ">", 3, "replacement changed");
        changed_replacement.replacement = "different_replacement".to_string();
        let mut changed_language = fixture_mutation(13, 2..3, "<", 4, "language changed");
        changed_language.language = "typescript".to_string();
        let current = fixture_report(vec![
            (
                fixture_mutation(10, 2..3, "<", 1, "timeout now survives"),
                MutationResult::Survived,
            ),
            (changed_operator, MutationResult::Survived),
            (changed_replacement, MutationResult::Survived),
            (changed_language, MutationResult::Survived),
        ]);

        let comparison = compare_survivors(&current, &baseline, dir.path());
        for id in [10, 11, 12, 13] {
            assert_eq!(comparison.status_for(id), Some(SurvivorBaselineStatus::New));
        }
    }

    #[test]
    fn comparison_fails_closed_for_changed_renamed_or_missing_sources() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let mutation = fixture_mutation(5, 2..3, "<", 1, "survivor");
        let report = fixture_report(vec![(mutation.clone(), MutationResult::Survived)]);
        let baseline = from_report(&report, dir.path());

        std::fs::write(dir.path().join("src/sample.rs"), "a < c\nc == d\nx > y\n").unwrap();
        assert_eq!(
            compare_survivors(&report, &baseline, dir.path()).status_for(5),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        write_fixture_source(dir.path());
        std::fs::rename(
            dir.path().join("src/sample.rs"),
            dir.path().join("src/renamed.rs"),
        )
        .unwrap();
        let mut renamed = mutation.clone();
        renamed.file = PathBuf::from("src/renamed.rs");
        let renamed_report = fixture_report(vec![(renamed.clone(), MutationResult::Survived)]);
        assert_eq!(
            compare_survivors(&renamed_report, &baseline, dir.path()).status_for(5),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        std::fs::remove_file(dir.path().join("src/renamed.rs")).unwrap();
        assert_eq!(
            compare_survivors(&renamed_report, &baseline, dir.path()).status_for(5),
            Some(SurvivorBaselineStatus::NonComparable)
        );
    }

    #[test]
    fn comparison_fails_closed_for_invalid_or_duplicate_snapshot_data() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let mutation = fixture_mutation(5, 2..3, "<", 1, "survivor");
        let report = fixture_report(vec![(mutation.clone(), MutationResult::Survived)]);
        let baseline = from_report(&report, dir.path());

        let mut invalid_range = baseline.clone();
        let mut invalid_snapshot = snapshot_v1(&invalid_range).unwrap();
        invalid_snapshot.mutants[0].byte_end += 1;
        invalid_range.mutant_snapshot = Some(serde_json::to_value(invalid_snapshot).unwrap());
        assert_eq!(
            compare_survivors(&report, &invalid_range, dir.path()).status_for(5),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        let mut duplicate = baseline.clone();
        let mut duplicate_snapshot = snapshot_v1(&duplicate).unwrap();
        duplicate_snapshot
            .mutants
            .push(duplicate_snapshot.mutants[0].clone());
        duplicate.mutant_snapshot = Some(serde_json::to_value(duplicate_snapshot).unwrap());
        assert_eq!(
            compare_survivors(&report, &duplicate, dir.path()).status_for(5),
            Some(SurvivorBaselineStatus::NonComparable)
        );

        let invalid_mutation = fixture_mutation(6, 2..4, "<", 1, "invalid range");
        let invalid_report = fixture_report(vec![(invalid_mutation, MutationResult::Survived)]);
        assert!(
            snapshot_v1(&from_report(&invalid_report, dir.path()))
                .unwrap()
                .mutants
                .is_empty()
        );
        assert_eq!(
            compare_survivors(&invalid_report, &baseline, dir.path()).status_for(6),
            Some(SurvivorBaselineStatus::NonComparable)
        );
    }

    #[test]
    fn duplicate_sibling_snapshot_entry_poisons_current_survivor() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let baseline = from_report(
            &fixture_report(vec![
                (
                    fixture_mutation(1, 2..3, "<", 1, "current A"),
                    MutationResult::Survived,
                ),
                (
                    fixture_mutation(2, 8..10, "==", 2, "sibling B"),
                    MutationResult::Killed,
                ),
            ]),
            dir.path(),
        );
        let current = fixture_report(vec![(
            fixture_mutation(9, 2..3, "<", 1, "current A"),
            MutationResult::Survived,
        )]);

        let mut duplicate_sibling = baseline.clone();
        let mut snapshot = snapshot_v1(&duplicate_sibling).unwrap();
        snapshot.mutants.push(snapshot.mutants[1].clone());
        duplicate_sibling.killed = 2;
        duplicate_sibling.total = 3;
        duplicate_sibling
            .files
            .get_mut("src/sample.rs")
            .unwrap()
            .killed = 2;
        duplicate_sibling
            .files
            .get_mut("src/sample.rs")
            .unwrap()
            .total = 3;
        duplicate_sibling.mutant_snapshot = Some(serde_json::to_value(snapshot).unwrap());
        assert_eq!(
            compare_survivors(&current, &duplicate_sibling, dir.path()).status_for(9),
            Some(SurvivorBaselineStatus::NonComparable)
        );
    }

    #[test]
    fn snapshot_preserves_aggregate_and_per_file_gates() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_source(dir.path());
        let baseline = from_report(
            &fixture_report(vec![
                (
                    fixture_mutation(1, 2..3, "<", 1, "killed"),
                    MutationResult::Killed,
                ),
                (
                    fixture_mutation(2, 8..10, "==", 2, "survived"),
                    MutationResult::Survived,
                ),
            ]),
            dir.path(),
        );
        let current = make_baseline_with_files(vec![("src/sample.rs", 0, 2)]);

        assert_eq!(baseline.killed, 1);
        assert_eq!(baseline.total, 2);
        assert_eq!(baseline.files["src/sample.rs"].killed, 1);
        assert!(baseline.mutant_snapshot.is_some());
        assert!(check_regression(&current, &baseline));
        assert_eq!(per_file_regressions(&current, &baseline).len(), 1);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_baseline(dir.path()).unwrap().is_none());
    }

    #[test]
    fn load_returns_error_when_baseline_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(BASELINE_FILE), "not json").unwrap();

        assert!(load_baseline(dir.path()).is_err());
    }

    #[test]
    fn from_report_excludes_exact_cache_verdicts() {
        let mut report = crate::test_helpers::sample_report();
        report
            .execution_provenance
            .insert(0, crate::MutationExecution::ExactCache);

        let baseline = from_report(&report, Path::new("."));

        assert_eq!(baseline.total, 1);
        assert_eq!(baseline.killed, 0);
        assert!(baseline.files.values().all(|score| score.total > 0));
    }

    #[test]
    fn from_report_excludes_incremental_history_alongside_exact_cache() {
        let mut report = crate::test_helpers::sample_report();
        report
            .execution_provenance
            .insert(0, crate::MutationExecution::ExactCache);
        report
            .execution_provenance
            .insert(1, crate::MutationExecution::IncrementalHistory);

        let baseline = from_report(&report, Path::new("."));

        assert_eq!(baseline.total, 0);
        assert_eq!(baseline.killed, 0);
        assert!(baseline.files.is_empty());
    }

    #[test]
    fn baseline_requires_complete_fresh_execution() {
        let fresh = crate::test_helpers::sample_report();
        assert!(has_fresh_execution(&fresh));
        assert!(is_baseline_eligible(&fresh));

        let mut partial = crate::test_helpers::sample_report();
        partial.planned_total += 1;
        assert!(has_fresh_execution(&partial));
        assert!(!is_baseline_eligible(&partial));

        let mut mixed = crate::test_helpers::sample_report();
        mixed
            .execution_provenance
            .insert(0, crate::MutationExecution::ExactCache);
        assert!(has_fresh_execution(&mixed));
        assert!(!is_baseline_eligible(&mixed));

        let mut reused = crate::test_helpers::sample_report();
        reused
            .execution_provenance
            .insert(0, crate::MutationExecution::ExactCache);
        reused
            .execution_provenance
            .insert(1, crate::MutationExecution::IncrementalHistory);
        assert!(!has_fresh_execution(&reused));
        assert!(!is_baseline_eligible(&reused));

        for result in [
            MutationResult::BuildError,
            MutationResult::Uncovered,
            MutationResult::Subsumed,
        ] {
            let mut non_executed = crate::test_helpers::sample_report();
            for (_, actual) in &mut non_executed.results {
                *actual = result;
            }
            assert!(!has_fresh_execution(&non_executed));
            assert!(!is_baseline_eligible(&non_executed));
        }
    }

    #[test]
    fn saving_mixed_report_retains_existing_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = crate::test_helpers::sample_report();
        assert!(
            save_baseline_from_report(&fresh, dir.path())
                .unwrap()
                .is_some()
        );
        let before = std::fs::read(dir.path().join(BASELINE_FILE)).unwrap();

        for execution in [
            crate::MutationExecution::ExactCache,
            crate::MutationExecution::IncrementalHistory,
        ] {
            let mut mixed = crate::test_helpers::sample_report();
            mixed.execution_provenance.insert(0, execution);

            assert!(has_fresh_execution(&mixed));
            assert!(!is_baseline_eligible(&mixed));
            assert!(
                save_baseline_from_report(&mixed, dir.path())
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                std::fs::read(dir.path().join(BASELINE_FILE)).unwrap(),
                before
            );
        }
    }

    fn make_baseline_with_files(files: Vec<(&str, usize, usize)>) -> Baseline {
        let mut file_map = BTreeMap::new();
        let mut total_killed = 0;
        let mut total_total = 0;
        for (path, killed, total) in files {
            total_killed += killed;
            total_total += total;
            file_map.insert(path.to_string(), FileScore { killed, total });
        }
        Baseline {
            files: file_map,
            killed: total_killed,
            total: total_total,
            mutant_snapshot: None,
        }
    }

    #[test]
    fn per_file_regression_detected() {
        let baseline = make_baseline_with_files(vec![("src/a.rs", 8, 10), ("src/b.rs", 5, 10)]);
        let current = make_baseline_with_files(vec![("src/a.rs", 6, 10), ("src/b.rs", 5, 10)]);
        let regs = per_file_regressions(&current, &baseline);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].file, "src/a.rs");
    }

    #[test]
    fn per_file_no_regression_when_improved() {
        let baseline = make_baseline_with_files(vec![("src/a.rs", 5, 10)]);
        let current = make_baseline_with_files(vec![("src/a.rs", 8, 10)]);
        assert!(per_file_regressions(&current, &baseline).is_empty());
    }

    #[test]
    fn per_file_ignores_new_files() {
        let baseline = make_baseline_with_files(vec![("src/a.rs", 5, 10)]);
        let current = make_baseline_with_files(vec![("src/a.rs", 5, 10), ("src/new.rs", 0, 5)]);
        assert!(per_file_regressions(&current, &baseline).is_empty());
    }

    #[test]
    fn per_file_sorted_by_largest_drop() {
        let baseline = make_baseline_with_files(vec![("src/a.rs", 10, 10), ("src/b.rs", 10, 10)]);
        let current = make_baseline_with_files(vec![
            ("src/a.rs", 5, 10), // 50% drop
            ("src/b.rs", 8, 10), // 20% drop
        ]);
        let regs = per_file_regressions(&current, &baseline);
        assert_eq!(regs.len(), 2);
        assert_eq!(regs[0].file, "src/a.rs"); // largest drop first
    }
}
