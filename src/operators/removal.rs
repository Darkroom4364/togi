use super::{IF_STMT_KINDS, MutationOperator};
use crate::MutationCandidate;

pub struct RemoveIfBody;

impl MutationOperator for RemoveIfBody {
    fn id(&self) -> &str {
        "remove_if_body"
    }
    fn description(&self) -> &str {
        "Replace if body with empty block"
    }
    fn apply(&self, node: &tree_sitter::Node, _source: &[u8]) -> Vec<MutationCandidate> {
        if !IF_STMT_KINDS.contains(&node.kind()) {
            return vec![];
        }
        // Look for "consequence" or "body" field
        let body = node
            .child_by_field_name("consequence")
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
    fn id(&self) -> &str {
        "remove_else"
    }
    fn description(&self) -> &str {
        "Remove else clause"
    }
    fn apply(&self, node: &tree_sitter::Node, _source: &[u8]) -> Vec<MutationCandidate> {
        if !IF_STMT_KINDS.contains(&node.kind()) {
            return vec![];
        }
        // Look for "alternative" or "else" field
        let else_clause = node
            .child_by_field_name("alternative")
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

const EXPR_STMT_KINDS: &[&str] = &["expression_statement", "expression_stmt"];
const CALL_EXPR_KINDS: &[&str] = &[
    "call_expression",
    "call",
    "method_invocation",
    "invocation_expression",
];

pub struct RemoveCallStatement;

impl MutationOperator for RemoveCallStatement {
    fn id(&self) -> &str {
        "remove_call_statement"
    }
    fn description(&self) -> &str {
        "Remove void method/function call"
    }
    fn apply(&self, node: &tree_sitter::Node, _source: &[u8]) -> Vec<MutationCandidate> {
        if !EXPR_STMT_KINDS.contains(&node.kind()) {
            return vec![];
        }
        let mut cursor = node.walk();
        let has_call = node
            .named_children(&mut cursor)
            .any(|child| CALL_EXPR_KINDS.contains(&child.kind()));
        if has_call {
            vec![MutationCandidate {
                byte_range: node.byte_range(),
                replacement: String::new(),
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
        } else {
            vec![]
        }
    }
}

const ASSIGNMENT_KINDS: &[&str] = &[
    "assignment_statement",
    "assignment_expression",
    "assignment",
    "augmented_assignment",
    "augmented_assignment_expression",
];

pub struct RemoveAssignment;

impl MutationOperator for RemoveAssignment {
    fn id(&self) -> &str {
        "remove_assignment"
    }
    fn description(&self) -> &str {
        "Remove assignment statement"
    }
    fn apply(&self, node: &tree_sitter::Node, _source: &[u8]) -> Vec<MutationCandidate> {
        if !ASSIGNMENT_KINDS.contains(&node.kind()) {
            return vec![];
        }
        vec![MutationCandidate {
            byte_range: node.byte_range(),
            replacement: String::new(),
            operator_id: self.id().to_string(),
            description: self.description().to_string(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::{find_node_by_kind, parse_go};

    fn assert_single_candidate(
        candidates: &[MutationCandidate],
        src: &str,
        replacement: &str,
        original: &str,
    ) {
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, replacement);
        assert_eq!(&src[candidates[0].byte_range.clone()], original);
    }

    #[test]
    fn test_remove_if_body() {
        let src = r#"package main
func f(x int) { if x > 0 { println("yes") } }"#;
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = RemoveIfBody.apply(&if_node, src.as_bytes());
        assert_single_candidate(&candidates, src, "{}", r#"{ println("yes") }"#);
    }

    #[test]
    fn test_remove_else() {
        let src = "package main\nfunc f(x int) int { if x > 0 { return 1 } else { return 0 } }";
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = RemoveElse.apply(&if_node, src.as_bytes());
        assert_single_candidate(&candidates, src, "", "else { return 0 }");
    }

    #[test]
    fn test_remove_call_statement() {
        let src = "package main\nfunc f() { println(\"hi\") }";
        let tree = parse_go(src);
        let stmt = find_node_by_kind(tree.root_node(), "expression_statement")
            .expect("should find expression_statement node");
        let candidates = RemoveCallStatement.apply(&stmt, src.as_bytes());
        assert_single_candidate(&candidates, src, "", r#"println("hi")"#);
    }

    #[test]
    fn test_remove_call_statement_no_match_on_binary_expression() {
        let src = "package main\nfunc f() int { return x + y }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression")
            .expect("should find binary_expression node");
        let candidates = RemoveCallStatement.apply(&bin, src.as_bytes());
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_remove_assignment() {
        let src = "package main\nfunc f() { x = 1 }";
        let tree = parse_go(src);
        let stmt = find_node_by_kind(tree.root_node(), "assignment_statement")
            .expect("should find assignment_statement node");
        let candidates = RemoveAssignment.apply(&stmt, src.as_bytes());
        assert_single_candidate(&candidates, src, "", "x = 1");
    }

    #[test]
    fn test_remove_if_body_no_match_on_for() {
        let src = "package main\nfunc f() { for i := 0; i < 10; i++ { println(i) } }";
        let tree = parse_go(src);
        let for_node = find_node_by_kind(tree.root_node(), "for_statement").unwrap();
        let candidates = RemoveIfBody.apply(&for_node, src.as_bytes());
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_remove_else_no_else_clause() {
        let src = "package main\nfunc f(x int) { if x > 0 { println(x) } }";
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = RemoveElse.apply(&if_node, src.as_bytes());
        assert!(candidates.is_empty());
    }
}
