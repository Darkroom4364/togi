crate::languages::define_language!(
    Go,
    name: "go",
    extensions: ["go"],
    ts_language: tree_sitter_go::LANGUAGE,
    skip_subtree_kinds: ["import_spec", "import_spec_list", "type_spec"],
    filter_candidate: should_filter_candidate,
);

fn should_filter_candidate(
    candidate: &crate::MutationCandidate,
    node: &tree_sitter::Node,
    source: &[u8],
) -> bool {
    match candidate.operator_id.as_str() {
        // Skip arithmetic mutations on expressions like `x * 1`.
        // `x * 1` -> `x / 1` is equivalent, but `1 * x` -> `1 / x` is not.
        "mul_to_div" | "div_to_mul" => has_rhs_literal_one(node, source),
        "string_to_empty" => {
            crate::languages::should_skip_string_to_empty_in_compiled_context(node)
        }
        _ => false,
    }
}

fn has_rhs_literal_one(node: &tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    if let Some(rhs) = children.last() {
        let text = std::str::from_utf8(&source[rhs.byte_range()]).unwrap_or("");
        return text == "1";
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;
    use crate::test_helpers::{find_node_by_kind, parse_go, walk_for_kind, walk_for_two_kinds};

    fn candidate(operator_id: &str) -> crate::MutationCandidate {
        crate::MutationCandidate {
            byte_range: 0..1,
            replacement: String::new(),
            operator_id: operator_id.to_string(),
            description: String::new(),
        }
    }

    #[test]
    fn test_go_extension_detection() {
        let go = Go;
        assert_eq!(go.extensions(), &["go"]);
        assert_eq!(go.name(), "go");
    }

    #[test]
    fn test_go_parse_binary_expression() {
        let go = Go;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&go.tree_sitter_language()).unwrap();

        let code = "package main\nfunc f() int { return a + b }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        walk_for_kind(&mut cursor, go.binary_expression_node(), &mut found);
        assert!(
            found,
            "Expected to find '{}' node in Go AST",
            go.binary_expression_node()
        );
    }

    #[test]
    fn test_go_parse_if_statement() {
        let go = Go;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&go.tree_sitter_language()).unwrap();

        let code = "package main\nfunc f() { if x < 10 { return } }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        walk_for_kind(&mut cursor, go.if_statement_node(), &mut found);
        assert!(
            found,
            "Expected to find '{}' node in Go AST",
            go.if_statement_node()
        );
    }

    #[test]
    fn test_go_parse_function_with_return() {
        let go = Go;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&go.tree_sitter_language()).unwrap();

        let code = "package main\nfunc add(a, b int) int { return a + b }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found_return = false;
        let mut found_binary = false;
        let mut cursor = root.walk();
        walk_for_two_kinds(
            &mut cursor,
            go.return_statement_node(),
            go.binary_expression_node(),
            &mut found_return,
            &mut found_binary,
        );
        assert!(found_return, "Expected return_statement node");
        assert!(found_binary, "Expected binary_expression node");
    }

    #[test]
    fn rhs_literal_one_detects_right_hand_one_only() {
        let src = "package main\nfunc f(x int) int { return x * 1 }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression")
            .expect("should find binary_expression node");

        assert!(has_rhs_literal_one(&bin, src.as_bytes()));
    }

    #[test]
    fn rhs_literal_one_ignores_left_hand_one() {
        let src = "package main\nfunc f(x int) int { return 1 * x }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression")
            .expect("should find binary_expression node");

        assert!(!has_rhs_literal_one(&bin, src.as_bytes()));
    }

    #[test]
    fn rhs_literal_one_ignores_other_literals() {
        let src = "package main\nfunc f(x int) int { return x * 2 }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression")
            .expect("should find binary_expression node");

        assert!(!has_rhs_literal_one(&bin, src.as_bytes()));
    }

    #[test]
    fn string_to_empty_allowed_inside_function_body() {
        let go = Go;
        let src = r#"package main
func f() string {
	return "hello"
}"#;
        let tree = parse_go(src);
        let string = find_node_by_kind(tree.root_node(), "interpreted_string_literal")
            .expect("should find interpreted_string_literal node");

        assert!(!go.should_filter_candidate(
            &candidate("string_to_empty"),
            &string,
            src.as_bytes()
        ));
    }
}
