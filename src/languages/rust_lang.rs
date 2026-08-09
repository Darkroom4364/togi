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

    fn should_filter_candidate(
        &self,
        candidate: &crate::MutationCandidate,
        node: &tree_sitter::Node,
        source: &[u8],
    ) -> bool {
        match candidate.operator_id.as_str() {
            "remove_if_body" | "remove_else" => {
                rust_if_removal_is_type_unsafe(candidate.operator_id.as_str(), node, source)
            }
            "return_empty" => crate::languages::should_skip_return_empty_for_type(node, false),
            "string_to_empty" => {
                crate::languages::should_skip_string_to_empty_in_compiled_context(node)
            }
            _ => false,
        }
    }
}

/// Removing an if branch changes its type independently of whether the
/// enclosing if result is ignored. Retain candidates only when syntax proves
/// the replacement is unit-compatible with its untouched branch; without an
/// alternative, replacing the consequence with `{}` is itself unit.
fn rust_if_removal_is_type_unsafe(operator: &str, node: &tree_sitter::Node, source: &[u8]) -> bool {
    if node.kind() != "if_expression" {
        return true;
    }
    let Some(consequence) = node.child_by_field_name("consequence") else {
        return true;
    };
    match operator {
        "remove_if_body" => node
            .child_by_field_name("alternative")
            .is_some_and(|alternative| !is_provably_unit_branch(&alternative, source)),
        "remove_else" => !is_provably_unit_branch(&consequence, source),
        _ => false,
    }
}

/// Return whether syntax proves that this branch evaluates to `()`.
fn is_provably_unit_branch(node: &tree_sitter::Node, source: &[u8]) -> bool {
    match node.kind() {
        "unit_expression" => true,
        "block" => {
            let mut cursor = node.walk();
            match node.named_children(&mut cursor).last() {
                None => true,
                Some(last) => {
                    last.kind() == "unit_expression"
                        || source_ends_with_semicolon(&last, source)
                        || unsemicolonated_tail_if_is_provably_unit(&last, source)
                }
            }
        }
        "else_clause" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .next()
                .is_some_and(|branch| is_provably_unit_branch(&branch, source))
        }
        "if_expression" => {
            let Some(consequence) = node.child_by_field_name("consequence") else {
                return false;
            };
            is_provably_unit_branch(&consequence, source)
                && node
                    .child_by_field_name("alternative")
                    .is_none_or(|alternative| is_provably_unit_branch(&alternative, source))
        }
        _ => false,
    }
}

/// Reuse the if proof after the enclosing block rules out a trailing semicolon.
fn unsemicolonated_tail_if_is_provably_unit(node: &tree_sitter::Node, source: &[u8]) -> bool {
    if node.kind() != "expression_statement" {
        return false;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next().is_some_and(|tail| {
        tail.kind() == "if_expression" && is_provably_unit_branch(&tail, source)
    })
}

/// A semicolon discards a statement value, making the containing block unit.
fn source_ends_with_semicolon(node: &tree_sitter::Node, source: &[u8]) -> bool {
    source
        .get(node.byte_range())
        .and_then(|text| text.iter().rev().find(|&&byte| !byte.is_ascii_whitespace()))
        == Some(&b';')
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
    use crate::test_helpers::{find_node_by_kind, walk_for_kind, walk_for_two_kinds};

    fn candidate(operator_id: &str) -> crate::MutationCandidate {
        crate::MutationCandidate {
            byte_range: 0..1,
            replacement: String::new(),
            operator_id: operator_id.to_string(),
            description: String::new(),
        }
    }

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
    fn string_to_empty_skipped_for_const_context() {
        let rs = Rust;
        let src = r#"const NAME: &str = "togi";"#;
        let tree = crate::test_helpers::parse_rust(src);
        let string = find_node_by_kind(tree.root_node(), "string_literal")
            .expect("should find string_literal node");

        assert!(rs.should_filter_candidate(&candidate("string_to_empty"), &string, src.as_bytes()));
    }

    #[test]
    fn string_to_empty_skipped_for_static_context() {
        let rs = Rust;
        let src = r#"static NAME: &str = "togi";"#;
        let tree = crate::test_helpers::parse_rust(src);
        let string = find_node_by_kind(tree.root_node(), "string_literal")
            .expect("should find string_literal node");

        assert!(rs.should_filter_candidate(&candidate("string_to_empty"), &string, src.as_bytes()));
    }

    #[test]
    fn string_to_empty_skipped_for_match_arm() {
        let rs = Rust;
        let src = r#"fn label(x: i32) -> &'static str {
    match x {
        0 => "zero",
        _ => "other",
    }
}"#;
        let tree = crate::test_helpers::parse_rust(src);
        let string = find_node_by_kind(tree.root_node(), "string_literal")
            .expect("should find string_literal node");

        assert!(rs.should_filter_candidate(&candidate("string_to_empty"), &string, src.as_bytes()));
    }

    #[test]
    fn should_skip_qualified_test_attribute() {
        let rs = Rust;
        let src = b"#[tokio::test]\nasync fn it_runs() {}\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let func =
            find_first(tree.root_node(), "function_item").expect("function_item should be present");
        assert!(rs.should_skip_node(&func, src));
    }

    #[test]
    fn should_skip_double_colon_test_attribute() {
        let rs = Rust;
        let src = b"#[test_case::test(42)]\nfn generated_case() {}\n";
        let tree = crate::test_helpers::parse_rust(std::str::from_utf8(src).unwrap());
        let func =
            find_first(tree.root_node(), "function_item").expect("function_item should be present");
        assert!(rs.should_skip_node(&func, src));
    }

    #[test]
    fn should_skip_plain_test_attribute() {
        let rs = Rust;
        let src = b"#[test(foo)]\nfn parameterized_case() {}\n";
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
