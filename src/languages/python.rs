crate::languages::define_language!(
    Python,
    name: "python",
    extensions: ["py"],
    ts_language: tree_sitter_python::LANGUAGE,
    binary_expression: "binary_operator",
    bool_true: ["True"],
    bool_false: ["False"],
    empty_block_replacement: "pass",
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MutationCandidate;
    use crate::languages::LanguageSupport;
    use crate::test_helpers::{walk_for_kind, walk_for_two_kinds};

    #[test]
    fn test_python_extension_detection() {
        let py = Python;
        assert_eq!(py.extensions(), &["py"]);
        assert_eq!(py.name(), "python");
    }

    #[test]
    fn test_python_parse_binary_expression() {
        let py = Python;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&py.tree_sitter_language()).unwrap();

        let code = "x = a + b\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        walk_for_kind(&mut cursor, py.binary_expression_node(), &mut found);
        assert!(
            found,
            "Expected to find '{}' node in Python AST",
            py.binary_expression_node()
        );
    }

    #[test]
    fn test_python_parse_if_statement() {
        let py = Python;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&py.tree_sitter_language()).unwrap();

        let code = "if x < 10:\n    pass\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        walk_for_kind(&mut cursor, py.if_statement_node(), &mut found);
        assert!(
            found,
            "Expected to find '{}' node in Python AST",
            py.if_statement_node()
        );
    }

    #[test]
    fn test_python_parse_function() {
        let py = Python;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&py.tree_sitter_language()).unwrap();

        let code = "def add(a, b):\n    return a + b\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found_return = false;
        let mut found_binary = false;
        let mut cursor = root.walk();
        walk_for_two_kinds(
            &mut cursor,
            py.return_statement_node(),
            py.binary_expression_node(),
            &mut found_return,
            &mut found_binary,
        );
        assert!(found_return, "Expected return_statement node");
        assert!(found_binary, "Expected binary_operator node");
    }

    #[test]
    fn remove_if_body_replacement_uses_pass() {
        let py = Python;
        let mut candidate = MutationCandidate {
            byte_range: 0..2,
            replacement: "{}".to_string(),
            operator_id: "remove_if_body".to_string(),
            description: String::new(),
        };

        py.fixup_replacement(&mut candidate);

        assert_eq!(candidate.replacement, "pass");
    }

    #[test]
    fn non_remove_if_body_replacement_is_unchanged() {
        let py = Python;
        let mut candidate = MutationCandidate {
            byte_range: 0..2,
            replacement: "{}".to_string(),
            operator_id: "remove_else".to_string(),
            description: String::new(),
        };

        py.fixup_replacement(&mut candidate);

        assert_eq!(candidate.replacement, "{}");
    }
}
