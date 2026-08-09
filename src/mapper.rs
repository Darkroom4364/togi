// Map changed lines to AST nodes

use crate::languages::LanguageSupport;
use crate::{LineRange, ts_row_to_line};

/// Find mutation-relevant AST nodes that overlap changed lines.
///
/// The mapper walks the parsed tree, ignores nodes outside the diff hunks, and
/// lets the language implementation skip imports, tests, macros, or other
/// subtrees. Nodes whose own start line is changed are retained even when they
/// contain mutable children, so statement/control-flow operators can run
/// alongside expression/literal operators. Returned nodes are candidates for
/// operator application; individual operator candidates may still be filtered
/// later by the mutator.
pub fn find_mutable_nodes<'a>(
    tree: &'a tree_sitter::Tree,
    source: &'a [u8],
    changed_lines: &[LineRange],
    lang: &dyn LanguageSupport,
) -> Vec<tree_sitter::Node<'a>> {
    let mut nodes = Vec::new();
    let root = tree.root_node();
    collect_mutable_nodes(root, source, changed_lines, lang, &mut nodes);
    nodes
}

/// Recursively walk the tree, collecting mutation-relevant nodes that overlap
/// with changed lines.
/// Returns `true` if a skipped subtree was encountered, preventing the parent
/// from being added as a fallback mutable node.
fn collect_mutable_nodes<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a [u8],
    changed_lines: &[LineRange],
    lang: &dyn LanguageSupport,
    results: &mut Vec<tree_sitter::Node<'a>>,
) -> bool {
    let node_start = ts_row_to_line(node.start_position().row);
    let node_end = ts_row_to_line(node.end_position().row);

    // Check if this node overlaps any changed line range
    if !overlaps(node_start, node_end, changed_lines) {
        return false;
    }

    // Skip subtrees by kind (imports, macros, etc.)
    if lang.skip_subtree_kinds().contains(&node.kind()) {
        return true;
    }

    // Skip subtrees by content (test modules, test functions, etc.)
    if lang.should_skip_node(&node, source) {
        return true;
    }

    // Try to find relevant children first (prefer deepest nodes)
    let mut found_child = false;
    let mut skipped_child = false;
    let child_count = node.child_count() as u32;
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            let before = results.len();
            skipped_child |= collect_mutable_nodes(child, source, changed_lines, lang, results);
            if results.len() > before {
                found_child = true;
            }
        }
    }

    // Add this node if it is directly on a changed line, even when it has
    // mutable children. That lets parent-level operators (if body removal,
    // condition negation, return replacement, assignment removal) run alongside
    // expression/literal operators. Keep the old deepest-node fallback for
    // mutable descendants that overlap changed ranges through their span.
    let starts_on_changed_line = overlaps(node_start, node_start, changed_lines);
    if !skipped_child
        && node.is_named()
        && lang.is_mutable_node_kind(node.kind())
        && (starts_on_changed_line || !found_child)
    {
        results.push(node);
    }
    skipped_child
}

/// Check whether a source line range overlaps any changed line range.
/// Requires: changed_lines sorted with non-decreasing start and end values,
/// and each range satisfies `start <= end`. Produced by `parse_diff`.
pub(crate) fn overlaps(node_start: usize, node_end: usize, changed_lines: &[LineRange]) -> bool {
    debug_assert!(
        changed_lines
            .windows(2)
            .all(|w| w[0].start <= w[1].start && w[0].end <= w[1].end)
            && changed_lines.iter().all(|r| r.start <= r.end),
        "changed_lines must be sorted with non-decreasing start and end values"
    );
    // Find first range whose end >= node_start (could overlap)
    let idx = changed_lines.partition_point(|r| r.end < node_start);
    // Check if that range's start <= node_end (actual overlap)
    idx < changed_lines.len() && changed_lines[idx].start <= node_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{parse_csharp, parse_go, parse_java, parse_rust, parse_typescript};

    struct StubLang {
        skip_kinds: &'static [&'static str],
        mutable_kinds: Option<&'static [&'static str]>,
    }

    impl StubLang {
        fn new() -> Self {
            Self {
                skip_kinds: &[],
                mutable_kinds: None,
            }
        }
        fn with_skip(skip_kinds: &'static [&'static str]) -> Self {
            Self {
                skip_kinds,
                mutable_kinds: None,
            }
        }
        fn with_mutable(mutable_kinds: &'static [&'static str]) -> Self {
            Self {
                skip_kinds: &[],
                mutable_kinds: Some(mutable_kinds),
            }
        }
    }

    impl LanguageSupport for StubLang {
        fn name(&self) -> &str {
            "stub"
        }
        fn extensions(&self) -> &[&str] {
            &[]
        }
        fn tree_sitter_language(&self) -> tree_sitter::Language {
            tree_sitter_go::LANGUAGE.into()
        }
        fn binary_expression_node(&self) -> &str {
            "binary_expression"
        }
        fn if_statement_node(&self) -> &str {
            "if_statement"
        }
        fn boolean_true_literals(&self) -> &[&str] {
            &["true"]
        }
        fn boolean_false_literals(&self) -> &[&str] {
            &["false"]
        }
        fn return_statement_node(&self) -> &str {
            "return_statement"
        }
        fn is_mutable_node_kind(&self, kind: &str) -> bool {
            self.mutable_kinds.map_or_else(
                || crate::languages::is_default_mutable_node_kind(self, kind),
                |kinds| kinds.contains(&kind),
            )
        }
        fn operator_field(&self) -> &str {
            "operator"
        }
        fn skip_subtree_kinds(&self) -> &[&str] {
            self.skip_kinds
        }
    }

    fn source_str(source: &[u8]) -> &str {
        std::str::from_utf8(source).unwrap()
    }

    #[test]
    fn finds_binary_expression_on_changed_lines() {
        let source = b"package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=return a+b, 5=}
        let tree = parse_go(source_str(source));
        let changed = vec![LineRange { start: 3, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(kinds.contains(&"binary_expression"));
    }

    #[test]
    fn no_changed_lines_returns_empty() {
        let source = b"package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n";
        let tree = parse_go(source_str(source));
        let changed: Vec<LineRange> = vec![];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(nodes.is_empty());
    }

    #[test]
    fn finds_if_statement_on_changed_line() {
        let source = b"package main\n\nfunc check(x int) int {\n\tif x > 0 {\n\t\treturn 1\n\t}\n\treturn 0\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=if x>0, 5=return 1, 6=}, 7=return 0, 8=}
        let tree = parse_go(source_str(source));
        let changed = vec![LineRange { start: 4, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"if_statement"),
            "Should include parent if_statement for parent-level operators, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"binary_expression"),
            "Should include condition binary_expression, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"int_literal"),
            "Should include condition literal mutation, got: {:?}",
            kinds
        );
    }

    #[test]
    fn multiple_overlapping_ranges() {
        let source = b"package main\n\nfunc f(a, b int) int {\n\tif a > 0 {\n\t\treturn a + b\n\t}\n\treturn a - b\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=if a>0, 5=return a+b, 6=}, 7=return a-b, 8=}
        let tree = parse_go(source_str(source));
        // Two ranges that both touch the function body
        let changed = vec![
            LineRange { start: 4, end: 5 },
            LineRange { start: 5, end: 7 },
        ];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        // Should find binary expressions from both return statements
        let binary_count = kinds.iter().filter(|k| **k == "binary_expression").count();
        assert!(
            binary_count >= 2,
            "Expected at least 2 binary_expression nodes from overlapping ranges, got: {:?}",
            kinds
        );
    }

    #[test]
    fn finds_return_statement_on_changed_line() {
        let source = b"package main\n\nfunc f() int {\n\treturn 42\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=return 42, 5=}
        let tree = parse_go(source_str(source));
        let changed = vec![LineRange { start: 4, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"return_statement"),
            "Should include parent return_statement for return_empty, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"int_literal"),
            "Should include return value literal mutation, got: {:?}",
            kinds
        );
    }

    #[test]
    fn mutable_node_selection_comes_from_language_support() {
        let source = b"package main\n\nfunc f() int {\n\treturn 42\n}\n";
        let tree = parse_go(source_str(source));
        let changed = vec![LineRange { start: 4, end: 4 }];
        let lang = StubLang::with_mutable(&["return_statement"]);

        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(kinds.contains(&"return_statement"));
        assert!(
            !kinds.contains(&"int_literal"),
            "mapper should use language-provided mutable kinds, got: {:?}",
            kinds
        );
    }

    #[test]
    fn finds_assignment_on_changed_line() {
        let source = b"package main\n\nfunc f() {\n\tx := 1\n\tx = x + 2\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=x:=1, 5=x=x+2, 6=}
        let tree = parse_go(source_str(source));
        let changed = vec![LineRange { start: 5, end: 5 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"assignment_statement"),
            "Should include parent assignment_statement for removal operators, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"binary_expression"),
            "Should include assignment value binary_expression, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"int_literal"),
            "Should include assignment value literal mutation, got: {:?}",
            kinds
        );
    }

    #[test]
    fn nested_if_inside_if() {
        let source = b"package main\n\nfunc f(a, b int) int {\n\tif a > 0 {\n\t\tif b > 0 {\n\t\t\treturn a + b\n\t\t}\n\t}\n\treturn 0\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=if a>0, 5=if b>0, 6=return a+b, 7=}, 8=}, 9=return 0, 10=}
        let tree = parse_go(source_str(source));
        // Only the inner if line is changed
        let changed = vec![LineRange { start: 5, end: 6 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"if_statement"),
            "Should include changed inner if_statement, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"binary_expression"),
            "Should include changed inner binary_expression, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"int_literal"),
            "Should include changed inner int_literal, got: {:?}",
            kinds
        );
        assert!(
            nodes
                .iter()
                .filter(|node| node.kind() == "if_statement")
                .all(|node| ts_row_to_line(node.start_position().row) >= 5),
            "Should not include outer if_statement whose start line did not change, got: {:?}",
            nodes
                .iter()
                .map(|node| (node.kind(), ts_row_to_line(node.start_position().row)))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn binary_expression_inside_function_call() {
        let source =
            b"package main\n\nimport \"fmt\"\n\nfunc f(a, b int) {\n\tfmt.Println(a + b)\n}\n";
        // Lines: 1=package, 2=empty, 3=import, 4=empty, 5=func, 6=fmt.Println(a+b), 7=}
        let tree = parse_go(source_str(source));
        let changed = vec![LineRange { start: 6, end: 6 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        // Should find the binary_expression nested inside the call
        assert!(
            kinds.contains(&"binary_expression"),
            "Expected binary_expression inside call, got: {:?}",
            kinds
        );
    }

    #[test]
    fn comment_only_lines_return_no_mutable_nodes() {
        let source = b"package main\n\n// this is a comment\n// another comment\nfunc f() {}\n";
        // Lines: 1=package, 2=empty, 3=comment, 4=comment, 5=func
        let tree = parse_go(source_str(source));
        let changed = vec![LineRange { start: 3, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(
            nodes.is_empty(),
            "Comments should not produce mutable nodes, got: {:?}",
            nodes.iter().map(|n| n.kind()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn package_declaration_returns_no_mutable_nodes() {
        let source = b"package main\n\nfunc f() {\n\treturn\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=return, 5=}
        let tree = parse_go(source_str(source));
        // Only the package line is changed
        let changed = vec![LineRange { start: 1, end: 1 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(
            nodes.is_empty(),
            "Package declaration should not produce mutable nodes, got: {:?}",
            nodes.iter().map(|n| n.kind()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rust_return_expression_on_changed_line() {
        let source = b"fn add(a: i32, b: i32) -> i32 {\n    return a + b;\n}\n";
        // Lines: 1=fn, 2=return a+b, 3=}
        let tree = parse_rust(source_str(source));
        let changed = vec![LineRange { start: 2, end: 2 }];
        let lang = crate::languages::rust_lang::Rust;

        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"return_expression"),
            "Should include parent return_expression for return_empty, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"binary_expression"),
            "Should include returned binary_expression, got: {:?}",
            kinds
        );
    }

    #[test]
    fn rust_use_declaration_skipped() {
        let source = b"use std::collections::HashMap;\nfn f() -> bool { true }\n";
        let tree = parse_rust(source_str(source));
        let changed = vec![LineRange { start: 1, end: 2 }];

        let lang = StubLang::with_skip(&["use_declaration"]);
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        // Should find `true` in the function but nothing from the use declaration
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            !kinds.contains(&"string_literal") && !kinds.contains(&"identifier"),
            "Use declaration should be skipped, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"true") || kinds.contains(&"boolean_literal"),
            "Should still find mutable nodes outside skipped ancestors, got: {:?}",
            kinds
        );
    }

    #[test]
    fn rust_macro_invocation_skipped() {
        let source = b"fn f() {\n    println!(\"hello\");\n    let x = true;\n}\n";
        let tree = parse_rust(source_str(source));
        let changed = vec![LineRange { start: 1, end: 4 }];

        let lang = StubLang::with_skip(&["macro_invocation"]);
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            !kinds.contains(&"string_literal") && !kinds.contains(&"string_content"),
            "Macro string should be skipped, got: {:?}",
            kinds
        );
        assert!(
            !kinds.contains(&"expression_statement"),
            "Parent expression_statement of skipped macro should not leak, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"true") || kinds.contains(&"boolean_literal"),
            "Should still find boolean literal outside macro, got: {:?}",
            kinds
        );
    }

    #[test]
    fn go_import_skipped() {
        let source = b"package main\n\nimport \"fmt\"\n\nfunc f() bool {\n\treturn true\n}\n";
        let tree = parse_go(source_str(source));
        let changed = vec![LineRange { start: 1, end: 7 }];

        let lang = StubLang::with_skip(&["import_spec"]);
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            !kinds.contains(&"interpreted_string_literal"),
            "Import string should be skipped, got: {:?}",
            kinds
        );
    }

    #[test]
    fn typescript_import_skipped() {
        let source = b"import { foo } from 'bar';\nconst x = true;\n";
        let tree = parse_typescript(source_str(source));
        let changed = vec![LineRange { start: 1, end: 2 }];

        let lang = StubLang::with_skip(&["import_statement"]);
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            !kinds.contains(&"string") && !kinds.contains(&"string_literal"),
            "Import string should be skipped, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"true"),
            "Should still find `true` outside import, got: {:?}",
            kinds
        );
    }

    #[test]
    fn typescript_type_annotation_skipped() {
        let source = b"function f(x: string): boolean { return true; }\n";
        let tree = parse_typescript(source_str(source));
        let changed = vec![LineRange { start: 1, end: 1 }];

        let lang = StubLang::with_skip(&["type_annotation"]);
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"true"),
            "Should still find `true` in function body, got: {:?}",
            kinds
        );
    }

    #[test]
    fn java_import_skipped() {
        let source = b"import java.util.List;\nclass T { boolean f() { return true; } }\n";
        let tree = parse_java(source_str(source));
        let changed = vec![LineRange { start: 1, end: 2 }];

        let lang = StubLang::with_skip(&["import_declaration"]);
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"true"),
            "Should still find `true` outside import, got: {:?}",
            kinds
        );
    }

    #[test]
    fn csharp_using_skipped() {
        let source = b"using System.Collections;\nclass T { bool F() { return true; } }\n";
        let tree = parse_csharp(source_str(source));
        let changed = vec![LineRange { start: 1, end: 2 }];

        let lang = StubLang::with_skip(&["using_directive"]);
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"true") || kinds.contains(&"boolean_literal"),
            "Should still find boolean outside using, got: {:?}",
            kinds
        );
    }

    #[test]
    fn rust_nested_if_expression() {
        let source = b"fn f(a: i32, b: i32) -> i32 {\n    if a > 0 {\n        if b > 0 {\n            return a + b;\n        }\n    }\n    0\n}\n";
        // Lines: 1=fn, 2=if a>0, 3=if b>0, 4=return a+b, 5=}, 6=}, 7=0, 8=}
        let tree = parse_rust(source_str(source));
        let changed = vec![LineRange { start: 3, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &StubLang::new());

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(kinds.contains(&"binary_expression"));
    }

    #[test]
    fn rust_cfg_test_module_skipped() {
        let source = b"fn prod() -> bool { true }\n\n#[cfg(test)]\nmod tests {\n    fn test_something() -> bool { true }\n}\n";
        let tree = parse_rust(source_str(source));
        let changed = vec![LineRange { start: 1, end: 6 }];
        let lang = crate::languages::rust_lang::Rust;
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);
        // Should find `true` in prod() but nothing from mod tests
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"true") || kinds.contains(&"boolean_literal"),
            "Should find boolean literal in production code, got: {:?}",
            kinds
        );
        // All nodes should be from line 1 (prod function), none from line 4+
        for node in &nodes {
            let line = crate::ts_row_to_line(node.start_position().row);
            assert!(
                line < 3,
                "Found mutation at line {} inside #[cfg(test)] module",
                line
            );
        }
    }

    #[test]
    fn rust_test_function_skipped() {
        let source =
            b"fn prod() -> bool { true }\n\n#[test]\nfn test_foo() -> bool {\n    1 + 2 == 3\n}\n";
        let tree = parse_rust(source_str(source));
        let changed = vec![LineRange { start: 1, end: 6 }];
        let lang = crate::languages::rust_lang::Rust;
        let nodes = find_mutable_nodes(&tree, source, &changed, &lang);
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"true") || kinds.contains(&"boolean_literal"),
            "Should find boolean literal in production code, got: {:?}",
            kinds
        );
        for node in &nodes {
            let line = crate::ts_row_to_line(node.start_position().row);
            assert!(
                line < 3,
                "Found mutation at line {} inside #[test] function",
                line
            );
        }
    }
}
