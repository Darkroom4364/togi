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
    println!(
        "Mutation score (test kills only): {:.1}%",
        super::mutation_score(report)
    );
    println!("Duration: {:.2}s", report.duration.as_secs_f64());
    println!("{}", separator);

    if let Some(guidance) = all_build_error_guidance(report) {
        println!();
        print!("{guidance}");
    }
}

/// Guidance text when every mutation is a build error.
fn all_build_error_guidance(report: &MutationReport) -> Option<String> {
    if report.build_errors == 0 || report.build_errors != report.total {
        return None;
    }
    Some(
        "All mutations caused build errors — no mutations were testable.\n\
         This typically happens with strictly-typed languages where mutations\n\
         break compilation. Try:\n\
         \x20 togi check --operators=-string_to_empty    (skip string mutations)\n\
         \x20 togi check --operators=binary,removal       (only logic operators)\n\
         \x20 togi check --build-cmd='cargo check'        (custom build check)\n"
            .to_string(),
    )
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
    writeln!(
        out,
        "Mutation score (test kills only): {:.1}%",
        super::mutation_score(report)
    )
    .unwrap();
    writeln!(out, "Duration: {:.2}s", report.duration.as_secs_f64()).unwrap();
    writeln!(out, "{}", separator).unwrap();

    if let Some(guidance) = all_build_error_guidance(report) {
        writeln!(out).unwrap();
        write!(out, "{guidance}").unwrap();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mutation;
    use crate::test_helpers::sample_report;
    use std::path::PathBuf;
    use std::time::Duration;

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
    fn terminal_output_summary_with_timeout_and_build_errors() {
        let report = MutationReport {
            results: vec![
                (
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
                    MutationResult::Killed,
                ),
                (
                    Mutation {
                        id: 2,
                        file: PathBuf::from("src/b.rs"),
                        language: String::new(),
                        line: 2,
                        column: 1,
                        operator: "op".to_string(),
                        description: "d".to_string(),
                        original: "x".to_string(),
                        replacement: "y".to_string(),
                        byte_range: 0..1,
                    },
                    MutationResult::Timeout,
                ),
                (
                    Mutation {
                        id: 3,
                        file: PathBuf::from("src/c.rs"),
                        language: String::new(),
                        line: 3,
                        column: 1,
                        operator: "op".to_string(),
                        description: "d".to_string(),
                        original: "x".to_string(),
                        replacement: "y".to_string(),
                        byte_range: 0..1,
                    },
                    MutationResult::BuildError,
                ),
            ],
            duration: Duration::from_millis(500),
            total: 3,
            killed: 1,
            survived: 0,
            timeout: 1,
            build_errors: 1,
        };
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1 killed, 0 survived, 1 timeout, 1 build errors"));
        // tested = 3 - 1 = 2, score = 1/2 = 50%
        assert!(output.contains("Mutation score (test kills only): 50.0%"));
    }

    #[test]
    fn terminal_output_contains_summary() {
        let report = sample_report();
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1 killed, 1 survived, 0 timeout, 0 build errors"));
        assert!(output.contains("Mutation score (test kills only): 50.0%"));
        assert!(output.contains("Duration: 1.23s"));
    }

    #[test]
    fn all_build_errors_shows_guidance() {
        let report = MutationReport {
            results: vec![(
                Mutation {
                    id: 0,
                    file: PathBuf::from("test.rs"),
                    line: 1,
                    column: 1,
                    operator: "eq_to_neq".into(),
                    description: "test".into(),
                    original: "==".into(),
                    replacement: "!=".into(),
                    byte_range: 0..2,
                    language: "rust".into(),
                },
                MutationResult::BuildError,
            )],
            duration: Duration::from_secs(1),
            total: 1,
            killed: 0,
            survived: 0,
            timeout: 0,
            build_errors: 1,
        };
        let output = format_report_plain(&report);
        assert!(output.contains("All mutations caused build errors"));
        assert!(output.contains("--operators="));
        assert!(output.contains("--build-cmd="));
    }

    #[test]
    fn partial_build_errors_no_guidance() {
        let report = MutationReport {
            results: vec![],
            duration: Duration::from_secs(1),
            total: 2,
            killed: 1,
            survived: 0,
            timeout: 0,
            build_errors: 1,
        };
        let output = format_report_plain(&report);
        assert!(!output.contains("All mutations caused build errors"));
    }
}
