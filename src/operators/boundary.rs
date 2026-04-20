use super::MutationOperator;
use super::binary::find_operator_child;
use crate::MutationCandidate;

const BINARY_EXPR_KINDS: &[&str] = &["binary_expression", "binary_expr", "comparison_expression"];

fn is_binary_expr(node: &tree_sitter::Node) -> bool {
    BINARY_EXPR_KINDS.contains(&node.kind())
}

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

    fn parse_go(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn find_first_kind<'a>(
        node: tree_sitter::Node<'a>,
        kind: &str,
    ) -> Option<tree_sitter::Node<'a>> {
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
    fn test_plus_to_minus() {
        let src = "package main\nfunc f(a, b int) int { return a + b }";
        let tree = parse_go(src);
        let bin = find_first_kind(tree.root_node(), "binary_expression").unwrap();
        let candidates = PlusToMinus.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "-");
    }

    #[test]
    fn test_minus_to_plus() {
        let src = "package main\nfunc f(a, b int) int { return a - b }";
        let tree = parse_go(src);
        let bin = find_first_kind(tree.root_node(), "binary_expression").unwrap();
        let candidates = MinusToPlus.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "+");
    }

    #[test]
    fn test_no_match_on_comparison() {
        let src = "package main\nfunc f(a, b int) bool { return a < b }";
        let tree = parse_go(src);
        let bin = find_first_kind(tree.root_node(), "binary_expression").unwrap();
        let candidates = PlusToMinus.apply(&bin, src.as_bytes());
        assert!(candidates.is_empty());
    }
}
