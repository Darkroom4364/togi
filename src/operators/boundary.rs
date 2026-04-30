use super::binary::find_operator_child;
use super::{MutationOperator, is_binary_expr};
use crate::MutationCandidate;

pub struct PlusToMinus;

impl MutationOperator for PlusToMinus {
    fn id(&self) -> &str {
        "plus_to_minus"
    }
    fn description(&self) -> &str {
        "Replace + with -"
    }
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
    fn id(&self) -> &str {
        "minus_to_plus"
    }
    fn description(&self) -> &str {
        "Replace - with +"
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::{find_node_by_kind, parse_go};

    #[test]
    fn test_plus_to_minus() {
        let src = "package main\nfunc f(a, b int) int { return a + b }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression").unwrap();
        let candidates = PlusToMinus.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "-");
    }

    #[test]
    fn test_minus_to_plus() {
        let src = "package main\nfunc f(a, b int) int { return a - b }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression").unwrap();
        let candidates = MinusToPlus.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "+");
    }

    #[test]
    fn test_no_match_on_comparison() {
        let src = "package main\nfunc f(a, b int) bool { return a < b }";
        let tree = parse_go(src);
        let bin = find_node_by_kind(tree.root_node(), "binary_expression").unwrap();
        let candidates = PlusToMinus.apply(&bin, src.as_bytes());
        assert!(candidates.is_empty());
    }
}
