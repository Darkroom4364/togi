pub mod go;
pub mod rust_lang;

/// Language-specific configuration for tree-sitter parsing and node identification
pub trait LanguageSupport: Send + Sync {
    fn name(&self) -> &str;
    fn extensions(&self) -> &[&str];
    fn tree_sitter_language(&self) -> tree_sitter::Language;
    fn binary_expression_node(&self) -> &str;
    fn if_statement_node(&self) -> &str;
    fn boolean_true_literals(&self) -> &[&str];
    fn boolean_false_literals(&self) -> &[&str];
    fn return_statement_node(&self) -> &str;
    fn operator_field(&self) -> &str;
}

/// Returns all supported languages.
pub fn all() -> Vec<Box<dyn LanguageSupport>> {
    vec![
        Box::new(go::Go),
        Box::new(rust_lang::Rust),
    ]
}

/// Detects a language by file extension.
pub fn detect(extension: &str) -> Option<Box<dyn LanguageSupport>> {
    all().into_iter().find(|l| l.extensions().contains(&extension))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_parse_node_kinds() {
        let lang = go::Go;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();

        let code = r#"
package main

func add(a int, b int) int {
    if a > b {
        return a
    }
    return a + b
}
"#;
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        assert!(find_node_kind(&root, lang.if_statement_node()));
        assert!(find_node_kind(&root, lang.binary_expression_node()));
        assert!(find_node_kind(&root, lang.return_statement_node()));

        // Verify operator field on binary_expression
        let bin_expr = find_first_node_kind(&root, lang.binary_expression_node()).unwrap();
        assert!(bin_expr.child_by_field_name(lang.operator_field()).is_some());
    }

    #[test]
    fn test_rust_parse_node_kinds() {
        let lang = rust_lang::Rust;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();

        let code = r#"
fn add(a: i32, b: i32) -> i32 {
    if a > b {
        return a;
    }
    a + b
}
"#;
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        assert!(find_node_kind(&root, lang.if_statement_node()));
        assert!(find_node_kind(&root, lang.binary_expression_node()));
        assert!(find_node_kind(&root, lang.return_statement_node()));

        // Verify operator field on binary_expression
        let bin_expr = find_first_node_kind(&root, lang.binary_expression_node()).unwrap();
        assert!(bin_expr.child_by_field_name(lang.operator_field()).is_some());
    }

    #[test]
    fn test_detect_by_extension() {
        let go = detect("go").unwrap();
        assert_eq!(go.name(), "go");

        let rs = detect("rs").unwrap();
        assert_eq!(rs.name(), "rust");

        assert!(detect("py").is_none());
    }

    fn find_node_kind(node: &tree_sitter::Node, kind: &str) -> bool {
        if node.kind() == kind {
            return true;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if find_node_kind(&child, kind) {
                return true;
            }
        }
        false
    }

    fn find_first_node_kind<'a>(node: &tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(*node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_first_node_kind(&child, kind) {
                return Some(found);
            }
        }
        None
    }
}
