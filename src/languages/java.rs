use crate::languages::LanguageSupport;

pub struct Java;

impl LanguageSupport for Java {
    fn name(&self) -> &str {
        "java"
    }

    fn extensions(&self) -> &[&str] {
        &["java"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
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

    fn skip_ancestor_kinds(&self) -> &[&str] {
        &["import_declaration", "annotation", "type_parameters"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_java_extension_detection() {
        let lang = Java;
        assert_eq!(lang.name(), "java");
        assert!(lang.extensions().contains(&"java"));
        assert!(!lang.extensions().contains(&"txt"));
    }

    #[test]
    fn test_java_parse_if_statement() {
        let lang = Java;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "class T { void f() { if (x < 10) { return; } } }";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("if_statement"));
    }

    #[test]
    fn parses_java_binary_expression() {
        let lang = Java;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "class T { boolean f() { return a > b; } }";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("binary_expression"));
    }
}
