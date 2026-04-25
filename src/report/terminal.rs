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
                if let Some(diff) = super::mutation_diff(mutation) {
                    for diff_line in diff.lines() {
                        if diff_line.starts_with('-') {
                            println!("              {}", diff_line.red());
                        } else if diff_line.starts_with('+') {
                            println!("              {}", diff_line.green());
                        } else {
                            println!("              {}", diff_line.dimmed());
                        }
                    }
                }
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
        "Results: {} killed, {} survived, {} timeout, {} build errors",
        report.killed, report.survived, report.timeout, report.build_errors
    );
    let tested = report.total - report.build_errors;
    let score = if tested > 0 {
        (report.killed as f64 / tested as f64) * 100.0
    } else if report.total == 0 {
        100.0
    } else {
        0.0
    };
    println!("Mutation score (test kills only): {:.1}%", score);
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
                if let Some(diff) = super::mutation_diff(mutation) {
                    for diff_line in diff.lines() {
                        writeln!(out, "              {}", diff_line).unwrap();
                    }
                }
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
        "Results: {} killed, {} survived, {} timeout, {} build errors",
        report.killed, report.survived, report.timeout, report.build_errors
    )
    .unwrap();
    let tested = report.total - report.build_errors;
    let score = if tested > 0 {
        (report.killed as f64 / tested as f64) * 100.0
    } else if report.total == 0 {
        100.0
    } else {
        0.0
    };
    writeln!(out, "Mutation score (test kills only): {:.1}%", score).unwrap();
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
                        language: String::new(),
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
                        language: String::new(),
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
        assert!(output.contains("Results: 1 killed, 1 survived, 0 timeout, 0 build errors"));
        assert!(output.contains("Mutation score (test kills only): 50.0%"));
        assert!(output.contains("Duration: 1.23s"));
    }
}
