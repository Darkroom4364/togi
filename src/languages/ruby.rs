use crate::languages::LanguageSupport;

pub struct Ruby;

impl LanguageSupport for Ruby {
    fn name(&self) -> &str {
        "ruby"
    }

    fn extensions(&self) -> &[&str] {
        &["rb"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_ruby::LANGUAGE.into()
    }

    fn binary_expression_node(&self) -> &str {
        "binary"
    }

    fn if_statement_node(&self) -> &str {
        "if"
    }

    fn boolean_true_literals(&self) -> &[&str] {
        &["true"]
    }

    fn boolean_false_literals(&self) -> &[&str] {
        &["false"]
    }

    fn return_statement_node(&self) -> &str {
        "return"
    }

    fn operator_field(&self) -> &str {
        "operator"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ruby_binary_expression() {
        let lang = Ruby;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "def f\n  a > b\nend\n";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("binary"));
    }
}
