use crate::{Mutation, MutationExecution, MutationReport, MutationResult, TestSelectionProvenance};

/// Format a GitHub Actions warning annotation for a survived mutation.
fn format_annotation_with_baseline(
    mutation: &Mutation,
    execution: MutationExecution,
    status: Option<crate::baseline::SurvivorBaselineStatus>,
    selection: Option<TestSelectionProvenance>,
    advisory: Option<&str>,
) -> String {
    let provenance = match execution {
        MutationExecution::Executed => String::new(),
        MutationExecution::ExactCache => " [reused: exact cache]".to_string(),
        MutationExecution::IncrementalHistory => " [reused: incremental history]".to_string(),
        MutationExecution::NotExecuted(reason) => format!(" [not executed: {reason}]"),
    };
    let baseline = status
        .map(|status| format!(" [baseline: {}]", status.as_str()))
        .unwrap_or_default();
    let selection = selection
        .map(|selection| format!(" [selection: {selection}]"))
        .unwrap_or_default();
    let advisory = advisory
        .map(|reason| format!(" [likely equivalent (advisory): {reason}]"))
        .unwrap_or_default();
    let message = format!(
        "Survived mutation: {} ({}){provenance}{baseline}{selection}{advisory}",
        mutation.operator, mutation.description
    );
    format!(
        "::warning file={},line={}::{}",
        escape_property(&mutation.file.display().to_string()),
        mutation.line,
        escape_data(&message)
    )
}

fn escape_data(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn escape_property(value: &str) -> String {
    escape_data(value).replace(':', "%3A").replace(',', "%2C")
}

/// Collect warning annotations for every survived mutation in `report`.
fn annotations_with_baseline(
    report: &MutationReport,
    comparison: Option<&crate::baseline::SurvivorBaselineComparison>,
) -> Vec<String> {
    let advisories = crate::equivalent::advisories_for(report);
    report
        .results
        .iter()
        .filter(|(_, result)| *result == MutationResult::Survived)
        .map(|(mutation, result)| {
            format_annotation_with_baseline(
                mutation,
                report.execution_for(mutation.id, *result),
                comparison.and_then(|comparison| comparison.status_for(mutation.id)),
                report.selection_for(mutation.id),
                advisories.get(&mutation.id).map(|reason| reason.message()),
            )
        })
        .collect()
}

/// Print GitHub Actions workflow annotations for survived mutations.
/// These show inline on PR diffs.
pub fn print_report(report: &MutationReport) {
    print_report_with_baseline(report, None)
}

pub fn print_report_with_baseline(
    report: &MutationReport,
    comparison: Option<&crate::baseline::SurvivorBaselineComparison>,
) {
    for line in annotations_with_baseline(report, comparison) {
        println!("{line}");
    }

    let execution_counts = report.execution_counts();
    let tested = execution_counts.executed;
    let score = super::mutation_score(report);
    let uncovered = report.uncovered_count();
    let uncovered_str = if uncovered > 0 {
        format!(", {uncovered} uncovered")
    } else {
        String::new()
    };
    let subsumed = report.subsumed_count();
    let subsumed_str = if subsumed > 0 {
        format!(", {subsumed} subsumed")
    } else {
        String::new()
    };
    eprintln!(
        "Mutation score: {:.1}% ({}/{} freshly executed killed, {} survived, {} timeout, {} build errors{uncovered_str}{subsumed_str})",
        score,
        execution_counts.executed_killed,
        tested,
        report.survived,
        report.timeout,
        report.build_errors
    );
    if execution_counts.reused() > 0 {
        eprintln!(
            "Reused verdicts: {} exact-cache, {} incremental-history",
            execution_counts.exact_cache_reused, execution_counts.incremental_history_reused
        );
    }
    if report.total < report.planned_total {
        eprintln!(
            "Partial results: stopped after {}/{} scheduled mutations",
            report.total, report.planned_total
        );
    }
    if let Some(reason) = &report.early_stop_reason {
        eprintln!("Early stop: {reason}");
    }

    if report.survived > 0 && tested > 0 {
        let message = format!(
            "Mutation score {:.1}% — {} mutations survived",
            score, report.survived
        );
        eprintln!("::error::{}", escape_data(&message));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;

    fn mutation(file: &str, line: usize, operator: &str, description: &str) -> Mutation {
        Mutation {
            id: 0,
            file: PathBuf::from(file),
            language: String::new(),
            line,
            column: 1,
            operator: operator.into(),
            description: description.into(),
            original: "x".into(),
            replacement: "y".into(),
            byte_range: 0..1,
        }
    }

    fn report_with(results: Vec<(Mutation, MutationResult)>) -> MutationReport {
        let killed = results
            .iter()
            .filter(|(_, r)| *r == MutationResult::Killed)
            .count();
        let survived = results
            .iter()
            .filter(|(_, r)| *r == MutationResult::Survived)
            .count();
        MutationReport {
            selection_provenance: std::collections::BTreeMap::new(),
            planned_total: results.len(),
            early_stop_reason: None,
            total: results.len(),
            killed,
            survived,
            timeout: 0,
            build_errors: 0,
            duration: Duration::from_secs(0),
            test_command: None,
            build_command: vec![],
            results,
            execution_provenance: BTreeMap::new(),
            build_error_diagnostics: vec![],
            schemata: None,
            baseline_timing: None,
        }
    }

    #[test]
    fn annotation_format_for_survived_mutation() {
        let m = mutation("src/auth.rs", 47, "lt_to_lte", "changed < to <=");
        let line =
            format_annotation_with_baseline(&m, MutationExecution::Executed, None, None, None);
        assert_eq!(
            line,
            "::warning file=src/auth.rs,line=47::Survived mutation: lt_to_lte (changed < to <=)"
        );
    }

    #[test]
    fn annotation_marks_likely_equivalent_survivors_as_advisory() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("fixture.rs");
        std::fs::write(&path, crate::test_helpers::BOOLEAN_LITERAL_SOURCE).unwrap();
        let report = report_with(vec![(
            crate::test_helpers::boolean_literal_mutation(path),
            MutationResult::Survived,
        )]);

        let annotations = annotations_with_baseline(&report, None);

        assert_eq!(annotations.len(), 1);
        assert!(annotations[0].contains("likely equivalent (advisory): both operands are the same boolean literal, so either logical operator produces the same value"));
    }

    #[test]
    fn killed_mutations_emit_no_annotations() {
        let report = report_with(vec![(
            mutation("src/a.rs", 1, "op", "desc"),
            MutationResult::Killed,
        )]);
        assert!(annotations_with_baseline(&report, None).is_empty());
    }

    #[test]
    fn only_survived_mutations_are_annotated() {
        let report = report_with(vec![
            (
                mutation("src/a.rs", 1, "op_a", "killed one"),
                MutationResult::Killed,
            ),
            (
                mutation("src/b.rs", 2, "op_b", "survived one"),
                MutationResult::Survived,
            ),
            (
                mutation("src/c.rs", 3, "op_c", "timed out"),
                MutationResult::Timeout,
            ),
        ]);
        let lines = annotations_with_baseline(&report, None);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("file=src/b.rs"));
        assert!(lines[0].contains("op_b"));
        assert!(lines[0].contains("survived one"));
    }

    #[test]
    fn annotations_include_active_survivor_baseline_status_only() {
        let mut survived = mutation("src/a.rs", 1, "op", "survived");
        survived.id = 5;
        let mut killed = mutation("src/b.rs", 2, "op", "killed");
        killed.id = 6;
        let report = report_with(vec![
            (survived, MutationResult::Survived),
            (killed, MutationResult::Killed),
        ]);
        let comparison =
            crate::baseline::SurvivorBaselineComparison::from_statuses(BTreeMap::from([
                (5, crate::baseline::SurvivorBaselineStatus::Historic),
                (6, crate::baseline::SurvivorBaselineStatus::New),
            ]));

        let inactive = annotations_with_baseline(&report, None);
        assert!(!inactive[0].contains("baseline:"));
        let active = annotations_with_baseline(&report, Some(&comparison));
        assert_eq!(active.len(), 1);
        assert!(active[0].ends_with("[baseline: historic]"));
        assert!(!active[0].contains("baseline: new"));
    }

    #[test]
    fn annotations_label_mixed_survivor_provenance() {
        let mut fresh = mutation("src/fresh.rs", 1, "fresh", "fresh survivor");
        fresh.id = 1;
        let mut exact = mutation("src/exact.rs", 2, "exact", "cached survivor");
        exact.id = 2;
        let mut history = mutation("src/history.rs", 3, "history", "history survivor");
        history.id = 3;
        let mut report = report_with(vec![
            (fresh, MutationResult::Survived),
            (exact, MutationResult::Survived),
            (history, MutationResult::Survived),
        ]);
        report
            .execution_provenance
            .insert(2, MutationExecution::ExactCache);
        report
            .execution_provenance
            .insert(3, MutationExecution::IncrementalHistory);

        let lines = annotations_with_baseline(&report, None);

        assert_eq!(lines.len(), 3);
        assert!(!lines[0].contains("reused"));
        assert!(lines[1].contains("[reused: exact cache]"));
        assert!(lines[2].contains("[reused: incremental history]"));
    }

    #[test]
    fn uncovered_mutations_emit_no_annotations() {
        let report = report_with(vec![(
            mutation("src/dead.rs", 7, "op", "desc"),
            MutationResult::Uncovered,
        )]);
        assert!(annotations_with_baseline(&report, None).is_empty());
    }

    #[test]
    fn annotation_escapes_special_chars_in_path_and_message() {
        let m = mutation("src/odd,name:thing.rs", 10, "op", "msg with %, \r\n chars");
        let line = format_annotation_with_baseline(
            &m,
            MutationExecution::Executed,
            Some(crate::baseline::SurvivorBaselineStatus::Historic),
            None,
            None,
        );
        assert_eq!(
            line,
            "::warning file=src/odd%2Cname%3Athing.rs,line=10::Survived mutation: op (msg with %25, %0D%0A chars) [baseline: historic]"
        );
    }

    #[test]
    fn command_escape_rules_are_position_specific() {
        assert_eq!(escape_data("a%b\r\nc,d:e"), "a%25b%0D%0Ac,d:e");
        assert_eq!(escape_property("a%b\r\nc,d:e"), "a%25b%0D%0Ac%2Cd%3Ae");
    }
}
