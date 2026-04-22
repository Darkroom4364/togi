pub mod json;
pub mod terminal;

use crate::Mutation;
use std::fs;

pub fn print_report(report: &crate::MutationReport, format: &str) -> anyhow::Result<()> {
    match format {
        "json" => json::print_report(report)?,
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
