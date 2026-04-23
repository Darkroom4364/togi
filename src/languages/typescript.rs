use crate::languages::LanguageSupport;

pub struct TypeScript;

impl LanguageSupport for TypeScript {
    fn name(&self) -> &str {
        "typescript"
    }

    fn extensions(&self) -> &[&str] {
        &["ts", "tsx"]
    }

    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
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
        &[
            "import_statement",
            "type_annotation",
            "type_alias_declaration",
            "decorator",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
