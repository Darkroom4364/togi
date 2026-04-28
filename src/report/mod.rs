pub mod github;
pub mod html;
pub mod json;
pub mod terminal;

use crate::{Mutation, MutationReport};
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
pub fn format_pr_comment(report: &MutationReport) -> String {
    use crate::MutationResult;
    use std::fmt::Write;

    let score = mutation_score(report);
    let tested = report.total.saturating_sub(report.build_errors);
    // Only ✅ when no survived AND no timeouts (timeouts could hide survivors)
    let emoji = if report.survived == 0 && report.timeout == 0 {
        "\u{2705}" // ✅
    } else if score >= 80.0 {
        "\u{26a0}\u{fe0f}" // ⚠️
    } else {
        "\u{274c}" // ❌
    };

    let mut md = String::new();
    writeln!(md, "<!-- togi-mutation-report -->").unwrap();
    writeln!(md, "## {emoji} togi mutation report").unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "**{score:.1}%** mutation score — {}/{tested} killed, {} survived, {} timeout, {} build errors — {:.2}s",
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
pub fn write_pr_comment(report: &MutationReport, path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let md = format_pr_comment(report);
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
    let mutated_line = format!(
        "{}{}{}",
        &original_line[..byte_start],
        mutation.replacement,
        &original_line[byte_end..]
    );

    let ctx = 1usize;
    let start = line_idx.saturating_sub(ctx);
    let end = (line_idx + ctx + 1).min(lines.len());

    let mut original_block = String::new();
    let mut mutated_block = String::new();
    for (i, line) in lines.iter().enumerate().take(end).skip(start) {
        if i == line_idx {
            original_block.push_str(original_line);
            original_block.push('\n');
            mutated_block.push_str(&mutated_line);
            mutated_block.push('\n');
        } else {
            original_block.push_str(line);
            original_block.push('\n');
            mutated_block.push_str(line);
            mutated_block.push('\n');
        }
    }

    let file_display = mutation.file.display().to_string();
    let original_name = format!("a/{}", file_display);
    let modified_name = format!("b/{}", file_display);
    let diff = diffy::DiffOptions::new()
        .set_context_len(ctx)
        .set_original_filename(original_name)
        .set_modified_filename(modified_name)
        .create_patch(&original_block, &mutated_block);

    Some(diff.to_string())
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
    fn pr_comment_contains_marker_and_score() {
        let report = crate::test_helpers::sample_report();
        let md = format_pr_comment(&report);
        assert!(md.contains("<!-- togi-mutation-report -->"));
        assert!(md.contains("50.0%"));
        assert!(md.contains("togi mutation report"));
    }

    #[test]
    fn pr_comment_lists_survived_mutations() {
        let report = crate::test_helpers::sample_report();
        let md = format_pr_comment(&report);
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
            duration: Duration::from_secs(1),
            total: 1,
            killed: 1,
            survived: 0,
            timeout: 0,
            build_errors: 0,
        };
        let md = format_pr_comment(&report);
        assert!(md.contains("✅"));
        assert!(!md.contains("<details>"));
    }
}
