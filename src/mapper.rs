// Map changed lines to AST nodes

use crate::{LineRange, ts_row_to_line};

/// Node kinds that are relevant for mutation testing.
const MUTABLE_NODE_KINDS: &[&str] = &[
    "binary_expression",
    "if_statement",
    "if_expression",
    "return_statement",
    "return_expression",
    "true",
    "false",
    "integer_literal",
    "int_literal",
    "number",
    "number_literal",
    "unary_expression",
    "unary_expr",
    "not_operator",
    "boolean_literal",
    "interpreted_string_literal",
    "raw_string_literal",
    "string",
    "string_literal",
    "string_content",
    "template_string",
    "expression_statement",
    "expression_stmt",
    "assignment_statement",
    "assignment_expression",
    "assignment",
    "augmented_assignment",
    "augmented_assignment_expression",
];

/// Find AST nodes that overlap with changed line ranges and are candidates for mutation.
pub fn find_mutable_nodes<'a>(
    tree: &'a tree_sitter::Tree,
    source: &'a [u8],
    changed_lines: &[LineRange],
    skip_subtree_kinds: &[&str],
) -> Vec<tree_sitter::Node<'a>> {
    let mut nodes = Vec::new();
    let root = tree.root_node();
    collect_mutable_nodes(root, source, changed_lines, skip_subtree_kinds, &mut nodes);
    nodes
}

/// Recursively walk the tree, collecting the deepest mutation-relevant nodes
/// that overlap with changed lines.
fn collect_mutable_nodes<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a [u8],
    changed_lines: &[LineRange],
    skip_subtree_kinds: &[&str],
    results: &mut Vec<tree_sitter::Node<'a>>,
) {
    let _ = source; // available for future use

    let node_start = ts_row_to_line(node.start_position().row);
    let node_end = ts_row_to_line(node.end_position().row);

    // Check if this node overlaps any changed line range
    if !overlaps(node_start, node_end, changed_lines) {
        return;
    }

    // Skip subtrees that produce non-compilable mutations (imports, macros, etc.)
    if skip_subtree_kinds.contains(&node.kind()) {
        return;
    }

    // Try to find relevant children first (prefer deepest nodes)
    let mut found_child = false;
    let child_count = node.child_count() as u32;
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            let before = results.len();
            collect_mutable_nodes(child, source, changed_lines, skip_subtree_kinds, results);
            if results.len() > before {
                found_child = true;
            }
        }
    }

    // Only add this node if no relevant children were found and it's a mutable kind
    if !found_child && is_mutable_kind(node.kind()) {
        results.push(node);
    }
}

/// Check if a node's line range overlaps with any changed line range.
fn overlaps(node_start: usize, node_end: usize, changed_lines: &[LineRange]) -> bool {
    changed_lines
        .iter()
        .any(|range| node_start <= range.end && node_end >= range.start)
}

/// Check if a node kind is relevant for mutation.
fn is_mutable_kind(kind: &str) -> bool {
    MUTABLE_NODE_KINDS.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_go(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn finds_binary_expression_on_changed_lines() {
        let source = b"package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=return a+b, 5=}
        let tree = parse_go(source);
        let changed = vec![LineRange { start: 3, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(kinds.contains(&"binary_expression"));
    }

    #[test]
    fn no_changed_lines_returns_empty() {
        let source = b"package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n";
        let tree = parse_go(source);
        let changed: Vec<LineRange> = vec![];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(nodes.is_empty());
    }

    #[test]
    fn finds_if_statement_on_changed_line() {
        let source = b"package main\n\nfunc check(x int) int {\n\tif x > 0 {\n\t\treturn 1\n\t}\n\treturn 0\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=if x>0, 5=return 1, 6=}, 7=return 0, 8=}
        let tree = parse_go(source);
        let changed = vec![LineRange { start: 4, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        // Should find mutable nodes inside the if condition (deepest wins)
        assert!(
            kinds.contains(&"if_statement")
                || kinds.contains(&"binary_expression")
                || kinds.contains(&"int_literal"),
            "Expected a mutable node from the if line, got: {:?}",
            kinds
        );
    }

    #[test]
    fn multiple_overlapping_ranges() {
        let source = b"package main\n\nfunc f(a, b int) int {\n\tif a > 0 {\n\t\treturn a + b\n\t}\n\treturn a - b\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=if a>0, 5=return a+b, 6=}, 7=return a-b, 8=}
        let tree = parse_go(source);
        // Two ranges that both touch the function body
        let changed = vec![
            LineRange { start: 4, end: 5 },
            LineRange { start: 5, end: 7 },
        ];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

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
        let tree = parse_go(source);
        let changed = vec![LineRange { start: 4, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        // Deepest mutable node inside `return 42` is the int_literal
        assert!(
            kinds.contains(&"int_literal") || kinds.contains(&"return_statement"),
            "Expected return_statement or int_literal, got: {:?}",
            kinds
        );
    }

    #[test]
    fn finds_assignment_on_changed_line() {
        let source = b"package main\n\nfunc f() {\n\tx := 1\n\tx = x + 2\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=x:=1, 5=x=x+2, 6=}
        let tree = parse_go(source);
        let changed = vec![LineRange { start: 5, end: 5 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        // Should find the binary_expression (deepest) or assignment-related node
        assert!(
            kinds.contains(&"binary_expression")
                || kinds.contains(&"assignment_statement")
                || kinds.contains(&"expression_statement")
                || kinds.contains(&"int_literal"),
            "Expected mutable node on assignment line, got: {:?}",
            kinds
        );
    }

    #[test]
    fn nested_if_inside_if() {
        let source = b"package main\n\nfunc f(a, b int) int {\n\tif a > 0 {\n\t\tif b > 0 {\n\t\t\treturn a + b\n\t\t}\n\t}\n\treturn 0\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=if a>0, 5=if b>0, 6=return a+b, 7=}, 8=}, 9=return 0, 10=}
        let tree = parse_go(source);
        // Only the inner if line is changed
        let changed = vec![LineRange { start: 5, end: 6 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        // Should find deepest mutable nodes (binary_expression, int_literal) not outer if
        assert!(
            kinds.contains(&"binary_expression") || kinds.contains(&"int_literal"),
            "Expected deepest nested nodes, got: {:?}",
            kinds
        );
    }

    #[test]
    fn binary_expression_inside_function_call() {
        let source =
            b"package main\n\nimport \"fmt\"\n\nfunc f(a, b int) {\n\tfmt.Println(a + b)\n}\n";
        // Lines: 1=package, 2=empty, 3=import, 4=empty, 5=func, 6=fmt.Println(a+b), 7=}
        let tree = parse_go(source);
        let changed = vec![LineRange { start: 6, end: 6 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

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
        let tree = parse_go(source);
        let changed = vec![LineRange { start: 3, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

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
        let tree = parse_go(source);
        // Only the package line is changed
        let changed = vec![LineRange { start: 1, end: 1 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(
            nodes.is_empty(),
            "Package declaration should not produce mutable nodes, got: {:?}",
            nodes.iter().map(|n| n.kind()).collect::<Vec<_>>()
        );
    }

    fn parse_rust(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_rust::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn rust_return_expression_on_changed_line() {
        let source = b"fn add(a: i32, b: i32) -> i32 {\n    return a + b;\n}\n";
        // Lines: 1=fn, 2=return a+b, 3=}
        let tree = parse_rust(source);
        let changed = vec![LineRange { start: 2, end: 2 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"binary_expression")
                || kinds.contains(&"return_expression")
                || kinds.contains(&"integer_literal"),
            "Expected mutable node from Rust return line, got: {:?}",
            kinds
        );
    }

    #[test]
    fn rust_use_declaration_skipped() {
        let source = b"use std::collections::HashMap;\nfn f() -> bool { true }\n";
        let tree = parse_rust(source);
        let changed = vec![LineRange { start: 1, end: 2 }];

        let skip = &["use_declaration"];
        let nodes = find_mutable_nodes(&tree, source, &changed, skip);

        // Should find `true` in the function but nothing from the use declaration
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            !kinds.contains(&"string_literal") && !kinds.contains(&"identifier"),
            "Use declaration should be skipped, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"true"),
            "Should still find mutable nodes outside skipped ancestors, got: {:?}",
            kinds
        );
    }

    #[test]
    fn rust_macro_invocation_skipped() {
        let source = b"fn f() {\n    println!(\"hello\");\n    let x = true;\n}\n";
        let tree = parse_rust(source);
        let changed = vec![LineRange { start: 1, end: 4 }];

        let skip = &["macro_invocation"];
        let nodes = find_mutable_nodes(&tree, source, &changed, skip);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            !kinds.contains(&"string_literal") && !kinds.contains(&"string_content"),
            "Macro string should be skipped, got: {:?}",
            kinds
        );
        assert!(
            kinds.contains(&"true"),
            "Should still find `true` outside macro, got: {:?}",
            kinds
        );
    }

    #[test]
    fn go_import_skipped() {
        let source = b"package main\n\nimport \"fmt\"\n\nfunc f() bool {\n\treturn true\n}\n";
        let tree = parse_go(source);
        let changed = vec![LineRange { start: 1, end: 7 }];

        let skip = &["import_spec"];
        let nodes = find_mutable_nodes(&tree, source, &changed, skip);

        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            !kinds.contains(&"interpreted_string_literal"),
            "Import string should be skipped, got: {:?}",
            kinds
        );
    }

    fn parse_typescript(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(source, None).unwrap()
    }

    fn parse_java(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_java::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(source, None).unwrap()
    }

    fn parse_csharp(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_c_sharp::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn typescript_import_skipped() {
        let source = b"import { foo } from 'bar';\nconst x = true;\n";
        let tree = parse_typescript(source);
        let changed = vec![LineRange { start: 1, end: 2 }];

        let skip = &["import_statement"];
        let nodes = find_mutable_nodes(&tree, source, &changed, skip);

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
        let tree = parse_typescript(source);
        let changed = vec![LineRange { start: 1, end: 1 }];

        let skip = &["type_annotation"];
        let nodes = find_mutable_nodes(&tree, source, &changed, skip);

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
        let tree = parse_java(source);
        let changed = vec![LineRange { start: 1, end: 2 }];

        let skip = &["import_declaration"];
        let nodes = find_mutable_nodes(&tree, source, &changed, skip);

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
        let tree = parse_csharp(source);
        let changed = vec![LineRange { start: 1, end: 2 }];

        let skip = &["using_directive"];
        let nodes = find_mutable_nodes(&tree, source, &changed, skip);

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
        let tree = parse_rust(source);
        let changed = vec![LineRange { start: 3, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed, &[]);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(
            kinds.contains(&"binary_expression") || kinds.contains(&"integer_literal"),
            "Expected deepest nodes from nested Rust if, got: {:?}",
            kinds
        );
    }
}
