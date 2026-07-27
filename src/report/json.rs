use crate::runner::{RunSuiteFailure, RunSuiteFailureOutcome, SuiteFailurePhase};
use crate::{Mutation, MutationReport, MutationResult};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
struct JsonReport {
    total: usize,
    planned_total: usize,
    tested: usize,
    killed: usize,
    survived: usize,
    timeout: usize,
    build_errors: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    executed_killed: Option<usize>,
    #[serde(skip_serializing_if = "super::is_zero")]
    exact_cache_reused: usize,
    #[serde(skip_serializing_if = "super::is_zero")]
    incremental_history_reused: usize,
    #[serde(skip_serializing_if = "super::is_zero")]
    uncovered: usize,
    #[serde(skip_serializing_if = "super::is_zero")]
    subsumed: usize,
    partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    early_stop_reason: Option<String>,
    mutation_score: f64,
    duration_ms: u128,
    test_command: Option<Vec<String>>,
    build_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_timing: Option<JsonBaselineTiming>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schemata: Option<JsonSchemata>,
    build_error_groups: Vec<JsonBuildErrorGroup>,
    mutations: Vec<JsonMutation>,
}

#[derive(Serialize)]
struct JsonBaselineTiming {
    build_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build_duration_ms: Option<u128>,
    test_command: Vec<String>,
    test_duration_ms: u128,
    calibrated_timeout_ms: u128,
}

#[derive(Serialize)]
struct JsonSchemata {
    fast_path: usize,
    fallback: usize,
    fallback_reasons: Vec<JsonSchemataFallbackReason>,
}

#[derive(Serialize)]
struct JsonSchemataFallbackReason {
    reason: String,
    count: usize,
}

#[derive(Serialize)]
struct JsonBuildErrorGroup {
    count: usize,
    language: String,
    operator: String,
    runner: String,
    phase: String,
    fingerprint: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,
    message: String,
    files: Vec<JsonBuildErrorFileCount>,
    examples: Vec<JsonBuildErrorExample>,
}

#[derive(Serialize)]
struct JsonBuildErrorFileCount {
    file: String,
    count: usize,
}

#[derive(Serialize)]
struct JsonBuildErrorExample {
    mutation_id: u32,
    file: String,
    line: usize,
}

#[derive(Serialize)]
struct JsonMutation {
    id: u32,
    file: String,
    line: usize,
    operator: String,
    description: String,
    result: String,
    execution: JsonMutationExecution,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build_error_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_status: Option<&'static str>,
}

#[derive(Serialize)]
struct JsonMutationExecution {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

#[derive(Serialize)]
struct JsonRunSuiteFailure {
    kind: &'static str,
    phase: &'static str,
    command: Vec<String>,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct JsonDryRun {
    kind: &'static str,
    dry_run: bool,
    planned_total: usize,
    mutations: Vec<JsonDryRunMutation>,
}

#[derive(Serialize)]
struct JsonDryRunMutation {
    id: u32,
    file: String,
    line: usize,
    column: usize,
    operator: String,
    description: String,
    original: String,
    replacement: String,
}

pub fn print_report(report: &MutationReport) -> Result<()> {
    print_report_with_baseline(report, None)
}

pub fn print_report_with_baseline(
    report: &MutationReport,
    comparison: Option<&crate::baseline::SurvivorBaselineComparison>,
) -> Result<()> {
    println!("{}", to_json_string_with_baseline(report, comparison)?);
    Ok(())
}

/// Print a machine-readable mutation preview without executing test suites.
pub fn print_dry_run(mutations: &[Mutation]) -> Result<()> {
    println!("{}", to_dry_run_json(mutations)?);
    Ok(())
}

/// Serialize a dry-run preview as a document distinct from a mutation report.
pub fn to_dry_run_json(mutations: &[Mutation]) -> Result<String> {
    let mutations: Vec<JsonDryRunMutation> = mutations
        .iter()
        .map(|mutation| JsonDryRunMutation {
            id: mutation.id + 1,
            file: mutation.file.display().to_string(),
            line: mutation.line,
            column: mutation.column,
            operator: mutation.operator.clone(),
            description: mutation.description.clone(),
            original: mutation.original.clone(),
            replacement: mutation.replacement.clone(),
        })
        .collect();
    serde_json::to_string_pretty(&JsonDryRun {
        kind: "dry_run",
        dry_run: true,
        planned_total: mutations.len(),
        mutations,
    })
    .map_err(Into::into)
}

/// Print a run-level suite failure instead of a mutation report.
pub fn print_run_suite_failure(failure: &RunSuiteFailure) -> Result<()> {
    println!("{}", to_run_suite_failure_json(failure)?);
    Ok(())
}

/// Serialize a baseline suite failure without inventing mutation verdicts.
pub fn to_run_suite_failure_json(failure: &RunSuiteFailure) -> Result<String> {
    let (outcome, output, timeout_ms, detail) = match &failure.outcome {
        RunSuiteFailureOutcome::Failed { output } => ("failed", output.clone(), None, None),
        RunSuiteFailureOutcome::TimedOut { timeout } => {
            ("timed_out", None, Some(timeout.as_millis()), None)
        }
        RunSuiteFailureOutcome::CannotRun { detail } => {
            ("cannot_run", None, None, Some(detail.clone()))
        }
    };
    let phase = match failure.phase {
        SuiteFailurePhase::Build => "build",
        SuiteFailurePhase::Test => "test",
    };
    serde_json::to_string_pretty(&JsonRunSuiteFailure {
        kind: "suite_failure",
        phase,
        command: failure.command.clone(),
        outcome,
        output,
        timeout_ms,
        detail,
    })
    .map_err(Into::into)
}

/// Serialize report to a JSON string (for testing and programmatic use).
pub fn to_json_string(report: &MutationReport) -> Result<String> {
    to_json_string_with_baseline(report, None)
}

/// Serialize report with optional per-survivor baseline comparison data.
pub fn to_json_string_with_baseline(
    report: &MutationReport,
    comparison: Option<&crate::baseline::SurvivorBaselineComparison>,
) -> Result<String> {
    let diagnostic_by_id: BTreeMap<_, _> = report
        .build_error_diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.mutation_id, diagnostic))
        .collect();
    let mutations: Vec<JsonMutation> = report
        .results
        .iter()
        .map(|(m, r)| {
            let execution = report.execution_for(m.id, *r);
            JsonMutation {
                id: m.id + 1,
                file: m.file.display().to_string(),
                line: m.line,
                operator: m.operator.clone(),
                description: m.description.clone(),
                result: match r {
                    MutationResult::Killed => "killed".to_string(),
                    MutationResult::Survived => "survived".to_string(),
                    MutationResult::Timeout => "timeout".to_string(),
                    MutationResult::BuildError => "build_error".to_string(),
                    MutationResult::Uncovered => "uncovered".to_string(),
                    MutationResult::Subsumed => "subsumed".to_string(),
                },
                execution: JsonMutationExecution {
                    state: execution.state_name(),
                    reason: execution.reason().map(|reason| reason.name()),
                },
                column: Some(m.column),
                original: Some(m.original.clone()),
                replacement: Some(m.replacement.clone()),
                diff: super::mutation_diff(m),
                build_error_fingerprint: if *r == MutationResult::BuildError {
                    diagnostic_by_id
                        .get(&m.id)
                        .map(|diagnostic| diagnostic.fingerprint.clone())
                } else {
                    None
                },
                baseline_status: (*r == MutationResult::Survived)
                    .then(|| comparison.and_then(|comparison| comparison.status_for(m.id)))
                    .flatten()
                    .map(|status| status.as_str()),
            }
        })
        .collect();
    let build_error_groups = super::build_error_groups(report)
        .into_iter()
        .map(|group| JsonBuildErrorGroup {
            count: group.count,
            language: group.language,
            operator: group.operator,
            runner: group.runner,
            phase: group.phase,
            fingerprint: group.fingerprint,
            command: group.command,
            message: group.message,
            files: group
                .files
                .into_iter()
                .map(|file| JsonBuildErrorFileCount {
                    file: file.file,
                    count: file.count,
                })
                .collect(),
            examples: group
                .examples
                .into_iter()
                .map(|example| JsonBuildErrorExample {
                    mutation_id: example.mutation_id,
                    file: example.file,
                    line: example.line,
                })
                .collect(),
        })
        .collect();

    let execution_counts = report.execution_counts();
    let json_report = JsonReport {
        total: report.total,
        planned_total: report.planned_total,
        tested: execution_counts.executed,
        killed: report.killed,
        survived: report.survived,
        timeout: report.timeout,
        build_errors: report.build_errors,
        executed_killed: (execution_counts.executed_killed != report.killed)
            .then_some(execution_counts.executed_killed),
        exact_cache_reused: execution_counts.exact_cache_reused,
        incremental_history_reused: execution_counts.incremental_history_reused,
        uncovered: report.uncovered_count(),
        subsumed: report.subsumed_count(),
        partial: report.total < report.planned_total,
        early_stop_reason: report.early_stop_reason.clone(),
        mutation_score: super::mutation_score(report),
        duration_ms: report.duration.as_millis(),
        test_command: report.test_command.clone(),
        build_command: report.build_command.clone(),
        baseline_timing: report
            .baseline_timing
            .as_ref()
            .map(|timing| JsonBaselineTiming {
                build_command: timing.build_command.clone(),
                build_duration_ms: timing.build_duration.map(|duration| duration.as_millis()),
                test_command: timing.test_command.clone(),
                test_duration_ms: timing.test_duration.as_millis(),
                calibrated_timeout_ms: timing.calibrated_timeout.as_millis(),
            }),
        schemata: report.schemata.as_ref().map(|schemata| JsonSchemata {
            fast_path: schemata.fast_path,
            fallback: schemata.fallback,
            fallback_reasons: schemata
                .fallback_reasons
                .iter()
                .map(|reason| JsonSchemataFallbackReason {
                    reason: reason.reason.clone(),
                    count: reason.count,
                })
                .collect(),
        }),
        build_error_groups,
        mutations,
    };

    Ok(serde_json::to_string_pretty(&json_report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{RunSuiteFailure, RunSuiteFailureOutcome, SuiteFailurePhase};
    use crate::test_helpers::sample_report;
    use crate::{
        BaselineTiming, BuildErrorDiagnostic, Mutation, SchemataFallbackReasonCount, SchemataReport,
    };
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

    fn assert_object_keys(value: &Value, expected: &[&str]) {
        let object = value.as_object().expect("value should be a JSON object");
        let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
        let mut expected = expected.to_vec();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn json_output_annotates_only_survivors_when_comparison_is_active() {
        let report = sample_report();
        let comparison =
            crate::baseline::SurvivorBaselineComparison::from_statuses(BTreeMap::from([
                (0, crate::baseline::SurvivorBaselineStatus::New),
                (1, crate::baseline::SurvivorBaselineStatus::Historic),
            ]));

        let annotated: Value = serde_json::from_str(
            &to_json_string_with_baseline(&report, Some(&comparison)).unwrap(),
        )
        .unwrap();
        assert_eq!(annotated["mutations"][1]["baseline_status"], "historic");
        assert!(annotated["mutations"][0].get("baseline_status").is_none());

        let unannotated: Value = serde_json::from_str(&to_json_string(&report).unwrap()).unwrap();
        assert!(unannotated["mutations"][1].get("baseline_status").is_none());
    }

    #[test]
    fn json_output_lists_uncovered_mutants() {
        let mut report = sample_report();
        report.results.push((
            Mutation {
                id: 2,
                file: PathBuf::from("src/dead.rs"),
                language: String::new(),
                line: 3,
                column: 1,
                operator: "op".to_string(),
                description: "d".to_string(),
                original: "x".to_string(),
                replacement: "y".to_string(),
                byte_range: 0..1,
            },
            MutationResult::Uncovered,
        ));
        report.total = 3;
        report.planned_total = 3;

        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(value["uncovered"], 1);
        // Uncovered mutants are excluded from the tested denominator:
        // 1 killed of 2 tested (killed + survived) → 50%.
        assert_eq!(value["tested"], 2);
        assert_eq!(value["mutation_score"], 50.0);
        let mutations = value["mutations"].as_array().unwrap();
        assert_eq!(mutations[2]["result"], "uncovered");
    }

    #[test]
    fn json_output_omits_uncovered_key_when_zero() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(value.get("uncovered").is_none());
    }

    #[test]
    fn json_output_lists_subsumed_mutants() {
        let mut report = sample_report();
        report.results.push((
            Mutation {
                id: 2,
                file: PathBuf::from("src/dup.rs"),
                language: String::new(),
                line: 4,
                column: 1,
                operator: "op".to_string(),
                description: "d".to_string(),
                original: "x".to_string(),
                replacement: "y".to_string(),
                byte_range: 0..1,
            },
            MutationResult::Subsumed,
        ));
        report.total = 3;
        report.planned_total = 3;

        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(value["subsumed"], 1);
        // Subsumed mutants are excluded from the tested denominator:
        // 1 killed of 2 tested (killed + survived) → 50%.
        assert_eq!(value["tested"], 2);
        assert_eq!(value["mutation_score"], 50.0);
        let mutations = value["mutations"].as_array().unwrap();
        assert_eq!(mutations[2]["result"], "subsumed");
    }

    #[test]
    fn json_output_omits_subsumed_key_when_zero() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(value.get("subsumed").is_none());
    }

    #[test]
    fn json_output_is_valid_json() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: Value = serde_json::from_str(&json_str).expect("output should be valid JSON");
        assert!(value.is_object());
    }

    #[test]
    fn json_output_has_correct_structure() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(value["total"], 2);
        assert_eq!(value["planned_total"], 2);
        assert_eq!(value["partial"], false);
        assert_eq!(value["killed"], 1);
        assert_eq!(value["survived"], 1);
        assert_eq!(value["timeout"], 0);
        assert_eq!(value["build_errors"], 0);
        assert_eq!(value["mutation_score"], 50.0);
        assert_eq!(value["duration_ms"], 1234);
        assert_eq!(value["test_command"], serde_json::json!(["cargo", "test"]));
        assert_eq!(value["build_command"], serde_json::json!([]));
        assert_eq!(value["build_error_groups"], serde_json::json!([]));

        let mutations = value["mutations"].as_array().unwrap();
        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0]["id"], 1);
        assert_eq!(mutations[0]["file"], "src/auth.rs");
        assert_eq!(mutations[0]["line"], 47);
        assert_eq!(mutations[0]["operator"], "lt_to_lte");
        assert_eq!(mutations[0]["result"], "killed");
        assert_eq!(mutations[1]["result"], "survived");
        assert_eq!(mutations[0]["execution"]["state"], "executed");
        assert_eq!(mutations[1]["execution"]["state"], "executed");
    }

    #[test]
    fn json_output_reports_reuse_and_nonexecution_reasons() {
        let mut report = sample_report();
        report
            .execution_provenance
            .insert(0, crate::MutationExecution::ExactCache);
        report
            .execution_provenance
            .insert(1, crate::MutationExecution::IncrementalHistory);
        let mut uncovered = report.results[0].0.clone();
        uncovered.id = 2;
        report.results.push((uncovered, MutationResult::Uncovered));
        let mut subsumed = report.results[1].0.clone();
        subsumed.id = 3;
        report.results.push((subsumed, MutationResult::Subsumed));
        report.total = 4;
        report.planned_total = 4;

        let value: Value = serde_json::from_str(&to_json_string(&report).unwrap()).unwrap();
        let mutations = value["mutations"].as_array().unwrap();

        assert_eq!(value["tested"], 0);
        assert_eq!(value["executed_killed"], 0);
        assert_eq!(value["exact_cache_reused"], 1);
        assert_eq!(value["incremental_history_reused"], 1);
        assert_eq!(value["mutation_score"], 0.0);
        assert_eq!(
            mutations[0]["execution"],
            serde_json::json!({"state": "exact_cache"})
        );
        assert_eq!(
            mutations[1]["execution"],
            serde_json::json!({"state": "incremental_history"})
        );
        assert_eq!(
            mutations[2]["execution"],
            serde_json::json!({"state": "not_executed", "reason": "uncovered"})
        );
        assert_eq!(
            mutations[3]["execution"],
            serde_json::json!({"state": "not_executed", "reason": "subsumed"})
        );
    }

    #[test]
    fn json_output_schema_is_stable() {
        let report = sample_report();
        let json_str = to_json_string(&report).expect("sample report should serialize");
        let value: Value = serde_json::from_str(&json_str).expect("sample report JSON is valid");

        assert_object_keys(
            &value,
            &[
                "total",
                "planned_total",
                "tested",
                "killed",
                "survived",
                "timeout",
                "build_errors",
                "partial",
                "mutation_score",
                "duration_ms",
                "test_command",
                "build_command",
                "build_error_groups",
                "mutations",
            ],
        );

        let mutations = value["mutations"].as_array().unwrap();
        assert_object_keys(
            &mutations[0],
            &[
                "id",
                "file",
                "line",
                "operator",
                "description",
                "result",
                "execution",
                "column",
                "original",
                "replacement",
            ],
        );
        assert_object_keys(
            &mutations[1],
            &[
                "id",
                "file",
                "line",
                "operator",
                "description",
                "result",
                "execution",
                "column",
                "original",
                "replacement",
            ],
        );
    }

    #[test]
    fn json_output_marks_partial_early_stop_reports() {
        let mut report = sample_report();
        report.planned_total = 5;
        report.early_stop_reason = Some("--max-survivors 1 reached".into());

        let json_str = to_json_string(&report).expect("sample report should serialize");
        let value: Value = serde_json::from_str(&json_str).expect("sample report JSON is valid");

        assert_eq!(value["total"], 2);
        assert_eq!(value["planned_total"], 5);
        assert_eq!(value["partial"], true);
        assert_eq!(value["early_stop_reason"], "--max-survivors 1 reached");
    }

    #[test]
    fn json_output_includes_baseline_timing_when_present() -> anyhow::Result<()> {
        let mut report = sample_report();
        report.baseline_timing = Some(BaselineTiming {
            build_command: vec!["cargo".into(), "check".into()],
            build_duration: Some(Duration::from_millis(250)),
            test_command: vec!["cargo".into(), "test".into()],
            test_duration: Duration::from_millis(750),
            calibrated_timeout: Duration::from_secs(5),
        });

        let json_str = to_json_string(&report)?;
        let value: Value = serde_json::from_str(&json_str)?;

        assert_eq!(
            value["baseline_timing"]["build_command"],
            serde_json::json!(["cargo", "check"])
        );
        assert_eq!(value["baseline_timing"]["build_duration_ms"], 250);
        assert_eq!(
            value["baseline_timing"]["test_command"],
            serde_json::json!(["cargo", "test"])
        );
        assert_eq!(value["baseline_timing"]["test_duration_ms"], 750);
        assert_eq!(value["baseline_timing"]["calibrated_timeout_ms"], 5000);
        Ok(())
    }

    #[test]
    fn json_score_zero_when_all_build_errors() {
        let report = MutationReport {
            results: vec![(
                Mutation {
                    id: 1,
                    file: PathBuf::from("src/a.rs"),
                    language: String::new(),
                    line: 1,
                    column: 1,
                    operator: "op".to_string(),
                    description: "d".to_string(),
                    original: "x".to_string(),
                    replacement: "y".to_string(),
                    byte_range: 0..1,
                },
                MutationResult::BuildError,
            )],
            execution_provenance: BTreeMap::new(),
            build_error_diagnostics: vec![],
            schemata: None,
            baseline_timing: None,
            duration: Duration::from_millis(100),
            test_command: None,
            build_command: vec![],
            planned_total: 1,
            early_stop_reason: None,
            total: 1,
            killed: 0,
            survived: 0,
            timeout: 0,
            build_errors: 1,
        };
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(value["mutation_score"], 0.0);
        assert_eq!(value["tested"], 0);
    }

    #[test]
    fn json_output_includes_build_error_groups() {
        let diagnostic = BuildErrorDiagnostic::new(
            1,
            "regular",
            "build_command",
            vec!["cargo".into(), "check".into()],
            "error[E0308]: mismatched types",
        );
        let fingerprint = diagnostic.fingerprint.clone();
        let report = MutationReport {
            results: vec![(
                Mutation {
                    id: 1,
                    file: PathBuf::from("src/a.rs"),
                    language: "rust".to_string(),
                    line: 1,
                    column: 1,
                    operator: "eq_to_neq".to_string(),
                    description: "d".to_string(),
                    original: "==".to_string(),
                    replacement: "!=".to_string(),
                    byte_range: 0..2,
                },
                MutationResult::BuildError,
            )],
            execution_provenance: BTreeMap::new(),
            build_error_diagnostics: vec![diagnostic],
            schemata: None,
            baseline_timing: None,
            duration: Duration::from_millis(100),
            test_command: None,
            build_command: vec![],
            planned_total: 1,
            early_stop_reason: None,
            total: 1,
            killed: 0,
            survived: 0,
            timeout: 0,
            build_errors: 1,
        };

        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(value["build_error_groups"][0]["count"], 1);
        assert_eq!(value["build_error_groups"][0]["language"], "rust");
        assert_eq!(value["build_error_groups"][0]["operator"], "eq_to_neq");
        assert_eq!(value["build_error_groups"][0]["runner"], "regular");
        assert_eq!(value["build_error_groups"][0]["phase"], "build_command");
        assert_eq!(
            value["build_error_groups"][0]["command"],
            serde_json::json!(["cargo", "check"])
        );
        assert_eq!(
            value["mutations"][0]["build_error_fingerprint"],
            serde_json::json!(fingerprint)
        );
        assert_eq!(
            value["mutations"][0]["execution"],
            serde_json::json!({"state": "not_executed", "reason": "build_error"})
        );
    }

    #[test]
    fn json_score_100_when_empty_report() {
        let report = MutationReport {
            results: vec![],
            execution_provenance: BTreeMap::new(),
            build_error_diagnostics: vec![],
            schemata: None,
            baseline_timing: None,
            duration: Duration::from_millis(0),
            test_command: None,
            build_command: vec![],
            planned_total: 0,
            early_stop_reason: None,
            total: 0,
            killed: 0,
            survived: 0,
            timeout: 0,
            build_errors: 0,
        };
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(value["mutation_score"], 100.0);
        assert_eq!(value["total"], 0);
    }

    #[test]
    fn json_output_includes_schemata_stats_when_present() {
        let mut report = sample_report();
        report.schemata = Some(SchemataReport {
            fast_path: 3,
            fallback: 2,
            fallback_reasons: vec![SchemataFallbackReasonCount {
                reason: "unsupported_operator".into(),
                count: 2,
            }],
        });

        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(value["schemata"]["fast_path"], 3);
        assert_eq!(value["schemata"]["fallback"], 2);
        assert_eq!(
            value["schemata"]["fallback_reasons"][0]["reason"],
            "unsupported_operator"
        );
        assert_eq!(value["schemata"]["fallback_reasons"][0]["count"], 2);
    }

    #[test]
    fn json_output_includes_mutation_details() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let mutations = value["mutations"].as_array().unwrap();

        // All mutations should include exact before/after details for `togi explain`.
        assert_eq!(mutations[0]["column"], 10);
        assert_eq!(mutations[0]["original"], "<");
        assert_eq!(mutations[0]["replacement"], "<=");
        assert!(mutations[0]["diff"].is_null());

        assert_eq!(mutations[1]["column"], 5);
        assert_eq!(mutations[1]["original"], "==");
        assert_eq!(mutations[1]["replacement"], "!=");
        // diff is None because the test file doesn't exist on disk
        assert!(mutations[1]["diff"].is_null());
    }

    #[test]
    fn json_dry_run_is_distinct_machine_readable_document() {
        let report = sample_report();
        let value: Value =
            serde_json::from_str(&to_dry_run_json(&[report.results[0].0.clone()]).unwrap())
                .unwrap();

        assert_eq!(value["kind"], "dry_run");
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["planned_total"], 1);
        assert_eq!(value["mutations"][0]["id"], 1);
        assert!(value["mutations"][0].get("result").is_none());
        assert!(value.get("mutation_score").is_none());
    }

    #[test]
    fn json_suite_failure_has_no_mutation_report_fields() {
        let failure = RunSuiteFailure {
            phase: SuiteFailurePhase::Test,
            command: vec!["false".into()],
            outcome: RunSuiteFailureOutcome::Failed {
                output: Some("test output".into()),
            },
        };

        let value: Value =
            serde_json::from_str(&to_run_suite_failure_json(&failure).unwrap()).unwrap();

        assert_eq!(value["kind"], "suite_failure");
        assert_eq!(value["phase"], "test");
        assert_eq!(value["command"], serde_json::json!(["false"]));
        assert_eq!(value["outcome"], "failed");
        assert_eq!(value["output"], "test output");
        assert!(value.get("mutations").is_none());
        assert!(value.get("mutation_score").is_none());
    }

    #[test]
    fn json_suite_failure_serializes_cannot_run_detail() {
        let failure = RunSuiteFailure {
            phase: SuiteFailurePhase::Build,
            command: vec!["missing-command".into()],
            outcome: RunSuiteFailureOutcome::CannotRun {
                detail: "not found".into(),
            },
        };

        let value: Value =
            serde_json::from_str(&to_run_suite_failure_json(&failure).unwrap()).unwrap();

        assert_eq!(value["phase"], "build");
        assert_eq!(value["outcome"], "cannot_run");
        assert_eq!(value["detail"], "not found");
        assert!(value.get("output").is_none());
        assert!(value.get("timeout_ms").is_none());
    }
}
