crate::languages::define_language!(
    C,
    name: "c",
    extensions: ["c", "h"],
    ts_language: tree_sitter_c::LANGUAGE,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;

    #[test]
    fn test_c_extension_detection() {
        let lang = C;
        assert_eq!(lang.name(), "c");
        assert!(lang.extensions().contains(&"c"));
        assert!(lang.extensions().contains(&"h"));
        assert!(!lang.extensions().contains(&"txt"));
    }

    #[test]
    fn test_c_parse_if_statement() {
        let lang = C;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "void f() { if (x < 10) { return; } }";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("if_statement"));
    }

    #[test]
    fn parses_c_binary_expression() {
        let lang = C;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "int f() { return a > b; }";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("binary_expression"));
    }
}
