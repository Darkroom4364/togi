use crate::MutationCandidate;
use super::MutationOperator;
use super::binary::find_operator_child;

const BINARY_EXPR_KINDS: &[&str] = &["binary_expression", "binary_expr", "comparison_expression"];

fn is_binary_expr(node: &tree_sitter::Node) -> bool {
    BINARY_EXPR_KINDS.contains(&node.kind())
}

pub struct PlusToMinus;

impl MutationOperator for PlusToMinus {
    fn id(&self) -> &str { "plus_to_minus" }
    fn description(&self) -> &str { "Replace + with -" }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if !is_binary_expr(node) {
            return vec![];
        }
        if let Some(range) = find_operator_child(node, source, "+") {
            vec![MutationCandidate {
                byte_range: range,
                replacement: "-".to_string(),
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
        } else {
            vec![]
        }
    }
}

pub struct MinusToPlus;

impl MutationOperator for MinusToPlus {
    fn id(&self) -> &str { "minus_to_plus" }
    fn description(&self) -> &str { "Replace - with +" }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if !is_binary_expr(node) {
            return vec![];
        }
        if let Some(range) = find_operator_child(node, source, "-") {
            vec![MutationCandidate {
                byte_range: range,
                replacement: "+".to_string(),
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
        } else {
            vec![]
        }
    }
}
