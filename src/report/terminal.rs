use crate::runner::{RunSuiteFailure, RunSuiteFailureOutcome, SuiteFailurePhase};
use crate::{MutationReport, MutationResult};
use std::fmt::Write;
use std::io::IsTerminal;

pub fn print_report(report: &MutationReport) {
    print!("{}", format_report(report, should_colorize(), None));
}

pub fn print_report_with_baseline(
    report: &MutationReport,
    comparison: Option<&crate::baseline::SurvivorBaselineComparison>,
) {
    print!("{}", format_report(report, should_colorize(), comparison));
}

pub fn format_report_plain(report: &MutationReport) -> String {
    format_report(report, false, None)
}

pub fn format_report_plain_with_baseline(
    report: &MutationReport,
    comparison: &crate::baseline::SurvivorBaselineComparison,
) -> String {
    format_report(report, false, Some(comparison))
}

/// Print a run-level suite failure instead of a mutation report.
pub fn print_run_suite_failure(failure: &RunSuiteFailure) {
    eprint!("{}", format_run_suite_failure(failure));
}

pub fn format_run_suite_failure(failure: &RunSuiteFailure) -> String {
    let mut out = String::new();
    let phase = match failure.phase {
        SuiteFailurePhase::Build => "build",
        SuiteFailurePhase::Test => "test",
    };
    writeln!(out, "Test suite failure before mutation execution.").unwrap();
    writeln!(out, "Baseline phase: {phase}").unwrap();
    writeln!(out, "Command: {}", failure.command.join(" ")).unwrap();
    match &failure.outcome {
        RunSuiteFailureOutcome::Failed { output } => {
            writeln!(out, "Outcome: failed").unwrap();
            if let Some(output) = output {
                writeln!(out, "Output:\n{output}").unwrap();
            }
        }
        RunSuiteFailureOutcome::TimedOut { timeout } => {
            writeln!(
                out,
                "Outcome: timed out after {:.2}s",
                timeout.as_secs_f64()
            )
            .unwrap();
        }
        RunSuiteFailureOutcome::CannotRun { detail } => {
            writeln!(out, "Outcome: could not run").unwrap();
            writeln!(out, "Detail: {detail}").unwrap();
        }
    }
    out
}

fn format_report(
    report: &MutationReport,
    color: bool,
    comparison: Option<&crate::baseline::SurvivorBaselineComparison>,
) -> String {
    let mut out = String::new();
    writeln!(out).unwrap();
    let execution_counts = report.execution_counts();

    for (mutation, result) in &report.results {
        let file = mutation.file.display().to_string();
        let line = mutation.line;
        let operator = &mutation.operator;
        let desc = &mutation.description;
        let execution = report.execution_for(mutation.id, *result);
        let execution_text = execution.to_string();
        let selection_text = report
            .selection_for(mutation.id)
            .map(|selection| format!("; {selection}"))
            .unwrap_or_default();

        let (tag, extra) = match result {
            MutationResult::Killed => ("✓ KILLED", None),
            MutationResult::Survived => {
                let mut detail = String::new();
                writeln!(
                    detail,
                    "              {}",
                    if color {
                        red("Your tests don't catch this mutation.")
                    } else {
                        "Your tests don't catch this mutation.".to_string()
                    }
                )
                .unwrap();
                if let Some(status) =
                    comparison.and_then(|comparison| comparison.status_for(mutation.id))
                {
                    let _ = writeln!(detail, "              Baseline: {}", status.as_str());
                }
                if let Some(diff) = super::mutation_diff(mutation) {
                    for diff_line in diff.lines() {
                        if color {
                            if diff_line.starts_with('-') {
                                writeln!(detail, "              {}", red(diff_line))
                            } else if diff_line.starts_with('+') {
                                writeln!(detail, "              {}", green(diff_line))
                            } else {
                                writeln!(detail, "              {}", dim(diff_line))
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
            MutationResult::Uncovered => {
                let mut detail = String::new();
                writeln!(
                    detail,
                    "              {}",
                    if color {
                        dim("Line has zero test coverage; mutant not executed.")
                    } else {
                        "Line has zero test coverage; mutant not executed.".to_string()
                    }
                )
                .unwrap();
                ("○ UNCOVERED", Some(detail))
            }
            MutationResult::Subsumed => {
                let mut detail = String::new();
                writeln!(
                    detail,
                    "              {}",
                    if color {
                        dim("Same recorded killer test as an earlier mutant; not executed.")
                    } else {
                        "Same recorded killer test as an earlier mutant; not executed.".to_string()
                    }
                )
                .unwrap();
                ("◌ SUBSUMED", Some(detail))
            }
        };

        if color {
            let tag_colored = match result {
                MutationResult::Killed => green(tag),
                MutationResult::Survived => red(tag),
                MutationResult::Timeout | MutationResult::BuildError => yellow(tag),
                MutationResult::Uncovered | MutationResult::Subsumed => dim(tag),
            };
            writeln!(
                out,
                "  {} {}:{}  {} {}: {} [{}{}]",
                tag_colored,
                file,
                line,
                dim("—"),
                dim(operator),
                desc,
                dim(&execution_text),
                dim(&selection_text)
            )
        } else {
            writeln!(
                out,
                "  {:<14}{}:{} — {}: {} [{}{}]",
                tag, file, line, operator, desc, execution_text, selection_text
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
    let uncovered_str = count_suffix(report.uncovered_count(), "uncovered");
    let subsumed_str = count_suffix(report.subsumed_count(), "subsumed");
    writeln!(
        out,
        "Results: {} killed, {} survived, {} timeout, {} build errors{uncovered_str}{subsumed_str}",
        report.killed, report.survived, report.timeout, report.build_errors
    )
    .unwrap();
    let exact_cache_reused =
        count_suffix(execution_counts.exact_cache_reused, "exact-cache reused");
    let incremental_history_reused = count_suffix(
        execution_counts.incremental_history_reused,
        "incremental-history reused",
    );
    let not_executed = count_suffix(execution_counts.not_executed, "not executed");
    writeln!(
        out,
        "Execution: {} freshly tested{exact_cache_reused}{incremental_history_reused}{not_executed}",
        execution_counts.executed
    )
    .unwrap();
    if report.total < report.planned_total {
        writeln!(
            out,
            "Partial: stopped after {}/{} scheduled mutations",
            report.total, report.planned_total
        )
        .expect("writing to String should not fail");
    }
    if let Some(reason) = &report.early_stop_reason {
        writeln!(out, "Early stop: {reason}").expect("writing to String should not fail");
    }
    writeln!(
        out,
        "Mutation score (fresh test kills only): {:.1}%",
        super::mutation_score(report)
    )
    .unwrap();
    writeln!(out, "Duration: {:.2}s", report.duration.as_secs_f64()).unwrap();
    if let Some(timing) = &report.baseline_timing {
        let build = timing
            .build_duration
            .map(|duration| format!(", build {:.2}s", duration.as_secs_f64()))
            .unwrap_or_default();
        out.push_str(&format!(
            "Baseline timing: test {:.2}s{build}; timeout {:.2}s\n",
            timing.test_duration.as_secs_f64(),
            timing.calibrated_timeout.as_secs_f64()
        ));
    }
    if let Some(schemata) = &report.schemata {
        writeln!(
            out,
            "Schemata: {} fast-path, {} fallback",
            schemata.fast_path, schemata.fallback
        )
        .unwrap();
        if !schemata.fallback_reasons.is_empty() {
            let reasons = schemata
                .fallback_reasons
                .iter()
                .map(|reason| format!("{} ({})", reason.reason, reason.count))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(out, "Fallback reasons: {reasons}").unwrap();
        }
    }
    writeln!(out, "{separator}").unwrap();

    let build_error_summary = format_build_error_groups(report);
    if !build_error_summary.is_empty() {
        writeln!(out).unwrap();
        write!(out, "{build_error_summary}").unwrap();
    }

    if let Some(guidance) = all_build_error_guidance(report) {
        writeln!(out).unwrap();
        write!(out, "{guidance}").unwrap();
    }

    out
}

/// ", N label" suffix for summary lines, empty when the count is zero.
fn count_suffix(count: usize, label: &str) -> String {
    if count > 0 {
        format!(", {count} {label}")
    } else {
        String::new()
    }
}

fn format_build_error_groups(report: &MutationReport) -> String {
    let groups = super::build_error_groups(report);
    if groups.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    writeln!(out, "Build error diagnostics:").unwrap();
    for group in groups.iter().take(5) {
        writeln!(
            out,
            "  {} build error{} — {} / {} / {} / {} [{}]",
            group.count,
            if group.count == 1 { "" } else { "s" },
            group.language,
            group.operator,
            group.runner,
            group.phase,
            group.fingerprint
        )
        .unwrap();
        if !group.command.is_empty() {
            writeln!(out, "      command: {}", group.command.join(" ")).unwrap();
        }
        if !group.files.is_empty() {
            let files = group
                .files
                .iter()
                .take(3)
                .map(|file| format!("{} ({})", file.file, file.count))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(out, "      files: {files}").unwrap();
        }
        let first_line = group.message.lines().next().unwrap_or("").trim();
        if !first_line.is_empty() {
            writeln!(out, "      {first_line}").unwrap();
        }
    }
    if groups.len() > 5 {
        writeln!(
            out,
            "  ... {} more build-error group{}",
            groups.len() - 5,
            if groups.len() - 5 == 1 { "" } else { "s" }
        )
        .unwrap();
    }
    out
}

fn ansi(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn green(text: &str) -> String {
    ansi("32", text)
}

fn red(text: &str) -> String {
    ansi("31", text)
}

fn yellow(text: &str) -> String {
    ansi("33", text)
}

fn dim(text: &str) -> String {
    ansi("2", text)
}

fn should_colorize() -> bool {
    color_enabled_from_env(std::io::stdout().is_terminal(), |name| {
        std::env::var(name).ok()
    })
}

fn color_enabled_from_env(is_terminal: bool, env: impl Fn(&str) -> Option<String>) -> bool {
    if env("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
        return true;
    }
    if env("NO_COLOR").is_some() {
        return false;
    }
    if env("CLICOLOR").is_some_and(|value| value == "0") {
        return false;
    }
    is_terminal
}

/// Guidance text when every mutation is a build error.
fn all_build_error_guidance(report: &MutationReport) -> Option<String> {
    if report.results.is_empty()
        || report.results.iter().any(|(mutation, result)| {
            *result != MutationResult::BuildError
                || report.execution_for(mutation.id, *result).is_reused()
        })
    {
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
    use crate::runner::{RunSuiteFailure, RunSuiteFailureOutcome, SuiteFailurePhase};
    use crate::{BuildErrorDiagnostic, Mutation, SurvivorConfirmation, TestSelectionProvenance};
    use std::collections::BTreeMap;
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
            selection_provenance: std::collections::BTreeMap::new(),
            results,
            execution_provenance: BTreeMap::new(),
            build_error_diagnostics: vec![],
            schemata: None,
            baseline_timing: None,
            duration: Duration::from_millis(500),
            test_command: None,
            build_command: vec![],
            planned_total: total,
            early_stop_reason: None,
            total,
            killed,
            survived,
            timeout,
            build_errors,
        }
    }

    #[test]
    fn terminal_output_annotates_survivors_only_when_comparing_baselines() {
        let report = report(vec![
            (mutation(0, "src/killed.rs", 1), MutationResult::Killed),
            (mutation(1, "src/survived.rs", 2), MutationResult::Survived),
        ]);
        let comparison =
            crate::baseline::SurvivorBaselineComparison::from_statuses(BTreeMap::from([
                (0, crate::baseline::SurvivorBaselineStatus::Historic),
                (1, crate::baseline::SurvivorBaselineStatus::New),
            ]));

        let annotated = format_report_plain_with_baseline(&report, &comparison);
        assert!(annotated.contains("Baseline: new"));
        assert_eq!(annotated.matches("Baseline:").count(), 1);
        assert!(!format_report_plain(&report).contains("Baseline:"));
    }

    #[test]
    fn terminal_output_records_selection_confirmation() {
        let mut report = report(vec![(
            mutation(0, "src/survived.rs", 1),
            MutationResult::Survived,
        )]);
        report.selection_provenance.insert(
            0,
            TestSelectionProvenance::Narrowed {
                confirmation: SurvivorConfirmation::ConfirmedSurvived,
            },
        );

        let output = format_report_plain(&report);

        assert!(output.contains("narrowed; confirmation: confirmed_survived"));
    }

    #[test]
    fn terminal_suite_failure_is_not_a_mutation_report() {
        let failure = RunSuiteFailure {
            phase: SuiteFailurePhase::Test,
            command: vec!["false".into()],
            outcome: RunSuiteFailureOutcome::Failed {
                output: Some("test output".into()),
            },
        };

        let output = format_run_suite_failure(&failure);

        assert!(output.contains("Test suite failure before mutation execution."));
        assert!(output.contains("Baseline phase: test"));
        assert!(output.contains("Command: false"));
        assert!(output.contains("Outcome: failed"));
        assert!(output.contains("test output"));
        assert!(!output.contains("Mutation score"));
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
        let mut report = report(vec![
            (mutation(1, "src/a.rs", 1), MutationResult::Killed),
            (mutation(2, "src/b.rs", 2), MutationResult::Timeout),
            (mutation(3, "src/c.rs", 3), MutationResult::BuildError),
        ]);
        report
            .build_error_diagnostics
            .push(BuildErrorDiagnostic::new(
                3,
                "regular",
                "build_command",
                vec!["cargo".into(), "check".into()],
                "error[E0308]: mismatched types",
            ));
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1 killed, 0 survived, 1 timeout, 1 build errors"));
        assert!(output.contains("Mutation score (fresh test kills only): 50.0%"));
        assert!(output.contains("Build error diagnostics:"));
        assert!(output.contains("unknown / op / regular / build_command"));
        assert!(output.contains("command: cargo check"));
        assert!(output.contains("error[E0308]: mismatched types"));
    }

    #[test]
    fn terminal_output_contains_summary() {
        let report = report(vec![
            (mutation(1, "src/a.rs", 1), MutationResult::Killed),
            (mutation(2, "src/b.rs", 2), MutationResult::Survived),
        ]);
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1 killed, 1 survived, 0 timeout, 0 build errors"));
        assert!(output.contains("Mutation score (fresh test kills only): 50.0%"));
    }

    #[test]
    fn terminal_output_shows_execution_provenance() {
        let mut report = report(vec![
            (mutation(1, "src/a.rs", 1), MutationResult::Killed),
            (mutation(2, "src/b.rs", 2), MutationResult::Survived),
        ]);
        report
            .execution_provenance
            .insert(1, crate::MutationExecution::ExactCache);
        report
            .execution_provenance
            .insert(2, crate::MutationExecution::IncrementalHistory);

        let output = format_report_plain(&report);

        assert!(output.contains("[exact cache]"), "got: {output}");
        assert!(output.contains("[incremental history]"), "got: {output}");
        assert!(
            output.contains(
                "Execution: 0 freshly tested, 1 exact-cache reused, 1 incremental-history reused"
            ),
            "got: {output}"
        );
        assert!(output.contains("Mutation score (fresh test kills only): 0.0%"));
    }

    #[test]
    fn terminal_output_marks_partial_early_stop_reports() {
        let mut report = report(vec![(mutation(1, "src/a.rs", 1), MutationResult::Survived)]);
        report.planned_total = 3;
        report.early_stop_reason = Some("--max-survivors 1 reached".into());

        let output = format_report_plain(&report);

        assert!(output.contains("Partial: stopped after 1/3 scheduled mutations"));
        assert!(output.contains("Early stop: --max-survivors 1 reached"));
    }

    #[test]
    fn terminal_output_contains_schemata_summary_when_present() {
        let mut report = report(vec![(mutation(1, "src/a.rs", 1), MutationResult::Killed)]);
        report.schemata = Some(crate::SchemataReport {
            fast_path: 1,
            fallback: 2,
            fallback_reasons: vec![crate::SchemataFallbackReasonCount {
                reason: "unsupported_operator".into(),
                count: 2,
            }],
        });

        let output = format_report_plain(&report);

        assert!(output.contains("Schemata: 1 fast-path, 2 fallback"));
        assert!(output.contains("Fallback reasons: unsupported_operator (2)"));
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

    #[test]
    fn reused_build_errors_do_not_show_fresh_build_error_guidance() {
        let mut report = report(vec![(
            mutation(1, "src/a.rs", 1),
            MutationResult::BuildError,
        )]);
        report
            .execution_provenance
            .insert(report.results[0].0.id, crate::MutationExecution::ExactCache);

        assert!(all_build_error_guidance(&report).is_none());
    }
    #[test]
    fn terminal_output_lists_uncovered_mutants_distinctly() {
        let report = report(vec![
            (mutation(1, "src/a.rs", 1), MutationResult::Killed),
            (mutation(2, "src/b.rs", 2), MutationResult::Uncovered),
        ]);
        let output = format_report_plain(&report);
        assert!(
            output
                .lines()
                .any(|line| line.contains("UNCOVERED") && line.contains("src/b.rs:2")),
            "got: {output}"
        );
        assert!(output.contains("Line has zero test coverage; mutant not executed."));
        assert!(
            output
                .contains("Results: 1 killed, 0 survived, 0 timeout, 0 build errors, 1 uncovered")
        );
        // Uncovered mutants are excluded from the tested denominator.
        assert!(output.contains("Mutation score (fresh test kills only): 100.0%"));
    }

    #[test]
    fn terminal_output_omits_uncovered_count_when_zero() {
        let report = report(vec![(mutation(1, "src/a.rs", 1), MutationResult::Killed)]);
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1 killed, 0 survived, 0 timeout, 0 build errors\n"));
        assert!(!output.contains("uncovered"));
    }

    #[test]
    fn terminal_output_lists_subsumed_mutants_distinctly() {
        let report = report(vec![
            (mutation(1, "src/a.rs", 1), MutationResult::Killed),
            (mutation(2, "src/b.rs", 2), MutationResult::Subsumed),
        ]);
        let output = format_report_plain(&report);
        assert!(
            output
                .lines()
                .any(|line| line.contains("SUBSUMED") && line.contains("src/b.rs:2")),
            "got: {output}"
        );
        assert!(output.contains("Same recorded killer test as an earlier mutant; not executed."));
        assert!(
            output.contains("Results: 1 killed, 0 survived, 0 timeout, 0 build errors, 1 subsumed")
        );
        // Subsumed mutants are excluded from the tested denominator.
        assert!(output.contains("Mutation score (fresh test kills only): 100.0%"));
    }

    #[test]
    fn terminal_output_omits_subsumed_count_when_zero() {
        let report = report(vec![(mutation(1, "src/a.rs", 1), MutationResult::Killed)]);
        let output = format_report_plain(&report);
        assert!(output.contains("Results: 1 killed, 0 survived, 0 timeout, 0 build errors\n"));
        assert!(!output.contains("subsumed"));
    }

    #[test]
    fn color_is_disabled_when_stdout_is_not_terminal() {
        assert!(!color_enabled_from_env(false, |_| None));
    }

    #[test]
    fn no_color_disables_color() {
        assert!(!color_enabled_from_env(true, |name| {
            (name == "NO_COLOR").then(|| "1".to_string())
        }));
    }

    #[test]
    fn clicolor_zero_disables_color() {
        assert!(!color_enabled_from_env(true, |name| {
            (name == "CLICOLOR").then(|| "0".to_string())
        }));
    }

    #[test]
    fn clicolor_force_overrides_no_color() {
        assert!(color_enabled_from_env(true, |name| match name {
            "NO_COLOR" => Some("1".to_string()),
            "CLICOLOR_FORCE" => Some("1".to_string()),
            _ => None,
        }));
    }

    #[test]
    fn clicolor_force_overrides_no_color_when_not_terminal() {
        assert!(color_enabled_from_env(false, |name| match name {
            "NO_COLOR" => Some("1".to_string()),
            "CLICOLOR_FORCE" => Some("1".to_string()),
            _ => None,
        }));
    }
}
