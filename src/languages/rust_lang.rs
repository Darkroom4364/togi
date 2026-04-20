/// Rust tree-sitter node mappings
pub struct Rust;

impl crate::languages::LanguageSupport for Rust {
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
