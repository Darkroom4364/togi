use crate::{Mutation, MutationReport, MutationResult};

/// Format a single GitHub Actions warning annotation for a survived mutation.
fn format_annotation(mutation: &Mutation) -> String {
    format!(
        "::warning file={},line={}::Survived mutation: {} ({})",
        mutation.file.display(),
        mutation.line,
        mutation.operator,
        mutation.description
    )
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

    if report.survived > 0 && tested > 0 {
        eprintln!(
            "::error::Mutation score {:.1}% — {} mutations survived",
            score, report.survived
        );
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
            total: results.len(),
            killed,
            survived,
            timeout: 0,
            build_errors: 0,
            duration: Duration::from_secs(0),
            results,
        }
    }

    #[test]
    fn annotation_format_for_survived_mutation() {
        let m = mutation("src/auth.rs", 47, "binary/lt_to_lte", "changed < to <=");
        let line = format_annotation(&m);
        assert_eq!(
            line,
            "::warning file=src/auth.rs,line=47::Survived mutation: binary/lt_to_lte (changed < to <=)"
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
    fn annotation_preserves_special_chars_in_path_and_message() {
        // Document current behavior: special chars (commas, colons, percent
        // signs, newlines) are passed through verbatim. GitHub's worker
        // command parser may misinterpret these — fixing that is a separate
        // concern from these tests.
        let m = mutation(
            "src/odd,name:thing.rs",
            10,
            "op",
            "msg with %, : and , chars",
        );
        let line = format_annotation(&m);
        assert!(
            line.starts_with("::warning file=src/odd,name:thing.rs,line=10::"),
            "path passed through verbatim: {line}"
        );
        assert!(
            line.ends_with("Survived mutation: op (msg with %, : and , chars)"),
            "message passed through verbatim: {line}"
        );
    }
}
