use crate::{MutationReport, MutationResult};

/// Print GitHub Actions workflow annotations for survived mutations.
/// These show inline on PR diffs.
pub fn print_report(report: &MutationReport) {
    for (mutation, result) in &report.results {
        if *result != MutationResult::Survived {
            continue;
        }
        let file = mutation.file.display();
        let line = mutation.line;
        let operator = &mutation.operator;
        let desc = &mutation.description;
        println!(
            "::warning file={},line={}::Survived mutation: {} ({})",
            file, line, operator, desc
        );
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
