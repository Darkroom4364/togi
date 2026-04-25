crate::languages::define_language!(
    TypeScript,
    name: "typescript",
    extensions: ["ts", "tsx"],
    ts_language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
    skip_subtree_kinds: ["import_statement", "type_annotation", "type_alias_declaration", "decorator"],
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;
    use tree_sitter::Parser;

    #[test]
    fn test_extensions() {
        let ts = TypeScript;
        assert!(ts.extensions().contains(&"ts"));
        assert!(ts.extensions().contains(&"tsx"));
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

        assert!(find_node(root, TypeScript.binary_expression_node()));
    }
}
