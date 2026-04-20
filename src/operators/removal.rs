use crate::MutationCandidate;
use super::MutationOperator;

const IF_STMT_KINDS: &[&str] = &["if_statement", "if_expression", "if_expr"];

pub struct RemoveIfBody;

impl MutationOperator for RemoveIfBody {
    fn id(&self) -> &str { "remove_if_body" }
    fn description(&self) -> &str { "Replace if body with empty block" }
    fn apply(&self, node: &tree_sitter::Node, _source: &[u8]) -> Vec<MutationCandidate> {
        if !IF_STMT_KINDS.contains(&node.kind()) {
            return vec![];
        }
        // Look for "consequence" or "body" field
        let body = node.child_by_field_name("consequence")
            .or_else(|| node.child_by_field_name("body"));
        if let Some(body_node) = body {
            vec![MutationCandidate {
                byte_range: body_node.byte_range(),
                replacement: "{}".to_string(),
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
        } else {
            vec![]
        }
    }
}

pub struct RemoveElse;

impl MutationOperator for RemoveElse {
    fn id(&self) -> &str { "remove_else" }
    fn description(&self) -> &str { "Remove else clause" }
    fn apply(&self, node: &tree_sitter::Node, _source: &[u8]) -> Vec<MutationCandidate> {
        if !IF_STMT_KINDS.contains(&node.kind()) {
            return vec![];
        }
        // Look for "alternative" or "else" field
        let else_clause = node.child_by_field_name("alternative")
            .or_else(|| node.child_by_field_name("else"));
        if let Some(else_node) = else_clause {
            // Find the "else" keyword before the clause to remove it too
            // We remove from the else keyword (searching backwards) to end of else block
            let mut cursor = node.walk();
            let mut else_kw_start = else_node.start_byte();
            for child in node.children(&mut cursor) {
                if !child.is_named() {
                    let text_range = child.byte_range();
                    if text_range.end <= else_node.start_byte() {
                        let kind = child.kind();
                        if kind == "else" {
                            else_kw_start = child.start_byte();
                        }
                    }
                }
            }
            vec![MutationCandidate {
                byte_range: else_kw_start..else_node.end_byte(),
                replacement: String::new(),
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_go(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn find_first_kind<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_first_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_remove_if_body() {
        let src = r#"package main
func f(x int) { if x > 0 { println("yes") } }"#;
        let tree = parse_go(src);
        let if_node = find_first_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = RemoveIfBody.apply(&if_node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "{}");
    }

    #[test]
    fn test_remove_else() {
        let src = "package main\nfunc f(x int) int { if x > 0 { return 1 } else { return 0 } }";
        let tree = parse_go(src);
        let if_node = find_first_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = RemoveElse.apply(&if_node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "");
        // The byte range should cover the else clause
        let removed = &src[candidates[0].byte_range.clone()];
        assert!(removed.contains("else"));
    }

    #[test]
    fn test_remove_if_body_no_match_on_for() {
        let src = "package main\nfunc f() { for i := 0; i < 10; i++ { println(i) } }";
        let tree = parse_go(src);
        let for_node = find_first_kind(tree.root_node(), "for_statement").unwrap();
        let candidates = RemoveIfBody.apply(&for_node, src.as_bytes());
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_remove_else_no_else_clause() {
        let src = "package main\nfunc f(x int) { if x > 0 { println(x) } }";
        let tree = parse_go(src);
        let if_node = find_first_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = RemoveElse.apply(&if_node, src.as_bytes());
        assert!(candidates.is_empty());
    }
}
