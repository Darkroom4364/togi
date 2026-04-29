crate::languages::define_language!(
    CSharp,
    name: "c_sharp",
    extensions: ["cs"],
    ts_language: tree_sitter_c_sharp::LANGUAGE,
    skip_subtree_kinds: ["using_directive", "attribute", "type_parameter_list"],
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
