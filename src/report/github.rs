use crate::{Mutation, MutationReport, MutationResult};

/// Format a single GitHub Actions warning annotation for a survived mutation.
fn format_annotation(mutation: &Mutation) -> String {
    let message = format!(
        "Survived mutation: {} ({})",
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

/// Collect warning annotations for every survived mutation in `report`,
/// in the same order they appear in `report.results`.
fn annotations(report: &MutationReport) -> Vec<String> {
    report
        .results
        .iter()
        .filter(|(_, r)| *r == MutationResult::Survived)
        .map(|(m, _)| format_annotation(m))
        .collect()
}

/// Print GitHub Actions workflow annotations for survived mutations.
/// These show inline on PR diffs.
pub fn print_report(report: &MutationReport) {
    for line in annotations(report) {
        println!("{line}");
    }

    let tested = report.total.saturating_sub(report.build_errors);
    let score = super::mutation_score(report);
    eprintln!(
        "Mutation score: {:.1}% ({} killed, {} survived, {} timeout, {} build errors)",
        score, report.killed, report.survived, report.timeout, report.build_errors
    );
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
    use std::path::PathBuf;
    use std::time::Duration;

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
            build_error_diagnostics: vec![],
            schemata: None,
        }
    }

    #[test]
    fn annotation_format_for_survived_mutation() {
        let m = mutation("src/auth.rs", 47, "lt_to_lte", "changed < to <=");
        let line = format_annotation(&m);
        assert_eq!(
            line,
            "::warning file=src/auth.rs,line=47::Survived mutation: lt_to_lte (changed < to <=)"
        );
    }

    #[test]
    fn killed_mutations_emit_no_annotations() {
        let report = report_with(vec![(
            mutation("src/a.rs", 1, "op", "desc"),
            MutationResult::Killed,
        )]);
        assert!(annotations(&report).is_empty());
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
        let lines = annotations(&report);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("file=src/b.rs"));
        assert!(lines[0].contains("op_b"));
        assert!(lines[0].contains("survived one"));
    }

    #[test]
    fn annotation_escapes_special_chars_in_path_and_message() {
        let m = mutation("src/odd,name:thing.rs", 10, "op", "msg with %, \r\n chars");
        let line = format_annotation(&m);
        assert_eq!(
            line,
            "::warning file=src/odd%2Cname%3Athing.rs,line=10::Survived mutation: op (msg with %25, %0D%0A chars)"
        );
    }

    #[test]
    fn command_escape_rules_are_position_specific() {
        assert_eq!(escape_data("a%b\r\nc,d:e"), "a%25b%0D%0Ac,d:e");
        assert_eq!(escape_property("a%b\r\nc,d:e"), "a%25b%0D%0Ac%2Cd%3Ae");
    }
}
