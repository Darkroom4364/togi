crate::languages::define_language!(
    Go,
    name: "go",
    extensions: ["go"],
    ts_language: tree_sitter_go::LANGUAGE,
    skip_subtree_kinds: ["import_spec", "import_spec_list", "type_spec"],
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;
    use crate::test_helpers::{walk_for_kind, walk_for_two_kinds};

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
}
