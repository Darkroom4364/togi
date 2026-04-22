use crate::languages::LanguageSupport;

pub struct Rust;

impl LanguageSupport for Rust {
    fn name(&self) -> &str {
        "rust"
    }

    fn extensions(&self) -> &[&str] {
        &["rs"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn binary_expression_node(&self) -> &str {
        "binary_expression"
    }

    fn if_statement_node(&self) -> &str {
        "if_expression"
    }

    fn boolean_true_literals(&self) -> &[&str] {
        &["true"]
    }

    fn boolean_false_literals(&self) -> &[&str] {
        &["false"]
    }

    fn return_statement_node(&self) -> &str {
        "return_expression"
    }

    fn operator_field(&self) -> &str {
        "operator"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{walk_for_kind, walk_for_two_kinds};

    #[test]
    fn test_rust_extension_detection() {
        let rs = Rust;
        assert_eq!(rs.extensions(), &["rs"]);
        assert_eq!(rs.name(), "rust");
    }

    #[test]
    fn test_rust_parse_binary_expression() {
        let rs = Rust;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&rs.tree_sitter_language()).unwrap();

        let code = "fn f() -> i32 { a + b }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        walk_for_kind(&mut cursor, rs.binary_expression_node(), &mut found);
        assert!(
            found,
            "Expected to find '{}' node in Rust AST",
            rs.binary_expression_node()
        );
    }

    #[test]
    fn test_rust_parse_if_expression() {
        let rs = Rust;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&rs.tree_sitter_language()).unwrap();

        let code = "fn f() { if x < 10 { return; } }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        walk_for_kind(&mut cursor, rs.if_statement_node(), &mut found);
        assert!(
            found,
            "Expected to find '{}' node in Rust AST",
            rs.if_statement_node()
        );
    }

    #[test]
    fn test_rust_parse_function_with_return() {
        let rs = Rust;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&rs.tree_sitter_language()).unwrap();

        let code = "fn add(a: i32, b: i32) -> i32 { return a + b; }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found_return = false;
        let mut found_binary = false;
        let mut cursor = root.walk();
        walk_for_two_kinds(
            &mut cursor,
            rs.return_statement_node(),
            rs.binary_expression_node(),
            &mut found_return,
            &mut found_binary,
        );
        assert!(found_return, "Expected return_expression node");
        assert!(found_binary, "Expected binary_expression node");
    }

    #[test]
    fn test_rust_if_node_is_expression() {
        let rs = Rust;
        assert_eq!(rs.if_statement_node(), "if_expression");
        assert_eq!(rs.return_statement_node(), "return_expression");
    }
}
