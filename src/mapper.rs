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
) -> Vec<tree_sitter::Node<'a>> {
    let mut nodes = Vec::new();
    let root = tree.root_node();
    collect_mutable_nodes(root, source, changed_lines, &mut nodes);
    nodes
}

/// Recursively walk the tree, collecting the deepest mutation-relevant nodes
/// that overlap with changed lines.
fn collect_mutable_nodes<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a [u8],
    changed_lines: &[LineRange],
    results: &mut Vec<tree_sitter::Node<'a>>,
) {
    let _ = source; // available for future use

    let node_start = ts_row_to_line(node.start_position().row);
    let node_end = ts_row_to_line(node.end_position().row);

    // Check if this node overlaps any changed line range
    if !overlaps(node_start, node_end, changed_lines) {
        return;
    }

    // Try to find relevant children first (prefer deepest nodes)
    let mut found_child = false;
    let child_count = node.child_count() as u32;
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            let before = results.len();
            collect_mutable_nodes(child, source, changed_lines, results);
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

        let nodes = find_mutable_nodes(&tree, source, &changed);

        assert!(!nodes.is_empty());
        let kinds: Vec<&str> = nodes.iter().map(|n| n.kind()).collect();
        assert!(kinds.contains(&"binary_expression"));
    }

    #[test]
    fn no_changed_lines_returns_empty() {
        let source = b"package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n";
        let tree = parse_go(source);
        let changed: Vec<LineRange> = vec![];

        let nodes = find_mutable_nodes(&tree, source, &changed);

        assert!(nodes.is_empty());
    }

    #[test]
    fn finds_if_statement_on_changed_line() {
        let source = b"package main\n\nfunc check(x int) int {\n\tif x > 0 {\n\t\treturn 1\n\t}\n\treturn 0\n}\n";
        // Lines: 1=package, 2=empty, 3=func, 4=if x>0, 5=return 1, 6=}, 7=return 0, 8=}
        let tree = parse_go(source);
        let changed = vec![LineRange { start: 4, end: 4 }];

        let nodes = find_mutable_nodes(&tree, source, &changed);

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
}
