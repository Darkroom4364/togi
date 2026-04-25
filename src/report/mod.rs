pub mod html;
pub mod json;
pub mod terminal;

use crate::{Mutation, MutationReport};
use std::fs;

/// Compute mutation score as a percentage, excluding build errors from the denominator.
pub fn mutation_score(report: &MutationReport) -> f64 {
    let tested = report.total - report.build_errors;
    if tested > 0 {
        (report.killed as f64 / tested as f64) * 100.0
    } else if report.total == 0 {
        100.0
    } else {
        0.0
    }
}

pub fn print_report(report: &crate::MutationReport, format: &str) -> anyhow::Result<()> {
    match format {
        "json" => json::print_report(report)?,
        "html" => {
            let path = std::path::Path::new("togi-report.html");
            html::write_report(report, path)?;
            eprintln!("HTML report written to {}", path.display());
        }
        _ => terminal::print_report(report),
    }
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
}
