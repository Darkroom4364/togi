/// Shared test utilities for tree-sitter parsing and node lookup.

/// Parse Go source code into a tree-sitter tree.
pub fn parse_go(src: &str) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_go::LANGUAGE;
    parser.set_language(&lang.into()).unwrap();
    parser.parse(src, None).unwrap()
}

/// Recursively find the first node matching `kind` in the tree.
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

/// Walk a tree cursor looking for a node of the given kind.
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
