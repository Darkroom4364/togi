pub mod go;
pub mod python;
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

/// Returns instances of all supported languages.
pub fn all() -> Vec<Box<dyn LanguageSupport>> {
    vec![
        Box::new(go::Go),
        Box::new(python::Python),
        Box::new(rust_lang::Rust),
    ]
}
