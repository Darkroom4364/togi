use crate::languages::LanguageSupport;

pub struct Rust;

impl LanguageSupport for Rust {
    fn name(&self) -> &str {
        "rust"
    }
    fn extensions(&self) -> &[&str] {
        &["rs"]
    }
    fn tree_sitter_language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }
    fn binary_expression_node(&self) -> &str {
        "binary_expression"
    }
    fn if_statement_node(&self) -> &str {
        "if_expression"
    }
    fn boolean_true_literals(&self) -> &[&str] {
        &["true"]
    }
    fn boolean_false_literals(&self) -> &[&str] {
        &["false"]
    }
    fn return_statement_node(&self) -> &str {
        "return_expression"
    }
    fn operator_field(&self) -> &str {
        "operator"
    }
    fn skip_subtree_kinds(&self) -> &[&str] {
        &[
            "use_declaration",
            "macro_invocation",
            "attribute_item",
            "type_parameters",
            "where_clause",
        ]
    }
    fn should_skip_node(&self, node: &tree_sitter::Node, source: &[u8]) -> bool {
        match node.kind() {
            "mod_item" => has_attribute(node, source, |attr| attr == "#[cfg(test)]"),
            "function_item" => has_attribute(node, source, |attr| {
                attr == "#[test]"
                    || attr.contains("::test]")
                    || attr.contains("::test(")
                    || attr.starts_with("#[test(")
            }),
            _ => false,
        }
    }
}

/// Check if a node has a preceding sibling `attribute_item` matching a predicate.
/// Normalizes whitespace before matching. Walks backward through consecutive attributes.
fn has_attribute(node: &tree_sitter::Node, source: &[u8], matches: impl Fn(&str) -> bool) -> bool {
    let mut sibling = node.prev_sibling();
    while let Some(sib) = sibling {
        if sib.kind() == "attribute_item" {
            if let Ok(text) = std::str::from_utf8(&source[sib.byte_range()]) {
                let normalized: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                if matches(&normalized) {
                    return true;
                }
            }
            sibling = sib.prev_sibling();
        } else {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{walk_for_kind, walk_for_two_kinds};

    #[test]
    fn test_rust_extension_detection() {
        let rs = Rust;
        assert_eq!(rs.extensions(), &["rs"]);
        assert_eq!(rs.name(), "rust");
    }

    #[test]
    fn test_rust_parse_binary_expression() {
        let rs = Rust;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&rs.tree_sitter_language()).unwrap();

        let code = "fn f() -> i32 { a + b }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        walk_for_kind(&mut cursor, rs.binary_expression_node(), &mut found);
        assert!(
            found,
            "Expected to find '{}' node in Rust AST",
            rs.binary_expression_node()
        );
    }

    #[test]
    fn test_rust_parse_if_expression() {
        let rs = Rust;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&rs.tree_sitter_language()).unwrap();

        let code = "fn f() { if x < 10 { return; } }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found = false;
        let mut cursor = root.walk();
        walk_for_kind(&mut cursor, rs.if_statement_node(), &mut found);
        assert!(
            found,
            "Expected to find '{}' node in Rust AST",
            rs.if_statement_node()
        );
    }

    #[test]
    fn test_rust_parse_function_with_return() {
        let rs = Rust;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&rs.tree_sitter_language()).unwrap();

        let code = "fn add(a: i32, b: i32) -> i32 { return a + b; }\n";
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        let mut found_return = false;
        let mut found_binary = false;
        let mut cursor = root.walk();
        walk_for_two_kinds(
            &mut cursor,
            rs.return_statement_node(),
            rs.binary_expression_node(),
            &mut found_return,
            &mut found_binary,
        );
        assert!(found_return, "Expected return_expression node");
        assert!(found_binary, "Expected binary_expression node");
    }

    #[test]
    fn test_rust_if_node_is_expression() {
        let rs = Rust;
        assert_eq!(rs.if_statement_node(), "if_expression");
        assert_eq!(rs.return_statement_node(), "return_expression");
    }

    /// Find the first node of the given kind in the tree (depth-first).
    fn find_first<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_first(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn should_skip_test_function() {
        let rs = Rust;
        let src = b"#[test]\nfn it_works() {}\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let func =
            find_first(tree.root_node(), "function_item").expect("function_item should be present");
        assert!(rs.should_skip_node(&func, src));
    }

    #[test]
    fn should_skip_cfg_test_module() {
        let rs = Rust;
        let src = b"#[cfg(test)]\nmod tests { fn x() {} }\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let m = find_first(tree.root_node(), "mod_item").expect("mod_item should be present");
        assert!(rs.should_skip_node(&m, src));
    }

    #[test]
    fn should_not_skip_plain_function() {
        let rs = Rust;
        let src = b"fn add(a: i32, b: i32) -> i32 { a + b }\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let func =
            find_first(tree.root_node(), "function_item").expect("function_item should be present");
        assert!(!rs.should_skip_node(&func, src));
    }

    #[test]
    fn should_not_skip_non_test_attributed_function() {
        let rs = Rust;
        let src = b"#[inline]\nfn fast() {}\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let func =
            find_first(tree.root_node(), "function_item").expect("function_item should be present");
        assert!(!rs.should_skip_node(&func, src));
    }

    #[test]
    fn should_skip_qualified_test_attribute() {
        // e.g. #[tokio::test] / #[test_case::test(...)]
        let rs = Rust;
        let src = b"#[tokio::test]\nasync fn it_runs() {}\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let func =
            find_first(tree.root_node(), "function_item").expect("function_item should be present");
        assert!(rs.should_skip_node(&func, src));
    }

    #[test]
    fn has_attribute_walks_consecutive_attributes() {
        // Two attributes precede the function; #[test] is the further one.
        // should_skip_node uses has_attribute under the hood; it must walk
        // past #[ignore] to find #[test].
        let rs = Rust;
        let src = b"#[test]\n#[ignore]\nfn skipped() {}\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let func =
            find_first(tree.root_node(), "function_item").expect("function_item should be present");
        assert!(rs.should_skip_node(&func, src));
    }

    #[test]
    fn has_attribute_returns_false_when_no_attribute() {
        let src = b"fn lonely() {}\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let func =
            find_first(tree.root_node(), "function_item").expect("function_item should be present");
        assert!(!has_attribute(&func, src, |a| a == "#[test]"));
    }

    #[test]
    fn has_attribute_normalizes_whitespace() {
        // Attribute with extra whitespace should still match the normalized form.
        let src = b"#[ cfg ( test ) ]\nmod m {}\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let m = find_first(tree.root_node(), "mod_item").expect("mod_item should be present");
        assert!(has_attribute(&m, src, |a| a == "#[cfg(test)]"));
    }
}
