crate::languages::define_language!(
    Java,
    name: "java",
    extensions: ["java"],
    ts_language: tree_sitter_java::LANGUAGE,
    skip_subtree_kinds: ["import_declaration", "annotation", "type_parameters"],
    filter_candidate: should_filter_candidate,
);

fn should_filter_candidate(
    candidate: &crate::MutationCandidate,
    node: &tree_sitter::Node,
    _source: &[u8],
) -> bool {
    candidate.operator_id == "string_to_empty"
        && crate::languages::should_skip_string_to_empty_in_compiled_context(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;

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
