crate::languages::define_language!(
    Cpp,
    name: "cpp",
    extensions: ["cpp", "cc", "cxx", "hpp", "hxx"],
    ts_language: tree_sitter_cpp::LANGUAGE,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;

    #[test]
    fn test_cpp_extension_detection() {
        let lang = Cpp;
        assert_eq!(lang.name(), "cpp");
        assert!(lang.extensions().contains(&"cpp"));
        assert!(lang.extensions().contains(&"hpp"));
        assert!(!lang.extensions().contains(&"txt"));
    }

    #[test]
    fn test_cpp_parse_if_statement() {
        let lang = Cpp;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang.tree_sitter_language()).unwrap();
        let code = "void f() { if (x < 10) { return; } }";
        let tree = parser.parse(code, None).unwrap();
        let src = tree.root_node().to_sexp();
        assert!(src.contains("if_statement"));
    }

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
