use crate::languages::LanguageSupport;

pub struct CSharp;

impl LanguageSupport for CSharp {
    fn name(&self) -> &str {
        "c_sharp"
    }

    fn extensions(&self) -> &[&str] {
        &["cs"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_c_sharp::LANGUAGE.into()
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
        &["using_directive", "attribute", "type_parameter_list"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csharp_extension_detection() {
        let lang = CSharp;
        assert_eq!(lang.name(), "c_sharp");
        assert!(lang.extensions().contains(&"cs"));
        assert!(!lang.extensions().contains(&"txt"));
    }

    #[test]
    fn test_csharp_parse_if_statement() {
        let lang = CSharp;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "class T { void F() { if (x < 10) { return; } } }";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("if_statement"));
    }

    #[test]
    fn parses_csharp_binary_expression() {
        let lang = CSharp;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "class T { bool F() { return a > b; } }";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("binary_expression"));
    }
}
