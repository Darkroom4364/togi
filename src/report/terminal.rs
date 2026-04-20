use crate::{MutationReport, MutationResult};
#[allow(unused_imports)]
use colored::Colorize;

pub fn print_report(report: &MutationReport) {
    println!();

    for (mutation, result) in &report.results {
        let file = mutation.file.display();
        let line = mutation.line;
        let operator = &mutation.operator;
        let desc = &mutation.description;

        match result {
            MutationResult::Killed => {
                println!(
                    "  {} {}:{}  {} {}: {}",
                    "✓ KILLED".green(),
                    file,
                    line,
                    "—".dimmed(),
                    operator.dimmed(),
                    desc
                );
            }
            MutationResult::Survived => {
                println!(
                    "  {} {}:{}  {} {}: {}",
                    "✗ SURVIVED".red(),
                    file,
                    line,
                    "—".dimmed(),
                    operator.dimmed(),
                    desc
                );
                println!(
                    "              {}",
                    "Your tests don't catch this mutation.".red()
                );
            }
            MutationResult::Timeout => {
                println!(
                    "  {} {}:{}  {} {}: {}",
                    "⏱ TIMEOUT".yellow(),
                    file,
                    line,
                    "—".dimmed(),
                    operator.dimmed(),
                    desc
                );
            }
            MutationResult::BuildError => {
                println!(
                    "  {} {}:{}  {} {}: {}",
                    "⚠ BUILD ERROR".yellow(),
                    file,
                    line,
                    "—".dimmed(),
                    operator.dimmed(),
                    desc
                );
            }
        }
        println!();
    }

    let separator = "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━";
    println!("{}", separator);
    println!(
        "Results: {}/{} mutations killed ({} survived)",
        report.killed, report.total, report.survived
    );
    println!("Duration: {:.2}s", report.duration.as_secs_f64());
    println!("{}", separator);
}

/// Format report as a plain-text string (no ANSI colors, for testing).
pub fn format_report_plain(report: &MutationReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    for (mutation, result) in &report.results {
        let file = mutation.file.display();
        let line = mutation.line;
        let operator = &mutation.operator;
        let desc = &mutation.description;

        match result {
            MutationResult::Killed => {
                writeln!(
                    out,
                    "  ✓ KILLED    {}:{} — {}: {}",
                    file, line, operator, desc
                )
                .unwrap();
            }
            MutationResult::Survived => {
                writeln!(
                    out,
                    "  ✗ SURVIVED  {}:{} — {}: {}",
                    file, line, operator, desc
                )
                .unwrap();
                writeln!(out, "              Your tests don't catch this mutation.").unwrap();
            }
            MutationResult::Timeout => {
                writeln!(
                    out,
                    "  ⏱ TIMEOUT   {}:{} — {}: {}",
                    file, line, operator, desc
                )
                .unwrap();
            }
            MutationResult::BuildError => {
                writeln!(
                    out,
                    "  ⚠ BUILD ERROR {}:{} — {}: {}",
                    file, line, operator, desc
                )
                .unwrap();
            }
        }
    }

    let separator = "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━";
    writeln!(out, "{}", separator).unwrap();
    writeln!(
        out,
        "Results: {}/{} mutations killed ({} survived)",
        report.killed, report.total, report.survived
    )
    .unwrap();
    writeln!(out, "Duration: {:.2}s", report.duration.as_secs_f64()).unwrap();
    writeln!(out, "{}", separator).unwrap();

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mutation;
    use std::path::PathBuf;
    use std::time::Duration;

    fn sample_report() -> MutationReport {
        MutationReport {
            results: vec![
                (
                    Mutation {
                        id: 1,
                        file: PathBuf::from("src/auth.rs"),
                        line: 47,
                        column: 10,
                        operator: "binary/lt_to_lte".to_string(),
                        description: "changed < to <=".to_string(),
                        original: "<".to_string(),
                        replacement: "<=".to_string(),
                        byte_range: 0..1,
                    },
                    MutationResult::Killed,
                ),
                (
                    Mutation {
                        id: 2,
                        file: PathBuf::from("src/handler.rs"),
                        line: 15,
                        column: 5,
                        operator: "binary/eq_to_neq".to_string(),
                        description: "changed == to !=".to_string(),
                        original: "==".to_string(),
                        replacement: "!=".to_string(),
                        byte_range: 0..2,
                    },
                    MutationResult::Survived,
                ),
            ],
            duration: Duration::from_millis(1234),
            total: 2,
            killed: 1,
            survived: 1,
            timeout: 0,
            build_errors: 0,
        }
    }

    #[test]
    fn terminal_output_contains_killed() {
        let report = sample_report();
        let output = format_report_plain(&report);
        assert!(output.contains("✓ KILLED"));
        assert!(output.contains("src/auth.rs:47"));
        assert!(output.contains("binary/lt_to_lte"));
    }

    #[test]
    fn terminal_output_contains_survived() {
        let report = sample_report();
        let output = format_report_plain(&report);
        assert!(output.contains("✗ SURVIVED"));
        assert!(output.contains("src/handler.rs:15"));
        assert!(output.contains("Your tests don't catch this mutation."));
    }

    #[test]
    fn terminal_output_contains_summary() {
        let report = sample_report();
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1/2 mutations killed (1 survived)"));
        assert!(output.contains("Duration: 1.23s"));
    }
}
