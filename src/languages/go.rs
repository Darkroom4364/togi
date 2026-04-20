/// Go tree-sitter node mappings
pub struct Go;

impl crate::languages::LanguageSupport for Go {
    fn name(&self) -> &str {
        "go"
    }
    fn extensions(&self) -> &[&str] {
        &["go"]
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_go::LANGUAGE.into()
    }
    fn binary_expression_node(&self) -> &str {
        "binary_expression"
    }
    fn if_statement_node(&self) -> &str {
        "if_statement"
    }
    fn boolean_true_literals(&self) -> &[&str] {
        &["true"]
    }
    fn boolean_false_literals(&self) -> &[&str] {
        &["false"]
    }
    fn return_statement_node(&self) -> &str {
        "return_statement"
    }
    fn operator_field(&self) -> &str {
        "operator"
    }
}
