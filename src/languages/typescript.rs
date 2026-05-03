crate::languages::define_language!(
    TypeScript,
    name: "typescript",
    extensions: ["ts"],
    ts_language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
    skip_subtree_kinds: ["import_statement", "type_annotation", "type_alias_declaration", "decorator"],
    filter_candidate: should_filter_candidate,
);

crate::languages::define_language!(
    Tsx,
    name: "typescript",
    extensions: ["tsx"],
    ts_language: tree_sitter_typescript::LANGUAGE_TSX,
    skip_subtree_kinds: ["import_statement", "type_annotation", "type_alias_declaration", "decorator"],
    filter_candidate: should_filter_candidate,
);

fn should_filter_candidate(
    candidate: &crate::MutationCandidate,
    node: &tree_sitter::Node,
    _source: &[u8],
) -> bool {
    match candidate.operator_id.as_str() {
        "return_empty" => crate::languages::should_skip_return_empty_for_type(node, false),
        "string_to_empty" => {
            crate::languages::should_skip_string_to_empty_in_compiled_context(node)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;
    use tree_sitter::Parser;

    fn find_node<'a>(node: tree_sitter::Node<'a>, kind: &str) -> bool {
        if node.kind() == kind {
            return true;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if find_node(child, kind) {
                return true;
            }
        }
        false
    }

    #[test]
    fn test_extensions() {
        let ts = TypeScript;
        let tsx = Tsx;
        assert!(ts.extensions().contains(&"ts"));
        assert!(!ts.extensions().contains(&"tsx"));
        assert!(tsx.extensions().contains(&"tsx"));
    }

    #[test]
    fn test_parse_typescript_function() {
        let mut parser = Parser::new();
        parser
            .set_language(&TypeScript.tree_sitter_language())
            .expect("Error loading TypeScript parser");

        let code = r#"
function add(a: number, b: number): number {
    if (a > b) {
        return a + b;
    }
    return true;
}
"#;
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();
        assert!(!root.has_error());
    }

    #[test]
    fn test_binary_expression_found() {
        let mut parser = Parser::new();
        parser
            .set_language(&TypeScript.tree_sitter_language())
            .expect("Error loading TypeScript parser");

        let code = "const x = a + b;";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        assert!(find_node(root, TypeScript.binary_expression_node()));
    }

    #[test]
    fn test_parse_tsx_jsx_expression() {
        let mut parser = Parser::new();
        parser
            .set_language(&Tsx.tree_sitter_language())
            .expect("Error loading TSX parser");

        let code = r#"
const View = ({ value }: { value: number }) => {
    return <div>{value > 0 ? "positive" : "zero"}</div>;
};
"#;
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();
        assert!(!root.has_error());
        assert!(find_node(root, Tsx.binary_expression_node()));
    }
}
