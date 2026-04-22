use super::MutationOperator;
use crate::MutationCandidate;

const BINARY_EXPR_KINDS: &[&str] = &["binary_expression", "binary_expr", "comparison_expression"];

pub fn find_operator_child(
    node: &tree_sitter::Node,
    source: &[u8],
    target: &str,
) -> Option<std::ops::Range<usize>> {
    // Try field name "operator" first
    if let Some(op_node) = node.child_by_field_name("operator") {
        let text = &source[op_node.byte_range()];
        if text == target.as_bytes() {
            return Some(op_node.byte_range());
        }
    }
    // Iterate unnamed children looking for the operator token
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() {
            let text = &source[child.byte_range()];
            if text == target.as_bytes() {
                return Some(child.byte_range());
            }
        }
    }
    None
}

fn is_binary_expr(node: &tree_sitter::Node) -> bool {
    BINARY_EXPR_KINDS.contains(&node.kind())
}

macro_rules! binary_operator {
    ($name:ident, $id:expr, $desc:expr, $from:expr, $to:expr) => {
        pub struct $name;

        impl MutationOperator for $name {
            fn id(&self) -> &str {
                $id
            }
            fn description(&self) -> &str {
                $desc
            }
            fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
                if !is_binary_expr(node) {
                    return vec![];
                }
                if let Some(range) = find_operator_child(node, source, $from) {
                    vec![MutationCandidate {
                        byte_range: range,
                        replacement: $to.to_string(),
                        operator_id: self.id().to_string(),
                        description: self.description().to_string(),
                    }]
                } else {
                    vec![]
                }
            }
        }
    };
}

binary_operator!(LtToLte, "lt_to_lte", "Replace < with <=", "<", "<=");
binary_operator!(GtToGte, "gt_to_gte", "Replace > with >=", ">", ">=");
binary_operator!(EqToNeq, "eq_to_neq", "Replace == with !=", "==", "!=");
binary_operator!(AndToOr, "and_to_or", "Replace && with ||", "&&", "||");
binary_operator!(OrToAnd, "or_to_and", "Replace || with &&", "||", "&&");
binary_operator!(MulToDiv, "mul_to_div", "Replace * with /", "*", "/");
binary_operator!(DivToMul, "div_to_mul", "Replace / with *", "/", "*");
binary_operator!(ModToMul, "mod_to_mul", "Replace % with *", "%", "*");

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_go(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn find_binary_expr(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
        if is_binary_expr(&node) {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_binary_expr(child) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_lt_to_lte() {
        let src = "package main\nfunc f() bool { return x < y }";
        let tree = parse_go(src);
        let root = tree.root_node();
        let bin = find_binary_expr(root).expect("should find binary_expression");
        let candidates = LtToLte.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "<=");
    }

    #[test]
    fn test_gt_to_gte() {
        let src = "package main\nfunc f() bool { return x > y }";
        let tree = parse_go(src);
        let root = tree.root_node();
        let bin = find_binary_expr(root).expect("should find binary_expression");
        let candidates = GtToGte.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, ">=");
    }

    #[test]
    fn test_eq_to_neq() {
        let src = "package main\nfunc f() bool { return x == y }";
        let tree = parse_go(src);
        let root = tree.root_node();
        let bin = find_binary_expr(root).expect("should find binary_expression");
        let candidates = EqToNeq.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "!=");
    }

    #[test]
    fn test_and_to_or() {
        let src = "package main\nfunc f() bool { return x && y }";
        let tree = parse_go(src);
        let root = tree.root_node();
        let bin = find_binary_expr(root).expect("should find binary_expression");
        let candidates = AndToOr.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "||");
    }

    #[test]
    fn test_or_to_and() {
        let src = "package main\nfunc f() bool { return x || y }";
        let tree = parse_go(src);
        let root = tree.root_node();
        let bin = find_binary_expr(root).expect("should find binary_expression");
        let candidates = OrToAnd.apply(&bin, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "&&");
    }

    #[test]
    fn test_no_match() {
        let src = "package main\nfunc f() bool { return x + y }";
        let tree = parse_go(src);
        let root = tree.root_node();
        let bin = find_binary_expr(root).expect("should find binary_expression");
        let candidates = LtToLte.apply(&bin, src.as_bytes());
        assert!(candidates.is_empty());
    }
}
