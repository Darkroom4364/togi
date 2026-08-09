crate::languages::define_language!(
    Python,
    name: "python",
    extensions: ["py"],
    ts_language: tree_sitter_python::LANGUAGE,
    binary_expression: "binary_operator",
    binary_operator_nodes: ["comparison_operator", "boolean_operator"],
    binary_operator_tokens: python_binary_operator_tokens,
    bool_true: ["True"],
    bool_false: ["False"],
    filter_candidate: should_filter_candidate,
    condition_negation: python_negation,
    empty_block_replacement: "pass",
);

fn python_negation(condition: &str) -> String {
    format!("not ({condition})")
}

fn python_binary_operator_tokens(operator_id: &str) -> Option<(&'static str, &'static str)> {
    match operator_id {
        "and_to_or" => Some(("and", "or")),
        "or_to_and" => Some(("or", "and")),
        _ => None,
    }
}

fn should_filter_candidate(
    candidate: &crate::MutationCandidate,
    node: &tree_sitter::Node,
    _source: &[u8],
) -> bool {
    if !matches!(
        candidate.operator_id.as_str(),
        "remove_call_statement" | "remove_assignment" | "remove_break" | "remove_continue"
    ) || !candidate.replacement.is_empty()
    {
        return false;
    }

    removal_would_empty_python_block(node)
}

fn removal_would_empty_python_block(node: &tree_sitter::Node) -> bool {
    let mut parent = node.parent();
    while let Some(p) = parent {
        if p.kind() == "block" {
            let mut cursor = p.walk();
            let mut named = p.named_children(&mut cursor);
            let Some(only_child) = named.next() else {
                return false;
            };
            return named.next().is_none() && only_child == *node;
        }
        parent = p.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MutationCandidate;
    use crate::languages::LanguageSupport;
    use crate::test_helpers::{find_node_by_kind, parse_python, walk_for_kind, walk_for_two_kinds};

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
    fn boolean_replacements_use_python_literals() {
        let py = Python;
        let mut true_to_false = MutationCandidate {
            byte_range: 0..4,
            replacement: "false".to_string(),
            operator_id: "true_to_false".to_string(),
            description: String::new(),
        };
        let mut false_to_true = MutationCandidate {
            byte_range: 0..5,
            replacement: "true".to_string(),
            operator_id: "false_to_true".to_string(),
            description: String::new(),
        };

        py.fixup_replacement(&mut true_to_false);
        py.fixup_replacement(&mut false_to_true);

        assert_eq!(true_to_false.replacement, "False");
        assert_eq!(false_to_true.replacement, "True");
    }

    #[test]
    fn return_empty_boolean_replacement_uses_python_false_literal() {
        let py = Python;
        let mut candidate = MutationCandidate {
            byte_range: 0..4,
            replacement: "false".to_string(),
            operator_id: "return_empty".to_string(),
            description: String::new(),
        };

        py.fixup_replacement(&mut candidate);

        assert_eq!(candidate.replacement, "False");
    }

    #[test]
    fn condition_negation_uses_python_syntax() {
        let py = Python;
        let mut candidate = MutationCandidate {
            byte_range: 0..5,
            replacement: "!(x > 0)".to_string(),
            operator_id: "negate_condition".to_string(),
            description: String::new(),
        };

        py.fixup_replacement(&mut candidate);

        assert_eq!(candidate.replacement, "not (x > 0)");
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

    #[test]
    fn removal_candidate_is_skipped_when_it_would_empty_a_block() {
        let src = "def f():\n    call()\n";
        let tree = parse_python(src);
        let statement = find_node_by_kind(tree.root_node(), "expression_statement")
            .expect("should find expression_statement node");
        let candidate = MutationCandidate {
            byte_range: statement.byte_range(),
            replacement: String::new(),
            operator_id: "remove_call_statement".to_string(),
            description: String::new(),
        };

        assert!(should_filter_candidate(
            &candidate,
            &statement,
            src.as_bytes()
        ));
    }

    #[test]
    fn removal_candidate_is_not_skipped_when_block_has_multiple_statements() {
        let src = "def f():\n    call()\n    other()\n";
        let tree = parse_python(src);
        let statement = find_node_by_kind(tree.root_node(), "expression_statement")
            .expect("should find expression_statement node");
        let candidate = MutationCandidate {
            byte_range: statement.byte_range(),
            replacement: String::new(),
            operator_id: "remove_call_statement".to_string(),
            description: String::new(),
        };

        assert!(!should_filter_candidate(
            &candidate,
            &statement,
            src.as_bytes()
        ));
    }
}
