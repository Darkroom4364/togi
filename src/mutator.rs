// Combine mapper + operators to generate mutations

use crate::mapper::find_mutable_nodes;
use crate::operators::{self};
use crate::{ChangedFile, Mutation};
use anyhow::Result;
use std::path::Path;

/// Generate mutations for all changed files.
/// Reads each file, parses it, finds mutable nodes in changed regions,
/// applies all operators, and returns concrete Mutation structs.
pub fn generate_mutations(
    changed_files: &[ChangedFile],
    project_root: &Path,
    max_mutations: usize,
) -> Result<Vec<Mutation>> {
    let operators = operators::all_operators();
    let mut mutations = Vec::new();
    let mut next_id: u32 = 0;

    for changed_file in changed_files {
        let file_path = project_root.join(&changed_file.path);
        if !file_path.exists() {
            continue;
        }

        let source = std::fs::read(&file_path)?;
        let (tree, _lang) = match crate::parser::parse_file(&changed_file.path, &source) {
            Ok(result) => result,
            Err(_) => continue, // Skip unsupported languages
        };

        let nodes = find_mutable_nodes(&tree, &source, &changed_file.hunks);

        for node in &nodes {
            for op in &operators {
                let candidates = op.apply(node, &source);
                for candidate in candidates {
                    if mutations.len() >= max_mutations {
                        return Ok(mutations);
                    }

                    let line = node.start_position().row + 1;
                    let column = node.start_position().column + 1;
                    let original = String::from_utf8_lossy(
                        &source[candidate.byte_range.clone()],
                    )
                    .to_string();

                    mutations.push(Mutation {
                        id: next_id,
                        file: file_path.clone(),
                        line,
                        column,
                        operator: candidate.operator_id,
                        description: candidate.description,
                        original,
                        replacement: candidate.replacement,
                        byte_range: candidate.byte_range,
                    });
                    next_id += 1;
                }
            }
        }
    }

    Ok(mutations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChangedFile, LineRange};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_go_file(dir: &Path, rel_path: &str, content: &str) -> PathBuf {
        let full = dir.join(rel_path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
        PathBuf::from(rel_path)
    }

    #[test]
    fn generates_mutations_for_go_file() {
        let tmp = TempDir::new().unwrap();
        // Use `x < y` (no literal children) so the mapper returns binary_expression
        let source = "package main\n\nfunc check(x, y int) bool {\n\tif x < y {\n\t\treturn true\n\t}\n\treturn false\n}\n";
        let rel = write_go_file(tmp.path(), "src/main.go", source);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 4, end: 5 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100).unwrap();
        assert!(!mutations.is_empty());

        let operators: Vec<&str> = mutations.iter().map(|m| m.operator.as_str()).collect();
        assert!(operators.contains(&"lt_to_lte"), "expected lt_to_lte, got: {:?}", operators);
        assert!(operators.contains(&"true_to_false"), "expected true_to_false, got: {:?}", operators);
    }

    #[test]
    fn respects_max_mutations() {
        let tmp = TempDir::new().unwrap();
        let source = "package main\n\nfunc check(x int) bool {\n\tif x < 10 {\n\t\treturn true\n\t}\n\treturn false\n}\n";
        let rel = write_go_file(tmp.path(), "main.go", source);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 8 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 2).unwrap();
        assert_eq!(mutations.len(), 2);
    }

    #[test]
    fn skips_unsupported_file_types() {
        let tmp = TempDir::new().unwrap();
        let rel = write_go_file(tmp.path(), "notes.txt", "hello world");

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 1 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100).unwrap();
        assert!(mutations.is_empty());
    }
}
