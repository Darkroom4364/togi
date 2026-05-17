pub mod github;
pub mod html;
pub mod json;
pub mod terminal;

use crate::{BuildErrorDiagnostic, Mutation, MutationReport, MutationResult};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;

/// Compute mutation score as a percentage, excluding build errors from the denominator.
pub fn mutation_score(report: &MutationReport) -> f64 {
    let tested = report.total.saturating_sub(report.build_errors);
    if tested > 0 {
        (report.killed as f64 / tested as f64) * 100.0
    } else if report.total == 0 {
        100.0
    } else {
        0.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildErrorGroup {
    pub count: usize,
    pub language: String,
    pub operator: String,
    pub runner: String,
    pub phase: String,
    pub fingerprint: String,
    pub command: Vec<String>,
    pub message: String,
    pub files: Vec<BuildErrorFileCount>,
    pub examples: Vec<BuildErrorExample>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildErrorFileCount {
    pub file: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildErrorExample {
    pub mutation_id: u32,
    pub file: String,
    pub line: usize,
}

struct BuildErrorGroupAccumulator {
    count: usize,
    command: Vec<String>,
    message: String,
    files: BTreeMap<String, usize>,
    examples: Vec<BuildErrorExample>,
}

/// Group build errors into actionable buckets for report output.
pub fn build_error_groups(report: &MutationReport) -> Vec<BuildErrorGroup> {
    let diagnostics: BTreeMap<u32, &BuildErrorDiagnostic> = report
        .build_error_diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.mutation_id, diagnostic))
        .collect();
    let mut groups =
        BTreeMap::<(String, String, String, String, String), BuildErrorGroupAccumulator>::new();

    for (mutation, result) in &report.results {
        if *result != MutationResult::BuildError {
            continue;
        }

        let diagnostic = diagnostics.get(&mutation.id).copied();
        let language = label_or_unknown(&mutation.language);
        let operator = label_or_unknown(&mutation.operator);
        let runner = diagnostic
            .map(|diagnostic| diagnostic.runner.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let phase = diagnostic
            .map(|diagnostic| diagnostic.phase.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let message = diagnostic
            .map(|diagnostic| diagnostic.message.clone())
            .unwrap_or_else(|| "build error diagnostic unavailable".to_string());
        let fingerprint = diagnostic
            .map(|diagnostic| diagnostic.fingerprint.clone())
            .unwrap_or_else(|| BuildErrorDiagnostic::fingerprint_for(&message));
        let command = diagnostic
            .map(|diagnostic| diagnostic.command.clone())
            .unwrap_or_default();
        let key = (
            language.clone(),
            operator.clone(),
            runner.clone(),
            phase.clone(),
            fingerprint.clone(),
        );
        let accumulator = groups
            .entry(key)
            .or_insert_with(|| BuildErrorGroupAccumulator {
                count: 0,
                command,
                message,
                files: BTreeMap::new(),
                examples: Vec::new(),
            });

        accumulator.count += 1;
        *accumulator
            .files
            .entry(mutation.file.display().to_string())
            .or_default() += 1;
        if accumulator.examples.len() < 3 {
            accumulator.examples.push(BuildErrorExample {
                mutation_id: mutation.id + 1,
                file: mutation.file.display().to_string(),
                line: mutation.line,
            });
        }
    }

    let mut groups: Vec<_> = groups
        .into_iter()
        .map(
            |((language, operator, runner, phase, fingerprint), accumulator)| BuildErrorGroup {
                count: accumulator.count,
                language,
                operator,
                runner,
                phase,
                fingerprint,
                command: accumulator.command,
                message: accumulator.message,
                files: accumulator
                    .files
                    .into_iter()
                    .map(|(file, count)| BuildErrorFileCount { file, count })
                    .collect(),
                examples: accumulator.examples,
            },
        )
        .collect();
    groups.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.language.cmp(&right.language))
            .then_with(|| left.operator.cmp(&right.operator))
            .then_with(|| left.runner.cmp(&right.runner))
            .then_with(|| left.phase.cmp(&right.phase))
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    groups
}

fn label_or_unknown(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn print_report(
    report: &crate::MutationReport,
    format: crate::cli::OutputFormat,
) -> anyhow::Result<()> {
    use crate::cli::OutputFormat;
    match format {
        OutputFormat::Json => json::print_report(report)?,
        OutputFormat::Github => github::print_report(report),
        OutputFormat::Html => {
            let path = std::path::Path::new("togi-report.html");
            html::write_report(report, path)?;
            eprintln!("HTML report written to {}", path.display());
        }
        OutputFormat::Terminal => terminal::print_report(report),
    }
    Ok(())
}

/// Generate a markdown PR comment summarizing mutation results.
///
/// Includes a hidden marker comment so CI pipelines can find/replace
/// existing togi comments on subsequent runs.
pub fn format_pr_comment(report: &MutationReport, baseline_score: Option<f64>) -> String {
    use crate::MutationResult;
    use std::fmt::Write;

    let score = mutation_score(report);
    let tested = report.total.saturating_sub(report.build_errors);
    let emoji = if report.survived == 0 && report.timeout == 0 && report.build_errors == 0 {
        "✓"
    } else if score >= 80.0 {
        "⚠"
    } else {
        "✗"
    };

    let mut md = String::new();
    writeln!(md, "<!-- togi-mutation-report -->").unwrap();
    writeln!(md, "## {emoji} togi mutation report").unwrap();
    writeln!(md).unwrap();
    let delta_str = if let Some(base) = baseline_score {
        let delta = score - base;
        let sign = if delta >= 0.0 { "+" } else { "" };
        format!(" ({sign}{delta:.1}% vs baseline)")
    } else {
        String::new()
    };
    writeln!(
        md,
        "**{score:.1}%** mutation score{delta_str} — {}/{tested} killed, {} survived, {} timeout, {} build errors — {:.2}s",
        report.killed, report.survived, report.timeout, report.build_errors, report.duration.as_secs_f64()
    ).unwrap();
    writeln!(md).unwrap();

    let survived: Vec<_> = report
        .results
        .iter()
        .filter(|(_, r)| *r == MutationResult::Survived)
        .collect();

    if !survived.is_empty() {
        writeln!(md, "<details>").unwrap();
        writeln!(
            md,
            "<summary>{} survived mutation{}</summary>",
            survived.len(),
            if survived.len() == 1 { "" } else { "s" }
        )
        .unwrap();
        writeln!(md).unwrap();
        writeln!(md, "| File | Line | Operator | Description |").unwrap();
        writeln!(md, "|------|------|----------|-------------|").unwrap();
        for (m, _) in &survived {
            writeln!(
                md,
                "| `{}` | {} | `{}` | {} |",
                escape_md_cell(&m.file.display().to_string()),
                m.line,
                escape_md_cell(&m.operator),
                escape_md_cell(&m.description)
            )
            .unwrap();
        }
        writeln!(md).unwrap();
        writeln!(md, "</details>").unwrap();
    }

    md
}

/// Escape characters that break markdown table cells.
fn escape_md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").replace('\r', "")
}

/// Write a PR comment markdown file.
pub fn write_pr_comment(
    report: &MutationReport,
    path: &std::path::Path,
    baseline_score: Option<f64>,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let md = format_pr_comment(report, baseline_score);
    fs::write(path, md)?;
    Ok(())
}

/// Generate a unified diff snippet for a survived mutation.
///
/// Reads the source file, extracts context around the mutated line,
/// and returns a unified diff string showing original vs mutated code.
pub fn mutation_diff(mutation: &Mutation) -> Option<String> {
    let content = fs::read_to_string(&mutation.file).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let line_idx = mutation.line.checked_sub(1)?;
    if line_idx >= lines.len() {
        return None;
    }

    let original_line = lines[line_idx];
    let col_idx = mutation.column.saturating_sub(1);
    let byte_start = col_idx.min(original_line.len());
    let byte_end = byte_start + mutation.original.len();
    if byte_end > original_line.len() {
        return None;
    }
    if !original_line.is_char_boundary(byte_start) || !original_line.is_char_boundary(byte_end) {
        return None;
    }
    if &original_line[byte_start..byte_end] != mutation.original.as_str() {
        return None;
    }
    let mutated_line = format!(
        "{}{}{}",
        &original_line[..byte_start],
        mutation.replacement,
        &original_line[byte_end..]
    );

    let ctx = 1usize;
    let start = line_idx.saturating_sub(ctx);
    let end = (line_idx + ctx + 1).min(lines.len());

    let file_display = mutation.file.display().to_string();
    let hunk_start = start + 1;
    let hunk_len = end - start;
    let mut diff = String::new();
    writeln!(diff, "--- a/{file_display}").ok()?;
    writeln!(diff, "+++ b/{file_display}").ok()?;
    writeln!(
        diff,
        "@@ -{hunk_start},{hunk_len} +{hunk_start},{hunk_len} @@"
    )
    .ok()?;
    for (i, line) in lines.iter().enumerate().take(end).skip(start) {
        if i == line_idx {
            writeln!(diff, "-{original_line}").ok()?;
            writeln!(diff, "+{mutated_line}").ok()?;
        } else {
            writeln!(diff, " {line}").ok()?;
        }
    }

    Some(diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_mutation(
        dir: &std::path::Path,
        filename: &str,
        content: &str,
        line: usize,
        column: usize,
        original: &str,
        replacement: &str,
    ) -> Mutation {
        let path = dir.join(filename);
        std::fs::write(&path, content).unwrap();
        Mutation {
            id: 0,
            file: path,
            language: "go".into(),
            line,
            column,
            operator: "test_op".into(),
            description: "test".into(),
            original: original.into(),
            replacement: replacement.into(),
            byte_range: 0..0,
        }
    }

    fn report_mutation(id: u32, file: &str, result: MutationResult) -> (Mutation, MutationResult) {
        (
            Mutation {
                id,
                file: std::path::PathBuf::from(file),
                language: "rust".into(),
                line: usize::try_from(id + 1).unwrap(),
                column: 1,
                operator: "eq_to_neq".into(),
                description: "Replace == with !=".into(),
                original: "==".into(),
                replacement: "!=".into(),
                byte_range: 0..2,
            },
            result,
        )
    }

    #[test]
    fn build_error_groups_deduplicate_by_diagnostic_fingerprint() {
        let report = MutationReport {
            results: vec![
                report_mutation(0, "src/a.rs", MutationResult::BuildError),
                report_mutation(1, "src/b.rs", MutationResult::BuildError),
                report_mutation(2, "src/c.rs", MutationResult::Killed),
            ],
            build_error_diagnostics: vec![
                BuildErrorDiagnostic::new(
                    0,
                    "regular",
                    "build_command",
                    vec!["cargo".into(), "check".into()],
                    "error[E0308]: mismatched types at line 10",
                ),
                BuildErrorDiagnostic::new(
                    1,
                    "regular",
                    "build_command",
                    vec!["cargo".into(), "check".into()],
                    "error[E0308]: mismatched types at line 20",
                ),
            ],
            duration: std::time::Duration::from_millis(10),
            test_command: None,
            build_command: vec![],
            total: 3,
            killed: 1,
            survived: 0,
            timeout: 0,
            build_errors: 2,
        };

        let groups = build_error_groups(&report);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[0].language, "rust");
        assert_eq!(groups[0].operator, "eq_to_neq");
        assert_eq!(groups[0].runner, "regular");
        assert_eq!(groups[0].phase, "build_command");
        assert_eq!(groups[0].files.len(), 2);
        assert_eq!(groups[0].examples.len(), 2);
    }

    #[test]
    fn mutation_diff_basic() {
        let tmp = TempDir::new().unwrap();
        let content = "package main\n\nfunc f() bool {\n\treturn true\n}\n";
        let m = make_mutation(tmp.path(), "main.go", content, 4, 9, "true", "false");
        let diff = mutation_diff(&m).unwrap();
        assert!(
            diff.contains("-\treturn true"),
            "diff should show original line: {diff}"
        );
        assert!(
            diff.contains("+\treturn false"),
            "diff should show mutated line: {diff}"
        );
    }

    #[test]
    fn mutation_diff_empty_replacement() {
        let tmp = TempDir::new().unwrap();
        let content = "x = 1 + 2\ny = 3\n";
        let m = make_mutation(tmp.path(), "test.py", content, 1, 7, "+ 2", "");
        let diff = mutation_diff(&m).unwrap();
        assert!(
            diff.contains("-x = 1 + 2"),
            "diff should show original: {diff}"
        );
        assert!(diff.contains("+x = 1"), "diff should show removal: {diff}");
    }

    #[test]
    fn mutation_diff_replacement_at_start() {
        let tmp = TempDir::new().unwrap();
        let content = "true\n";
        let m = make_mutation(tmp.path(), "t.go", content, 1, 1, "true", "false");
        let diff = mutation_diff(&m).unwrap();
        assert!(diff.contains("-true"), "{diff}");
        assert!(diff.contains("+false"), "{diff}");
    }

    #[test]
    fn mutation_diff_replacement_at_end() {
        let tmp = TempDir::new().unwrap();
        let content = "a = true\n";
        let m = make_mutation(tmp.path(), "t.py", content, 1, 5, "true", "false");
        let diff = mutation_diff(&m).unwrap();
        assert!(diff.contains("-a = true"), "{diff}");
        assert!(diff.contains("+a = false"), "{diff}");
    }

    #[test]
    fn mutation_diff_invalid_line_returns_none() {
        let tmp = TempDir::new().unwrap();
        let content = "one line\n";
        let m = make_mutation(tmp.path(), "t.go", content, 99, 1, "x", "y");
        assert!(mutation_diff(&m).is_none());
    }

    #[test]
    fn mutation_diff_line_zero_returns_none() {
        let tmp = TempDir::new().unwrap();
        let content = "hello\n";
        let m = make_mutation(tmp.path(), "t.go", content, 0, 1, "h", "H");
        assert!(mutation_diff(&m).is_none());
    }

    #[test]
    fn mutation_diff_multibyte_chars() {
        let tmp = TempDir::new().unwrap();
        // "日本語 = true\n" — CJK chars are 3 bytes each
        let content = "日本語 = true\n";
        // "true" starts at byte offset 12 (9 bytes for CJK + 3 for " = ")
        // column is 1-indexed byte offset: 13
        let m = make_mutation(tmp.path(), "t.rs", content, 1, 13, "true", "false");
        let diff = mutation_diff(&m).unwrap();
        assert!(diff.contains("-日本語 = true"), "{diff}");
        assert!(diff.contains("+日本語 = false"), "{diff}");
    }

    #[test]
    fn mutation_diff_invalid_char_boundary_returns_none() {
        let tmp = TempDir::new().unwrap();
        let content = "日本語\n";
        // column 2 (1-indexed) → byte_start 1, which is mid-character
        let m = make_mutation(tmp.path(), "t.rs", content, 1, 2, "x", "y");
        assert!(mutation_diff(&m).is_none());
    }

    #[test]
    fn mutation_diff_original_mismatch_returns_none() {
        let tmp = TempDir::new().unwrap();
        let content = "a = true\n";
        let m = make_mutation(tmp.path(), "t.rs", content, 1, 5, "false", "true");
        assert!(mutation_diff(&m).is_none());
    }

    #[test]
    fn pr_comment_contains_marker_and_score() {
        let report = crate::test_helpers::sample_report();
        let md = format_pr_comment(&report, None);
        assert!(md.contains("<!-- togi-mutation-report -->"));
        assert!(md.contains("50.0%"));
        assert!(md.contains("togi mutation report"));
    }

    #[test]
    fn pr_comment_lists_survived_mutations() {
        let report = crate::test_helpers::sample_report();
        let md = format_pr_comment(&report, None);
        assert!(md.contains("survived mutation"));
        assert!(md.contains("src/handler.rs"));
    }

    #[test]
    fn pr_comment_no_details_when_all_killed() {
        use crate::{MutationReport, MutationResult};
        use std::time::Duration;
        let report = MutationReport {
            results: vec![(
                Mutation {
                    id: 0,
                    file: std::path::PathBuf::from("test.rs"),
                    language: "rust".into(),
                    line: 1,
                    column: 1,
                    operator: "op".into(),
                    description: "d".into(),
                    original: "x".into(),
                    replacement: "y".into(),
                    byte_range: 0..1,
                },
                MutationResult::Killed,
            )],
            build_error_diagnostics: vec![],
            duration: Duration::from_secs(1),
            test_command: None,
            build_command: vec![],
            total: 1,
            killed: 1,
            survived: 0,
            timeout: 0,
            build_errors: 0,
        };
        let md = format_pr_comment(&report, None);
        assert!(md.contains("✓"));
        assert!(!md.contains("<details>"));
    }

    #[test]
    fn pr_comment_no_checkmark_when_timeouts() {
        use crate::{MutationReport, MutationResult};
        use std::time::Duration;
        let report = MutationReport {
            results: vec![(
                Mutation {
                    id: 0,
                    file: std::path::PathBuf::from("test.rs"),
                    language: "rust".into(),
                    line: 1,
                    column: 1,
                    operator: "op".into(),
                    description: "d".into(),
                    original: "x".into(),
                    replacement: "y".into(),
                    byte_range: 0..1,
                },
                MutationResult::Timeout,
            )],
            build_error_diagnostics: vec![],
            duration: Duration::from_secs(1),
            test_command: None,
            build_command: vec![],
            total: 1,
            killed: 0,
            survived: 0,
            timeout: 1,
            build_errors: 0,
        };
        let md = format_pr_comment(&report, None);
        assert!(!md.contains("✓"), "should not show checkmark with timeouts");
        assert!(md.contains("1 timeout"));
        assert!(!md.contains("<details>"));
    }

    #[test]
    fn pr_comment_no_checkmark_when_all_build_errors() {
        use crate::{MutationReport, MutationResult};
        use std::time::Duration;
        let report = MutationReport {
            results: vec![(
                Mutation {
                    id: 0,
                    file: std::path::PathBuf::from("test.rs"),
                    language: "rust".into(),
                    line: 1,
                    column: 1,
                    operator: "op".into(),
                    description: "d".into(),
                    original: "x".into(),
                    replacement: "y".into(),
                    byte_range: 0..1,
                },
                MutationResult::BuildError,
            )],
            build_error_diagnostics: vec![],
            duration: Duration::from_secs(1),
            test_command: None,
            build_command: vec![],
            total: 1,
            killed: 0,
            survived: 0,
            timeout: 0,
            build_errors: 1,
        };
        let md = format_pr_comment(&report, None);
        assert!(
            !md.contains("✓"),
            "should not show checkmark when every mutation is a build error"
        );
        assert!(md.contains("## ✗ togi mutation report"));
        assert!(md.contains("1 build errors"));
    }

    #[test]
    fn pr_comment_no_checkmark_when_mixed_build_errors() {
        use crate::{MutationReport, MutationResult};
        use std::time::Duration;
        let report = MutationReport {
            results: vec![
                (
                    Mutation {
                        id: 0,
                        file: std::path::PathBuf::from("test.rs"),
                        language: "rust".into(),
                        line: 1,
                        column: 1,
                        operator: "op".into(),
                        description: "d".into(),
                        original: "x".into(),
                        replacement: "y".into(),
                        byte_range: 0..1,
                    },
                    MutationResult::Killed,
                ),
                (
                    Mutation {
                        id: 1,
                        file: std::path::PathBuf::from("test.rs"),
                        language: "rust".into(),
                        line: 2,
                        column: 1,
                        operator: "op".into(),
                        description: "d".into(),
                        original: "x".into(),
                        replacement: "y".into(),
                        byte_range: 0..1,
                    },
                    MutationResult::BuildError,
                ),
            ],
            build_error_diagnostics: vec![],
            duration: Duration::from_secs(1),
            test_command: None,
            build_command: vec![],
            total: 2,
            killed: 1,
            survived: 0,
            timeout: 0,
            build_errors: 1,
        };
        let md = format_pr_comment(&report, None);
        assert!(
            !md.contains("✓"),
            "should not show checkmark when build errors remain"
        );
        assert!(md.contains("## ⚠ togi mutation report"));
        assert!(md.contains("100.0%"));
        assert!(md.contains("1 build errors"));
    }

    #[test]
    fn pr_comment_includes_baseline_delta() {
        let report = crate::test_helpers::sample_report();
        let md = format_pr_comment(&report, Some(40.0));
        assert!(md.contains("vs baseline"), "should include baseline delta");
        assert!(md.contains("+10.0%"), "50% - 40% = +10%");
    }
}
