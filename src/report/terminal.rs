use crate::{MutationReport, MutationResult};
use colored::Colorize;
use std::fmt::Write;

pub fn print_report(report: &MutationReport) {
    print!("{}", format_report(report, true));
}

pub fn format_report_plain(report: &MutationReport) -> String {
    format_report(report, false)
}

fn format_report(report: &MutationReport, color: bool) -> String {
    let mut out = String::new();
    writeln!(out).unwrap();

    for (mutation, result) in &report.results {
        let file = mutation.file.display().to_string();
        let line = mutation.line;
        let operator = &mutation.operator;
        let desc = &mutation.description;

        let (tag, extra) = match result {
            MutationResult::Killed => ("✓ KILLED", None),
            MutationResult::Survived => {
                let mut detail = String::new();
                writeln!(
                    detail,
                    "              {}",
                    if color {
                        "Your tests don't catch this mutation.".red().to_string()
                    } else {
                        "Your tests don't catch this mutation.".to_string()
                    }
                )
                .unwrap();
                if let Some(diff) = super::mutation_diff(mutation) {
                    for diff_line in diff.lines() {
                        if color {
                            if diff_line.starts_with('-') {
                                writeln!(detail, "              {}", diff_line.red())
                            } else if diff_line.starts_with('+') {
                                writeln!(detail, "              {}", diff_line.green())
                            } else {
                                writeln!(detail, "              {}", diff_line.dimmed())
                            }
                        } else {
                            writeln!(detail, "              {diff_line}")
                        }
                        .unwrap();
                    }
                }
                ("✗ SURVIVED", Some(detail))
            }
            MutationResult::Timeout => ("⏱ TIMEOUT", None),
            MutationResult::BuildError => ("⚠ BUILD ERROR", None),
        };

        if color {
            let tag_colored = match result {
                MutationResult::Killed => tag.green().to_string(),
                MutationResult::Survived => tag.red().to_string(),
                MutationResult::Timeout | MutationResult::BuildError => tag.yellow().to_string(),
            };
            writeln!(
                out,
                "  {} {}:{}  {} {}: {}",
                tag_colored,
                file,
                line,
                "—".dimmed(),
                operator.dimmed(),
                desc
            )
        } else {
            writeln!(
                out,
                "  {:<14}{}:{} — {}: {}",
                tag, file, line, operator, desc
            )
        }
        .unwrap();

        if let Some(detail) = extra {
            write!(out, "{detail}").unwrap();
        }
        writeln!(out).unwrap();
    }

    let separator = "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━";
    writeln!(out, "{separator}").unwrap();
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
    writeln!(out, "{separator}").unwrap();

    if let Some(guidance) = all_build_error_guidance(report) {
        writeln!(out).unwrap();
        write!(out, "{guidance}").unwrap();
    }

    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mutation;
    use std::path::PathBuf;
    use std::time::Duration;

    fn mutation(id: u32, file: &str, line: usize) -> Mutation {
        Mutation {
            id,
            file: PathBuf::from(file),
            language: String::new(),
            line,
            column: 1,
            operator: "op".to_string(),
            description: "d".to_string(),
            original: "x".to_string(),
            replacement: "y".to_string(),
            byte_range: 0..1,
        }
    }

    fn report(results: Vec<(Mutation, MutationResult)>) -> MutationReport {
        let total = results.len();
        let killed = results
            .iter()
            .filter(|(_, result)| *result == MutationResult::Killed)
            .count();
        let survived = results
            .iter()
            .filter(|(_, result)| *result == MutationResult::Survived)
            .count();
        let timeout = results
            .iter()
            .filter(|(_, result)| *result == MutationResult::Timeout)
            .count();
        let build_errors = results
            .iter()
            .filter(|(_, result)| *result == MutationResult::BuildError)
            .count();

        MutationReport {
            results,
            duration: Duration::from_millis(500),
            total,
            killed,
            survived,
            timeout,
            build_errors,
        }
    }

    #[test]
    fn terminal_output_lists_mutations_with_status_and_location() {
        let killed_path = PathBuf::from("src").join("a.rs");
        let survived_path = PathBuf::from("src").join("b.rs");
        let report = report(vec![
            (
                mutation(1, &killed_path.display().to_string(), 1),
                MutationResult::Killed,
            ),
            (
                mutation(2, &survived_path.display().to_string(), 2),
                MutationResult::Survived,
            ),
        ]);
        let output = format_report_plain(&report);
        let killed_location = format!("{}:1", killed_path.display());
        let survived_location = format!("{}:2", survived_path.display());

        assert!(
            output
                .lines()
                .any(|line| line.contains("KILLED") && line.contains(&killed_location))
        );
        assert!(
            output
                .lines()
                .any(|line| line.contains("SURVIVED") && line.contains(&survived_location))
        );
        assert!(output.contains("Your tests don't catch this mutation."));
    }

    #[test]
    fn terminal_output_summary_with_timeout_and_build_errors() {
        let report = report(vec![
            (mutation(1, "src/a.rs", 1), MutationResult::Killed),
            (mutation(2, "src/b.rs", 2), MutationResult::Timeout),
            (mutation(3, "src/c.rs", 3), MutationResult::BuildError),
        ]);
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1 killed, 0 survived, 1 timeout, 1 build errors"));
        assert!(output.contains("Mutation score (test kills only): 50.0%"));
    }

    #[test]
    fn terminal_output_contains_summary() {
        let report = report(vec![
            (mutation(1, "src/a.rs", 1), MutationResult::Killed),
            (mutation(2, "src/b.rs", 2), MutationResult::Survived),
        ]);
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1 killed, 1 survived, 0 timeout, 0 build errors"));
        assert!(output.contains("Mutation score (test kills only): 50.0%"));
    }

    #[test]
    fn all_build_errors_shows_guidance() {
        let report = report(vec![(
            mutation(1, "src/a.rs", 1),
            MutationResult::BuildError,
        )]);
        let output = format_report_plain(&report);
        assert!(output.contains("All mutations caused build errors"));
        assert!(output.contains("--operators="));
        assert!(output.contains("--build-cmd="));
    }

    #[test]
    fn partial_build_errors_no_guidance() {
        let report = report(vec![
            (mutation(1, "src/a.rs", 1), MutationResult::Killed),
            (mutation(2, "src/b.rs", 2), MutationResult::BuildError),
        ]);
        let output = format_report_plain(&report);
        assert!(!output.contains("All mutations caused build errors"));
    }
}
