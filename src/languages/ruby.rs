crate::languages::define_language!(
    Ruby,
    name: "ruby",
    extensions: ["rb"],
    ts_language: tree_sitter_ruby::LANGUAGE,
    binary_expression: "binary",
    if_statement: "if",
    return_statement: "return",
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;

    #[test]
    fn test_ruby_extension_detection() {
        let lang = Ruby;
        assert_eq!(lang.name(), "ruby");
        assert!(lang.extensions().contains(&"rb"));
        assert!(!lang.extensions().contains(&"txt"));
    }

    #[test]
    fn test_ruby_parse_if_statement() {
        let lang = Ruby;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "if x < 10\n  puts x\nend\n";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("if"));
    }

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
