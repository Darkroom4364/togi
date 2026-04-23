pub mod c;
pub mod cpp;
pub mod csharp;
pub mod go;
pub mod java;
pub mod python;
pub mod ruby;
pub mod rust_lang;
pub mod typescript;

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

    /// AST node kinds that should suppress mutation of any descendant.
    /// Nodes whose ancestor matches any of these kinds will be skipped.
    fn skip_subtree_kinds(&self) -> &[&str] {
        &[]
    }
}

/// Returns instances of all supported languages.
pub fn all() -> Vec<Box<dyn LanguageSupport>> {
    vec![
        Box::new(c::C),
        Box::new(cpp::Cpp),
        Box::new(csharp::CSharp),
        Box::new(go::Go),
        Box::new(java::Java),
        Box::new(python::Python),
        Box::new(ruby::Ruby),
        Box::new(rust_lang::Rust),
        Box::new(typescript::TypeScript),
    ]
}
