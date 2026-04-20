use crate::languages::LanguageSupport;

pub struct Cpp;

impl LanguageSupport for Cpp {
    fn name(&self) -> &str {
        "cpp"
    }

    fn extensions(&self) -> &[&str] {
        &["cpp", "cc", "cxx", "hpp", "hxx"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_cpp::LANGUAGE.into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpp_binary_expression() {
        let lang = Cpp;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "bool f() { return a > b; }";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("binary_expression"));
    }
}
