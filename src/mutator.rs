// Combine mapper + operators to generate mutations

use crate::mapper::find_mutable_nodes;
use crate::operators::{self};
use crate::{ChangedFile, Mutation};
use anyhow::{Context, Result};
use std::path::Path;

/// Convert a byte offset in source to (line, column), both 1-indexed.
fn byte_offset_to_line_col(source: &[u8], offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for &b in source.iter().take(offset) {
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Generate concrete mutations for changed files.
///
/// Each changed file is parsed with tree-sitter, mapped to mutable AST nodes,
/// passed through the selected operators, filtered by language-specific rules,
/// and converted into [`Mutation`] values with stable ids and byte ranges.
/// `max_mutations` caps the total output; `max_per_file` caps each file before
/// global ids are assigned. A value of `0` for `max_per_file` means unlimited.
pub fn generate_mutations(
    changed_files: &[ChangedFile],
    project_root: &Path,
    max_mutations: usize,
    max_per_file: usize,
    operator_filters: &[String],
) -> Result<Vec<Mutation>> {
    let all = operators::all_operators();
    if !operator_filters.is_empty() {
        operators::validate_patterns(&all, operator_filters).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    let operators = operators::filter_operators(all, operator_filters);
    let mut mutations = Vec::new();
    let mut next_id: u32 = 0;
    let mut parser = tree_sitter::Parser::new();

    for changed_file in changed_files {
        // Resolve through the shared source-identity helpers so only paths
        // contained in the project root (after symlink resolution) are read.
        let Some(normalized) = crate::source_identity::normalized_project_relative_path(
            project_root,
            &changed_file.path,
        ) else {
            eprintln!(
                "warning: skipping {} — path is not project-relative",
                changed_file.path.display()
            );
            continue;
        };
        let Some(file_path) = crate::source_identity::resolve_normalized_project_relative_path(
            project_root,
            &normalized,
        ) else {
            eprintln!(
                "warning: skipping {} — path does not resolve inside the project root",
                changed_file.path.display()
            );
            continue;
        };

        if file_path.is_dir() {
            eprintln!(
                "warning: skipping {} — path is a directory",
                changed_file.path.display()
            );
            continue;
        }
        let source = std::fs::read(&file_path).with_context(|| {
            format!(
                "could not read mutation source {}",
                changed_file.path.display()
            )
        })?;
        let (tree, lang) =
            match crate::parser::parse_file_with_parser(&mut parser, &changed_file.path, &source) {
                Ok(result) => result,
                Err(err) => {
                    eprintln!(
                        "warning: skipping {} — {}",
                        changed_file.path.display(),
                        err
                    );
                    continue;
                }
            };
        let nodes = find_mutable_nodes(&tree, &source, &changed_file.hunks, lang.as_ref());

        let mut file_mutations = Vec::new();

        for node in &nodes {
            for op in &operators {
                let candidates = op.apply(node, &source, lang.as_ref());
                for mut candidate in candidates {
                    if lang.should_filter_candidate(&candidate, node, &source) {
                        continue;
                    }
                    lang.fixup_replacement(&mut candidate);

                    if candidate.byte_range.start > candidate.byte_range.end
                        || candidate.byte_range.end > source.len()
                    {
                        continue;
                    }

                    // Candidates from a mapped AST node may target a narrower
                    // child token, so filter by the candidate's own source span.
                    let (line, column) =
                        byte_offset_to_line_col(&source, candidate.byte_range.start);
                    let end_line = if candidate.byte_range.is_empty() {
                        line
                    } else {
                        line + source[candidate.byte_range.start..candidate.byte_range.end - 1]
                            .iter()
                            .filter(|&&byte| byte == b'\n')
                            .count()
                    };
                    if !crate::mapper::overlaps(line, end_line, &changed_file.hunks) {
                        continue;
                    }
                    // The original bytes must survive losslessly; reject
                    // candidates whose range is not valid UTF-8 (e.g. splits a
                    // multi-byte character) instead of lossy conversion.
                    let Ok(original) = std::str::from_utf8(&source[candidate.byte_range.clone()])
                    else {
                        continue;
                    };

                    file_mutations.push(Mutation {
                        id: 0,
                        file: changed_file.path.clone(),
                        language: lang.name().to_string(),
                        line,
                        column,
                        operator: candidate.operator_id,
                        description: candidate.description,
                        original: original.to_string(),
                        replacement: candidate.replacement,
                        byte_range: candidate.byte_range,
                    });
                }
            }
        }

        if max_per_file > 0 && file_mutations.len() > max_per_file {
            file_mutations = sample_diverse(file_mutations, max_per_file);
        }

        for mut m in file_mutations {
            if mutations.len() >= max_mutations {
                return Ok(mutations);
            }
            m.id = next_id;
            next_id += 1;
            mutations.push(m);
        }
    }

    Ok(mutations)
}

/// Sample up to `cap` mutations with operator diversity via deterministic round-robin.
/// Groups by operator name (sorted for reproducibility), shuffles within each group
/// to avoid positional bias, then round-robins across groups.
fn sample_diverse(mutations: Vec<Mutation>, cap: usize) -> Vec<Mutation> {
    use std::collections::BTreeMap;

    let mut by_operator: BTreeMap<String, Vec<Mutation>> = BTreeMap::new();
    for m in mutations {
        by_operator.entry(m.operator.clone()).or_default().push(m);
    }

    // Shuffle within each group: interleave from front and back
    // to break positional clustering (deterministic, no RNG needed).
    for group in by_operator.values_mut() {
        if group.len() > 2 {
            let mut front: Vec<Mutation> = Vec::new();
            let mut back: Vec<Mutation> = Vec::new();
            let mid = group.len() / 2;
            let taken = std::mem::take(group);
            for (i, m) in taken.into_iter().enumerate() {
                if i < mid {
                    front.push(m);
                } else {
                    back.push(m);
                }
            }
            back.reverse();
            let mut shuffled = Vec::with_capacity(front.len() + back.len());
            let mut fi = front.into_iter();
            let mut bi = back.into_iter();
            loop {
                match (fi.next(), bi.next()) {
                    (Some(a), Some(b)) => {
                        shuffled.push(a);
                        shuffled.push(b);
                    }
                    (Some(a), None) => shuffled.push(a),
                    (None, Some(b)) => shuffled.push(b),
                    (None, None) => break,
                }
            }
            *group = shuffled;
        }
    }

    let mut result = Vec::with_capacity(cap);
    let mut iters: Vec<std::vec::IntoIter<Mutation>> =
        by_operator.into_values().map(|v| v.into_iter()).collect();

    while result.len() < cap && !iters.is_empty() {
        iters.retain_mut(|iter| {
            if result.len() >= cap {
                return false;
            }
            if let Some(m) = iter.next() {
                result.push(m);
                true
            } else {
                false
            }
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;
    use crate::languages::go::Go;
    use crate::test_helpers::{find_node_by_kind, parse_go, parse_python, parse_rust};
    use crate::{ChangedFile, LineRange, MutationCandidate};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_test_file(dir: &Path, rel_path: &str, content: &str) -> PathBuf {
        let full = dir.join(rel_path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
        PathBuf::from(rel_path)
    }

    fn candidate(operator_id: &str) -> MutationCandidate {
        MutationCandidate {
            byte_range: 0..1,
            replacement: String::new(),
            operator_id: operator_id.to_string(),
            description: String::new(),
        }
    }

    fn apply_mutation(source: &str, mutation: &Mutation) -> String {
        let mut mutated = source.as_bytes().to_vec();
        mutated.splice(
            mutation.byte_range.clone(),
            mutation.replacement.as_bytes().iter().copied(),
        );
        String::from_utf8(mutated).unwrap()
    }

    fn assert_rust_library_compiles(source: &str) {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("lib.rs");
        std::fs::write(&source_path, source).unwrap();
        let output = std::process::Command::new("rustc")
            .args(["--crate-type=lib", "--emit=metadata"])
            .arg(&source_path)
            .arg("-o")
            .arg(dir.path().join("lib.rmeta"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "mutated Rust source must compile:\n{source}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn mul_to_div_filter_skips_right_hand_one() {
        let lang = Go;
        let src = "package main\nfunc f(x int) int { return x * 1 }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression")
            .expect("should find binary_expression node");

        assert!(lang.should_filter_candidate(&candidate("mul_to_div"), &bin, src.as_bytes()));
    }

    #[test]
    fn mul_to_div_filter_allows_left_hand_one() {
        let lang = Go;
        let src = "package main\nfunc f(x int) int { return 1 * x }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression")
            .expect("should find binary_expression node");

        assert!(!lang.should_filter_candidate(&candidate("mul_to_div"), &bin, src.as_bytes()));
    }

    #[test]
    fn div_to_mul_filter_skips_right_hand_one() {
        let lang = Go;
        let src = "package main\nfunc f(x int) int { return x / 1 }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression")
            .expect("should find binary_expression node");

        assert!(lang.should_filter_candidate(&candidate("div_to_mul"), &bin, src.as_bytes()));
    }

    #[test]
    fn string_to_empty_absent_for_go_const_declaration() {
        let tmp = TempDir::new().unwrap();
        let src = "package main\n\nconst name = \"togi\"\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 3, end: 3 }],
        }];

        let mutations =
            generate_mutations(&changed, tmp.path(), 100, 0, &["string_to_empty".into()]).unwrap();

        assert!(
            mutations.is_empty(),
            "string_to_empty should be skipped for const declarations, got: {:?}",
            mutations.iter().map(|m| &m.operator).collect::<Vec<_>>()
        );
    }

    #[test]
    fn string_to_empty_present_for_go_function_body() {
        let tmp = TempDir::new().unwrap();
        let src = "package main\n\nfunc f() string {\n\treturn \"togi\"\n}\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 4, end: 4 }],
        }];

        let mutations =
            generate_mutations(&changed, tmp.path(), 100, 0, &["string_to_empty".into()]).unwrap();

        assert!(
            mutations.iter().any(|m| m.operator == "string_to_empty"),
            "string_to_empty should be allowed in function bodies, got: {:?}",
            mutations.iter().map(|m| &m.operator).collect::<Vec<_>>()
        );
    }

    #[test]
    fn skips_changed_files_escaping_project_root() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let outside_src = "package main\n\nfunc f() int {\n\treturn 1 + 2\n}\n";
        let outside = tmp.path().join("outside.go");
        std::fs::write(&outside, outside_src).unwrap();

        let changed = vec![ChangedFile {
            path: PathBuf::from("../outside.go"),
            hunks: vec![LineRange { start: 4, end: 4 }],
        }];
        let mutations = generate_mutations(&changed, &project, 100, 0, &[]).unwrap();

        assert!(
            mutations.is_empty(),
            "path escaping the project root must be skipped before any read"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            outside_src,
            "outside file must remain untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn skips_changed_files_resolving_through_symlink_escape() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let outside_src = "package main\n\nfunc f() int {\n\treturn 1 + 2\n}\n";
        let outside = tmp.path().join("outside.go");
        std::fs::write(&outside, outside_src).unwrap();
        std::os::unix::fs::symlink(&outside, project.join("link.go")).unwrap();

        let changed = vec![ChangedFile {
            path: PathBuf::from("link.go"),
            hunks: vec![LineRange { start: 4, end: 4 }],
        }];
        let mutations = generate_mutations(&changed, &project, 100, 0, &[]).unwrap();

        assert!(
            mutations.is_empty(),
            "symlink resolving outside the project root must be skipped"
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), outside_src);
    }

    #[test]
    fn mutation_originals_match_source_bytes_losslessly() {
        // Unicode and CRLF content: every emitted `original` must be the exact
        // source slice for its byte range — never a lossy conversion.
        let tmp = TempDir::new().unwrap();
        let src = "package main\r\n\r\nfunc f() string {\r\n\tname := \"héllo wörld\"\r\n\tif name == \"x\" {\r\n\t\treturn name\r\n\t}\r\n\treturn \"\"\r\n}\r\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 9 }],
        }];
        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();

        assert!(!mutations.is_empty(), "expected mutations for {src:?}");
        for m in &mutations {
            assert_eq!(
                &src.as_bytes()[m.byte_range.clone()],
                m.original.as_bytes(),
                "original must be the lossless source slice for {m:?}"
            );
        }
    }

    #[test]
    fn python_remove_if_body_uses_pass_replacement() {
        let tmp = TempDir::new().unwrap();
        let src = "def f(x):\n    if x:\n        return 1\n    return 0\n";
        let rel = write_test_file(tmp.path(), "test.py", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 2, end: 3 }],
        }];

        let mutations =
            generate_mutations(&changed, tmp.path(), 100, 0, &["remove_if_body".into()]).unwrap();
        let m = mutations
            .iter()
            .find(|m| m.operator == "remove_if_body")
            .expect("remove_if_body mutation should be generated");

        assert_eq!(m.replacement, "pass");
    }

    #[test]
    fn python_boolean_literal_replacements_use_python_syntax() {
        let tmp = TempDir::new().unwrap();
        let src = "def check(x):\n    if x > 0:\n        return True\n    return False\n";
        let rel = write_test_file(tmp.path(), "test.py", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 4 }],
        }];

        let mutations = generate_mutations(
            &changed,
            tmp.path(),
            100,
            0,
            &["true_to_false".into(), "false_to_true".into()],
        )
        .unwrap();

        assert!(
            mutations.iter().any(|m| m.operator == "true_to_false"
                && m.original == "True"
                && m.replacement == "False"),
            "expected Python True -> False mutation, got: {mutations:?}"
        );
        assert!(
            mutations.iter().any(|m| m.operator == "false_to_true"
                && m.original == "False"
                && m.replacement == "True"),
            "expected Python False -> True mutation, got: {mutations:?}"
        );

        for mutation in &mutations {
            let mutated = apply_mutation(src, mutation);
            let tree = parse_python(&mutated);
            assert!(
                !tree.root_node().has_error(),
                "mutation should parse as Python:\n{mutated}"
            );
        }
    }

    #[test]
    fn python_negate_condition_uses_python_syntax() {
        let tmp = TempDir::new().unwrap();
        let src = "def check(x):\n    if x > 0:\n        return True\n    return False\n";
        let rel = write_test_file(tmp.path(), "test.py", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 2, end: 2 }],
        }];

        let mutations =
            generate_mutations(&changed, tmp.path(), 100, 0, &["negate_condition".into()]).unwrap();
        let mutation = mutations
            .iter()
            .find(|m| m.operator == "negate_condition")
            .expect("negate_condition mutation should be generated");

        assert_eq!(mutation.original, "x > 0");
        assert_eq!(mutation.replacement, "not (x > 0)");

        let mutated = apply_mutation(src, mutation);
        let tree = parse_python(&mutated);
        assert!(
            !tree.root_node().has_error(),
            "mutation should parse as Python:\n{mutated}"
        );
    }

    #[test]
    fn go_negate_condition_keeps_c_family_syntax() {
        let tmp = TempDir::new().unwrap();
        let src = "package main\n\nfunc check(x int) bool {\n\tif x > 0 {\n\t\treturn true\n\t}\n\treturn false\n}\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 4, end: 4 }],
        }];

        let mutations =
            generate_mutations(&changed, tmp.path(), 100, 0, &["negate_condition".into()]).unwrap();
        let mutation = mutations
            .iter()
            .find(|m| m.operator == "negate_condition")
            .expect("negate_condition mutation should be generated");

        assert_eq!(mutation.original, "x > 0");
        assert_eq!(mutation.replacement, "!(x > 0)");

        let mutated = apply_mutation(src, mutation);
        let tree = parse_go(&mutated);
        assert!(
            !tree.root_node().has_error(),
            "mutation should parse as Go:\n{mutated}"
        );
    }

    #[test]
    fn ruby_remove_if_body_uses_nil_replacement() {
        let tmp = TempDir::new().unwrap();
        let src = "def f(x)\n  if x\n    1\n  end\nend\n";
        let rel = write_test_file(tmp.path(), "test.rb", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 2, end: 2 }],
        }];

        let mutations =
            generate_mutations(&changed, tmp.path(), 100, 0, &["remove_if_body".into()]).unwrap();
        let m = mutations
            .iter()
            .find(|m| m.operator == "remove_if_body")
            .expect("remove_if_body mutation should be generated");

        assert_eq!(m.replacement, "nil");
    }

    #[test]
    fn generates_mutations_for_go_file() {
        let tmp = TempDir::new().unwrap();
        // Use `x < y` (no literal children) so the mapper returns binary_expression
        let source = "package main\n\nfunc check(x, y int) bool {\n\tif x < y {\n\t\treturn true\n\t}\n\treturn false\n}\n";
        let rel = write_test_file(tmp.path(), "src/main.go", source);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 4, end: 5 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
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
        let rel = write_test_file(tmp.path(), "main.go", source);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 8 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 2, 0, &[]).unwrap();
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

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        assert!(mutations.is_empty());
    }

    #[test]
    fn generates_mutations_for_multiple_files() {
        let tmp = TempDir::new().unwrap();
        let go_src = "package main\n\nfunc f() bool {\n\treturn true\n}\n";
        let go_rel = write_test_file(tmp.path(), "a.go", go_src);

        let go_src2 = "package main\n\nfunc g(x, y int) bool {\n\treturn x < y\n}\n";
        let go_rel2 = write_test_file(tmp.path(), "b.go", go_src2);

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

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        let files: std::collections::HashSet<_> =
            mutations.iter().map(|m| m.file.clone()).collect();
        assert!(
            files.iter().all(|file| file.is_relative()),
            "generated mutation paths should stay project-relative: {:?}",
            files
        );
        assert!(
            files.len() >= 2,
            "expected mutations from both files, got files: {:?}",
            files
        );
    }

    #[test]
    fn generates_python_predicate_operator_mutations() {
        let tmp = TempDir::new().unwrap();
        let py_src = concat!(
            "def check(low, value, high, left, right, items):\n",
            "    chained = low < value < high\n",
            "    greater = left > right\n",
            "    equal = left == right\n",
            "    conjunction = left and right\n",
            "    disjunction = left or right\n",
            "    less_or_equal = left <= right\n",
            "    greater_or_equal = left >= right\n",
            "    not_equal = left != right\n",
            "    identity = left is right\n",
            "    membership = left in items\n",
            "    not_identity = left is not right\n",
            "    not_membership = left not in items\n",
            "    text = \"left < right and left == right\"\n",
            "    return left and right\n",
        );
        let rel = write_test_file(tmp.path(), "test.py", py_src);
        let operators = [
            "lt_to_lte".to_string(),
            "gt_to_gte".to_string(),
            "eq_to_neq".to_string(),
            "and_to_or".to_string(),
            "or_to_and".to_string(),
        ];

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange {
                start: 1,
                end: py_src.lines().count(),
            }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &operators).unwrap();
        let mut actual = mutations
            .iter()
            .map(|mutation| {
                (
                    mutation.operator.as_str(),
                    mutation.original.as_str(),
                    mutation.replacement.as_str(),
                )
            })
            .collect::<Vec<_>>();
        actual.sort_unstable();

        assert_eq!(
            actual,
            vec![
                ("and_to_or", "and", "or"),
                ("and_to_or", "and", "or"),
                ("eq_to_neq", "==", "!="),
                ("gt_to_gte", ">", ">="),
                ("lt_to_lte", "<", "<="),
                ("lt_to_lte", "<", "<="),
                ("or_to_and", "or", "and"),
            ]
        );
        for mutation in &mutations {
            let mutated = apply_mutation(py_src, mutation);
            assert!(
                !parse_python(&mutated).root_node().has_error(),
                "mutation should parse as Python:\n{mutated}"
            );
        }
    }

    #[test]
    fn python_multiline_comparison_candidates_stay_in_hunk() {
        let tmp = TempDir::new().unwrap();
        let src = concat!(
            "def check(a, b, c):\n",
            "    return (\n",
            "        a\n",
            "        < b\n",
            "        < c\n",
            "    )\n",
        );
        let rel = write_test_file(tmp.path(), "test.py", src);
        let mutations = generate_mutations(
            &[ChangedFile {
                path: rel,
                hunks: vec![LineRange { start: 4, end: 4 }],
            }],
            tmp.path(),
            100,
            0,
            &["lt_to_lte".into()],
        )
        .unwrap();
        let first_operator = src.find("\n        < b\n").unwrap() + "\n        ".len();
        let second_operator = src.find("\n        < c\n").unwrap() + "\n        ".len();

        assert_eq!(
            mutations.len(),
            1,
            "only the changed operator should mutate"
        );
        let mutation = &mutations[0];
        assert_eq!(mutation.byte_range, first_operator..first_operator + 1);
        assert_eq!(mutation.line, 4);
        assert_eq!(mutation.original, "<");
        assert_eq!(mutation.replacement, "<=");
        assert_ne!(mutation.byte_range.start, second_operator);

        let mutated = apply_mutation(src, mutation);
        assert!(
            !parse_python(&mutated).root_node().has_error(),
            "mutation should parse as Python:\n{mutated}"
        );
    }

    #[test]
    fn python_boolean_operator_return_does_not_generate_return_empty() {
        let tmp = TempDir::new().unwrap();
        let src = "def check(left, right):\n    return left and right\n";
        let rel = write_test_file(tmp.path(), "test.py", src);
        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 2 }],
        }];

        let mutations =
            generate_mutations(&changed, tmp.path(), 100, 0, &["return_empty".into()]).unwrap();

        assert!(
            mutations.is_empty(),
            "and/or returns are not necessarily boolean"
        );
    }

    #[test]
    fn generates_mutations_for_rust_file() {
        let tmp = TempDir::new().unwrap();
        let rs_src = "fn check(x: i32, y: i32) -> bool {\n    x < y\n}\n";
        let rel = write_test_file(tmp.path(), "lib.rs", rs_src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 3 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        assert!(!mutations.is_empty(), "expected mutations for Rust file");
        assert_eq!(mutations[0].language, "rust");
    }

    #[test]
    fn mutation_ids_are_sequential() {
        let tmp = TempDir::new().unwrap();
        let src = "package main\n\nfunc f(x, y int) bool {\n\tif x < y {\n\t\treturn true\n\t}\n\treturn false\n}\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 8 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        assert!(!mutations.is_empty(), "expected at least one mutation");
        for (i, m) in mutations.iter().enumerate() {
            assert_eq!(m.id, i as u32, "mutation ids should be sequential");
        }
    }

    #[test]
    fn rust_return_empty_skipped_for_result_type() {
        let tmp = TempDir::new().unwrap();
        let src = "fn f() -> Result<i32, String> {\n    return Ok(42);\n}\n";
        let rel = write_test_file(tmp.path(), "lib.rs", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 3 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        let has_return_empty = mutations.iter().any(|m| m.operator == "return_empty");
        assert!(
            !has_return_empty,
            "return_empty should be skipped for Result return type, got: {:?}",
            mutations.iter().map(|m| &m.operator).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rust_return_empty_allowed_for_primitive() {
        let tmp = TempDir::new().unwrap();
        let src = "fn f() -> i32 {\n    return 42;\n}\n";
        let rel = write_test_file(tmp.path(), "lib.rs", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 3 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        let has_return_empty = mutations.iter().any(|m| m.operator == "return_empty");
        assert!(
            has_return_empty,
            "return_empty should be allowed for i32 return type, got: {:?}",
            mutations.iter().map(|m| &m.operator).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rust_expression_if_filters_invalid_removal_mutations() {
        let tmp = TempDir::new().unwrap();
        let src = concat!(
            "fn label(flag: bool) -> Option<String> {\n",
            "    if flag {\n",
            "        Some(\"yes\".to_owned())\n",
            "    } else {\n",
            "        None\n",
            "    }\n",
            "}\n",
        );
        let rel = write_test_file(tmp.path(), "lib.rs", src);
        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 7 }],
        }];

        let mutations = generate_mutations(
            &changed,
            tmp.path(),
            100,
            0,
            &[
                "remove_if_body".into(),
                "remove_else".into(),
                "negate_condition".into(),
            ],
        )
        .unwrap();

        assert!(
            !mutations.iter().any(|mutation| matches!(
                mutation.operator.as_str(),
                "remove_if_body" | "remove_else"
            )),
            "expression-valued Rust if removals must be filtered: {mutations:?}"
        );
        let control = mutations
            .iter()
            .find(|mutation| mutation.operator == "negate_condition")
            .expect("the Rust feasibility filter must leave valid if mutations available");
        let mutated = apply_mutation(src, control);
        assert!(
            !parse_rust(&mutated).root_node().has_error(),
            "control mutation should remain valid Rust:\n{mutated}"
        );
        assert_rust_library_compiles(&mutated);
    }

    #[test]
    fn rust_field_value_if_filters_invalid_removal_mutations() {
        let tmp = TempDir::new().unwrap();
        let src = concat!(
            "struct Marker { value: Option<i32> }\n",
            "fn label(flag: bool) -> Marker {\n",
            "    Marker { value: if flag { Some(1) } else { None } }\n",
            "}\n",
        );
        let rel = write_test_file(tmp.path(), "lib.rs", src);
        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 4 }],
        }];
        let mutations = generate_mutations(
            &changed,
            tmp.path(),
            100,
            0,
            &[
                "remove_if_body".into(),
                "remove_else".into(),
                "negate_condition".into(),
            ],
        )
        .unwrap();

        assert!(
            !mutations.iter().any(|mutation| matches!(
                mutation.operator.as_str(),
                "remove_if_body" | "remove_else"
            )),
            "field-valued Rust if removals must be filtered: {mutations:?}"
        );
        let control = mutations
            .iter()
            .find(|mutation| mutation.operator == "negate_condition")
            .expect("the Rust feasibility filter must preserve valid if mutations");
        let mutated = apply_mutation(src, control);
        assert!(
            !parse_rust(&mutated).root_node().has_error(),
            "control mutation should remain valid Rust:\n{mutated}"
        );
        assert_rust_library_compiles(&mutated);
    }

    #[test]
    fn rust_nested_tail_if_filters_invalid_removal_mutations() {
        let tmp = TempDir::new().unwrap();
        let src = concat!(
            "pub fn f(flag: bool) -> i32 {\n",
            "    if flag {\n",
            "        if flag {\n",
            "            1\n",
            "        } else {\n",
            "            2\n",
            "        }\n",
            "    } else {\n",
            "        3\n",
            "    }\n",
            "}\n",
        );
        let rel = write_test_file(tmp.path(), "lib.rs", src);
        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange {
                start: 1,
                end: src.lines().count(),
            }],
        }];
        let mutations = generate_mutations(
            &changed,
            tmp.path(),
            100,
            0,
            &[
                "remove_if_body".into(),
                "remove_else".into(),
                "negate_condition".into(),
            ],
        )
        .unwrap();

        assert!(
            !mutations.iter().any(|mutation| matches!(
                mutation.operator.as_str(),
                "remove_if_body" | "remove_else"
            )),
            "nested value-producing Rust if removals must be filtered: {mutations:?}"
        );
        let controls: Vec<_> = mutations
            .iter()
            .filter(|mutation| mutation.operator == "negate_condition")
            .collect();
        assert_eq!(
            controls.len(),
            2,
            "both nested if conditions should retain valid mutations: {mutations:?}"
        );
        for control in controls {
            let mutated = apply_mutation(src, control);
            assert!(
                !parse_rust(&mutated).root_node().has_error(),
                "control mutation should remain valid Rust:\n{mutated}"
            );
            assert_rust_library_compiles(&mutated);
        }
    }

    #[test]
    fn rust_match_arm_tail_if_filters_invalid_removal_mutations() {
        let tmp = TempDir::new().unwrap();
        let src = concat!(
            "pub fn f(flag: bool) -> i32 {\n",
            "    match flag {\n",
            "        true => {\n",
            "            if flag {\n",
            "                1\n",
            "            } else {\n",
            "                2\n",
            "            }\n",
            "        }\n",
            "        false => 3,\n",
            "    }\n",
            "}\n",
        );
        let rel = write_test_file(tmp.path(), "lib.rs", src);
        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange {
                start: 1,
                end: src.lines().count(),
            }],
        }];
        let mutations = generate_mutations(
            &changed,
            tmp.path(),
            100,
            0,
            &[
                "remove_if_body".into(),
                "remove_else".into(),
                "negate_condition".into(),
            ],
        )
        .unwrap();

        assert!(
            !mutations.iter().any(|mutation| matches!(
                mutation.operator.as_str(),
                "remove_if_body" | "remove_else"
            )),
            "match-arm value-producing Rust if removals must be filtered: {mutations:?}"
        );
        let controls: Vec<_> = mutations
            .iter()
            .filter(|mutation| mutation.operator == "negate_condition")
            .collect();
        assert_eq!(
            controls.len(),
            1,
            "the match-arm if condition should retain a valid mutation: {mutations:?}"
        );
        for control in controls {
            let mutated = apply_mutation(src, control);
            assert!(
                !parse_rust(&mutated).root_node().has_error(),
                "control mutation should remain valid Rust:\n{mutated}"
            );
            assert_rust_library_compiles(&mutated);
        }
    }

    #[test]
    fn rust_non_tail_nonunit_if_filters_invalid_removal_mutations() {
        let tmp = TempDir::new().unwrap();
        let src = concat!(
            "fn work() {}\n",
            "pub fn f(flag: bool) {\n",
            "    if flag {\n",
            "        1\n",
            "    } else {\n",
            "        2\n",
            "    };\n",
            "    work();\n",
            "}\n",
        );
        let rel = write_test_file(tmp.path(), "lib.rs", src);
        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange {
                start: 1,
                end: src.lines().count(),
            }],
        }];
        let mutations = generate_mutations(
            &changed,
            tmp.path(),
            100,
            0,
            &[
                "remove_if_body".into(),
                "remove_else".into(),
                "negate_condition".into(),
            ],
        )
        .unwrap();

        assert!(
            !mutations.iter().any(|mutation| matches!(
                mutation.operator.as_str(),
                "remove_if_body" | "remove_else"
            )),
            "non-unit if statement removals must be filtered: {mutations:?}"
        );
        let controls: Vec<_> = mutations
            .iter()
            .filter(|mutation| mutation.operator == "negate_condition")
            .collect();
        assert_eq!(
            controls.len(),
            1,
            "the non-unit if condition should retain a valid mutation: {mutations:?}"
        );
        for control in controls {
            let mutated = apply_mutation(src, control);
            assert!(
                !parse_rust(&mutated).root_node().has_error(),
                "control mutation should remain valid Rust:\n{mutated}"
            );
            assert_rust_library_compiles(&mutated);
        }
    }

    #[test]
    fn rust_statement_and_unit_if_keep_valid_removal_mutations() {
        for (name, src) in [
            (
                "statement",
                concat!(
                    "fn work() {}\n",
                    "fn statement(flag: bool) -> i32 {\n",
                    "    if flag {\n",
                    "        work();\n",
                    "    } else {\n",
                    "        work();\n",
                    "    }\n",
                    "    3\n",
                    "}\n",
                ),
            ),
            (
                "unit tail",
                concat!(
                    "fn work() {}\n",
                    "fn unit_tail(flag: bool) {\n",
                    "    if flag {\n",
                    "        work();\n",
                    "    } else {\n",
                    "        work();\n",
                    "    }\n",
                    "}\n",
                ),
            ),
        ] {
            let tmp = TempDir::new().unwrap();
            let rel = write_test_file(tmp.path(), "lib.rs", src);
            let changed = vec![ChangedFile {
                path: rel,
                hunks: vec![LineRange {
                    start: 1,
                    end: src.lines().count(),
                }],
            }];
            let mutations = generate_mutations(
                &changed,
                tmp.path(),
                100,
                0,
                &["remove_if_body".into(), "remove_else".into()],
            )
            .unwrap();

            for operator in ["remove_if_body", "remove_else"] {
                let mutation = mutations
                    .iter()
                    .find(|mutation| mutation.operator == operator)
                    .unwrap_or_else(|| {
                        panic!("{name} Rust if should retain {operator}: {mutations:?}")
                    });
                let mutated = apply_mutation(src, mutation);
                assert!(
                    !parse_rust(&mutated).root_node().has_error(),
                    "{operator} should parse as Rust:\n{mutated}"
                );
                assert_rust_library_compiles(&mutated);
            }
        }
    }

    #[test]
    fn rust_nested_unit_if_keeps_all_removal_mutations() {
        let tmp = TempDir::new().unwrap();
        let src = concat!(
            "fn work() {}\n",
            "pub fn f(a: bool, b: bool) {\n",
            "    if a {\n",
            "        if b {\n",
            "            work();\n",
            "        } else {\n",
            "            work();\n",
            "        }\n",
            "    } else {\n",
            "        work();\n",
            "    }\n",
            "}\n",
        );
        let rel = write_test_file(tmp.path(), "lib.rs", src);
        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange {
                start: 1,
                end: src.lines().count(),
            }],
        }];
        let mutations = generate_mutations(
            &changed,
            tmp.path(),
            100,
            0,
            &["remove_if_body".into(), "remove_else".into()],
        )
        .unwrap();
        let removals: Vec<_> = mutations
            .iter()
            .filter(|mutation| {
                matches!(mutation.operator.as_str(), "remove_if_body" | "remove_else")
            })
            .collect();
        assert_eq!(
            removals.len(),
            4,
            "both nested unit ifs should retain both removals: {mutations:?}"
        );
        for mutation in removals {
            let mutated = apply_mutation(src, mutation);
            assert!(
                !parse_rust(&mutated).root_node().has_error(),
                "unit if removal should parse as Rust:\n{mutated}"
            );
            assert_rust_library_compiles(&mutated);
        }
    }

    #[test]
    fn rust_no_else_never_if_keeps_remove_if_body() {
        let tmp = TempDir::new().unwrap();
        let src = concat!(
            "fn never() -> ! { panic!() }\n",
            "pub fn f(flag: bool) {\n",
            "    if flag {\n",
            "        never()\n",
            "    }\n",
            "}\n",
        );
        let rel = write_test_file(tmp.path(), "lib.rs", src);
        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange {
                start: 1,
                end: src.lines().count(),
            }],
        }];
        let mutations =
            generate_mutations(&changed, tmp.path(), 100, 0, &["remove_if_body".into()]).unwrap();
        let mutation = mutations
            .iter()
            .find(|mutation| mutation.operator == "remove_if_body")
            .expect("no-else if replacement is always unit");
        let mutated = apply_mutation(src, mutation);
        assert!(
            !parse_rust(&mutated).root_node().has_error(),
            "no-else if removal should parse as Rust:\n{mutated}"
        );
        assert_rust_library_compiles(&mutated);
    }

    #[test]
    fn return_empty_skips_unknown_call_return_value() {
        let tmp = TempDir::new().unwrap();
        let src = "fn f() -> bool {\n    return ready();\n}\n";
        let rel = write_test_file(tmp.path(), "lib.rs", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 3 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        let has_return_empty = mutations.iter().any(|m| m.operator == "return_empty");

        assert!(
            !has_return_empty,
            "return_empty should skip call return values until it can derive a typed default, got: {:?}",
            mutations.iter().map(|m| &m.operator).collect::<Vec<_>>()
        );
    }

    #[test]
    fn go_return_empty_skipped_for_multi_return() {
        let tmp = TempDir::new().unwrap();
        let src = "package main\n\nfunc f() (int, error) {\n\treturn 0, nil\n}\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 5 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        let has_return_empty = mutations.iter().any(|m| m.operator == "return_empty");
        assert!(
            !has_return_empty,
            "return_empty should be skipped for multi-return Go func, got: {:?}",
            mutations.iter().map(|m| &m.operator).collect::<Vec<_>>()
        );
    }

    #[test]
    fn go_if_condition_excludes_unchanged_else_mutation() {
        let tmp = TempDir::new().unwrap();
        let src = "package main\n\nfunc f(x int) int {\n\tif x > 0 {\n\t\treturn 1\n\t} else {\n\t\treturn 0\n\t}\n}\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 4, end: 4 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        let operators: Vec<&str> = mutations.iter().map(|m| m.operator.as_str()).collect();

        assert!(
            operators.contains(&"gt_to_gte"),
            "condition binary mutation should be generated, got: {:?}",
            operators
        );
        assert!(
            operators.contains(&"zero_to_one"),
            "condition literal mutation should be generated, got: {:?}",
            operators
        );
        assert!(
            operators.contains(&"negate_condition"),
            "parent if condition mutation should be generated, got: {:?}",
            operators
        );
        assert!(
            operators.contains(&"remove_if_body"),
            "parent if body mutation should be generated, got: {:?}",
            operators
        );
        assert!(
            !operators.contains(&"remove_else"),
            "else removal must stay outside a condition-only hunk, got: {:?}",
            operators
        );
    }

    #[test]
    fn skips_unsupported_file_types() {
        let tmp = TempDir::new().unwrap();
        let rel = write_test_file(tmp.path(), "notes.txt", "hello world");

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 1 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        assert!(mutations.is_empty());
    }

    #[test]
    fn respects_max_per_file() {
        let tmp = TempDir::new().unwrap();
        // File with many mutable nodes
        let src = "package main\n\nfunc f(a, b, c, d int) bool {\n\tif a < b {\n\t\treturn true\n\t}\n\tif c < d {\n\t\treturn false\n\t}\n\treturn a > c\n}\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 1, end: 11 }],
        }];

        let uncapped = generate_mutations(&changed, tmp.path(), 1000, 0, &[]).unwrap();
        let capped = generate_mutations(&changed, tmp.path(), 1000, 3, &[]).unwrap();

        assert!(
            uncapped.len() > 3,
            "need more than 3 uncapped mutations to test cap, got {}",
            uncapped.len()
        );
        assert_eq!(capped.len(), 3, "expected exactly 3 mutations with cap");
    }

    #[test]
    fn sample_diverse_round_robins_operators() {
        let make = |op: &str, line: usize| Mutation {
            id: 0,
            file: PathBuf::from("test.rs"),
            language: "rust".into(),
            line,
            column: 1,
            operator: op.into(),
            description: String::new(),
            original: "x".into(),
            replacement: "y".into(),
            byte_range: 0..1,
        };

        let mutations = vec![
            make("string_to_empty", 1),
            make("string_to_empty", 2),
            make("string_to_empty", 3),
            make("string_to_empty", 4),
            make("string_to_empty", 5),
            make("string_to_empty", 6),
            make("string_to_empty", 7),
            make("string_to_empty", 8),
            make("string_to_empty", 9),
            make("string_to_empty", 10),
            make("true_to_false", 20),
            make("true_to_false", 21),
        ];

        let sampled = sample_diverse(mutations, 4);
        assert_eq!(sampled.len(), 4);

        let string_count = sampled
            .iter()
            .filter(|m| m.operator == "string_to_empty")
            .count();
        let bool_count = sampled
            .iter()
            .filter(|m| m.operator == "true_to_false")
            .count();

        assert_eq!(bool_count, 2, "expected 2 true_to_false mutations");
        assert_eq!(string_count, 2, "expected 2 string_to_empty mutations");
    }

    #[test]
    fn sample_diverse_returns_all_when_under_cap() {
        let make = |op: &str, line: usize| Mutation {
            id: 0,
            file: PathBuf::from("test.rs"),
            language: "rust".into(),
            line,
            column: 1,
            operator: op.into(),
            description: String::new(),
            original: "x".into(),
            replacement: "y".into(),
            byte_range: 0..1,
        };

        let mutations = vec![make("op_a", 1), make("op_b", 2)];
        let sampled = sample_diverse(mutations, 10);
        assert_eq!(sampled.len(), 2);
    }

    #[test]
    fn byte_offset_to_line_col_basic() {
        let src = b"line1\nline2\nline3\n";
        assert_eq!(byte_offset_to_line_col(src, 0), (1, 1));
        assert_eq!(byte_offset_to_line_col(src, 4), (1, 5));
        assert_eq!(byte_offset_to_line_col(src, 6), (2, 1));
        assert_eq!(byte_offset_to_line_col(src, 12), (3, 1));
    }

    #[test]
    fn mutation_line_col_matches_byte_range() {
        let tmp = TempDir::new().unwrap();
        // return_empty mutation: the operator targets the return value,
        // not the return keyword.
        let src = "package main\n\nfunc f() int {\n\treturn 42\n}\n";
        let rel = write_test_file(tmp.path(), "main.go", src);

        let changed = vec![ChangedFile {
            path: rel,
            hunks: vec![LineRange { start: 4, end: 4 }],
        }];

        let mutations = generate_mutations(&changed, tmp.path(), 100, 0, &[]).unwrap();
        let m = mutations
            .iter()
            .find(|m| m.operator == "return_empty")
            .expect("return_empty mutation should be generated");
        // column should point to "42", not "return"
        assert_eq!(
            m.column, 9,
            "column should point to the value, not the return keyword"
        );
    }
}
