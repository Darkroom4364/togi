/// Shared test utilities.
/// Used across operator, language, and report tests to avoid duplication.
use crate::{Mutation, MutationReport, MutationResult};
use std::path::PathBuf;
use std::time::Duration;

/// A standard two-mutation report for testing report formatters.
pub fn sample_report() -> MutationReport {
    MutationReport {
        results: vec![
            (
                Mutation {
                    id: 1,
                    file: PathBuf::from("src/auth.rs"),
                    language: String::new(),
                    line: 47,
                    column: 10,
                    operator: "binary/lt_to_lte".to_string(),
                    description: "changed < to <=".to_string(),
                    original: "<".to_string(),
                    replacement: "<=".to_string(),
                    byte_range: 0..1,
                },
                MutationResult::Killed,
            ),
            (
                Mutation {
                    id: 2,
                    file: PathBuf::from("src/handler.rs"),
                    language: String::new(),
                    line: 15,
                    column: 5,
                    operator: "binary/eq_to_neq".to_string(),
                    description: "changed == to !=".to_string(),
                    original: "==".to_string(),
                    replacement: "!=".to_string(),
                    byte_range: 0..2,
                },
                MutationResult::Survived,
            ),
        ],
        duration: Duration::from_millis(1234),
        total: 2,
        killed: 1,
        survived: 1,
        timeout: 0,
        build_errors: 0,
    }
}

/// Parse Go source code into a tree-sitter tree.
pub fn parse_go(src: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_go::LANGUAGE;
    parser.set_language(&lang.into()).unwrap();
    parser.parse(src, None).unwrap()
}

/// Recursively find the first node matching `kind` in the tree.
/// Visits all children (named and anonymous).
pub fn find_node_by_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_node_by_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

/// Walk a tree cursor looking for a node of the given kind, setting `found` to true.
pub fn walk_for_kind(cursor: &mut tree_sitter::TreeCursor, target: &str, found: &mut bool) {
    if cursor.node().kind() == target {
        *found = true;
        return;
    }
    if cursor.goto_first_child() {
        loop {
            walk_for_kind(cursor, target, found);
            if *found || !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Walk a tree cursor looking for two node kinds simultaneously.
pub fn walk_for_two_kinds(
    cursor: &mut tree_sitter::TreeCursor,
    kind_a: &str,
    kind_b: &str,
    found_a: &mut bool,
    found_b: &mut bool,
) {
    let kind = cursor.node().kind();
    if kind == kind_a {
        *found_a = true;
    }
    if kind == kind_b {
        *found_b = true;
    }
    if cursor.goto_first_child() {
        loop {
            walk_for_two_kinds(cursor, kind_a, kind_b, found_a, found_b);
            if (*found_a && *found_b) || !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
