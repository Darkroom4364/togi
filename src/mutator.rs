// Combine mapper + operators to generate mutations

use crate::mapper::find_mutable_nodes;
use crate::operators::{self};
use crate::{ChangedFile, Mutation, ts_row_to_line};
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
        let (tree, lang) = match crate::parser::parse_file(&changed_file.path, &source) {
            Ok(result) => result,
            Err(_) => {
                eprintln!(
                    "warning: skipping {} — unsupported language",
                    changed_file.path.display()
                );
                continue;
            }
        };
        let language_name = lang.name().to_string();

        let nodes = find_mutable_nodes(&tree, &source, &changed_file.hunks);

        for node in &nodes {
            for op in &operators {
                let candidates = op.apply(node, &source);
                for candidate in candidates {
                    if mutations.len() >= max_mutations {
                        return Ok(mutations);
                    }

                    let line = ts_row_to_line(node.start_position().row);
                    let column = node.start_position().column + 1; // 1-indexed for display
                    let original =
                        String::from_utf8_lossy(&source[candidate.byte_range.clone()]).to_string();

                    mutations.push(Mutation {
                        id: next_id,
                        file: file_path.clone(),
                        language: language_name.clone(),
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
        assert!(
            operators.contains(&"lt_to_lte"),
            "expected lt_to_lte, got: {:?}",
            operators
        );
        assert!(
            operators.contains(&"true_to_false"),
            "expected true_to_false, got: {:?}",
            operators
        );
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
    fn skips_nonexistent_files() {
        let tmp = TempDir::new().unwrap();
        // Reference a file that doesn't exist on disk
        let changed = vec![ChangedFile {
            path: PathBuf::from("ghost/missing.go"),
            hunks: vec![LineRange { start: 1, end: 5 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100).unwrap();
        assert!(mutations.is_empty());
    }

    #[test]
    fn generates_mutations_for_multiple_files() {
        let tmp = TempDir::new().unwrap();
        let go_src = "package main\n\nfunc f() bool {\n\treturn true\n}\n";
        let go_rel = write_go_file(tmp.path(), "a.go", go_src);

        let go_src2 = "package main\n\nfunc g(x, y int) bool {\n\treturn x < y\n}\n";
        let go_rel2 = write_go_file(tmp.path(), "b.go", go_src2);

        let changed = vec![
            ChangedFile {
                path: go_rel,
                hunks: vec![LineRange { start: 1, end: 5 }],
            },
            ChangedFile {
                path: go_rel2,
                hunks: vec![LineRange { start: 1, end: 5 }],
            },
        ];

        let mutations = generate_mutations(&changed, tmp.path(), 100).unwrap();
        let files: std::collections::HashSet<_> =
            mutations.iter().map(|m| m.file.clone()).collect();
        assert!(
            files.len() >= 2,
            "expected mutations from both files, got files: {:?}",
            files
        );
    }

    #[test]
    fn generates_mutations_for_python_file() {
        let tmp = TempDir::new().unwrap();
        let py_src = "def check(x, y):\n    return x < y\n";
        let rel = write_go_file(tmp.path(), "test.py", py_src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 2 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100).unwrap();
        assert!(!mutations.is_empty(), "expected mutations for Python file");
        assert_eq!(mutations[0].language, "python");
    }

    #[test]
    fn generates_mutations_for_rust_file() {
        let tmp = TempDir::new().unwrap();
        let rs_src = "fn check(x: i32, y: i32) -> bool {\n    x < y\n}\n";
        let rel = write_go_file(tmp.path(), "lib.rs", rs_src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 3 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100).unwrap();
        assert!(!mutations.is_empty(), "expected mutations for Rust file");
        assert_eq!(mutations[0].language, "rust");
    }

    #[test]
    fn mutation_ids_are_sequential() {
        let tmp = TempDir::new().unwrap();
        let src = "package main\n\nfunc f(x, y int) bool {\n\tif x < y {\n\t\treturn true\n\t}\n\treturn false\n}\n";
        let rel = write_go_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 8 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100).unwrap();
        assert!(!mutations.is_empty(), "expected at least one mutation");
        for (i, m) in mutations.iter().enumerate() {
            assert_eq!(m.id, i as u32, "mutation ids should be sequential");
        }
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
