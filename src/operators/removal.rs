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
