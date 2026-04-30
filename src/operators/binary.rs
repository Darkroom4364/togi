use super::{MutationOperator, is_binary_expr};
use crate::MutationCandidate;

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

    use crate::test_helpers::{find_node_by_kind, parse_go};

    fn apply_to_binary(src: &str, op: &dyn MutationOperator) -> Vec<MutationCandidate> {
        let tree = parse_go(src);
        let root = tree.root_node();
        let bin =
            find_node_by_kind(root, "binary_expression").expect("should find binary_expression");
        op.apply(&bin, src.as_bytes())
    }

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
    fn test_lt_to_lte() {
        let src = "package main\nfunc f() bool { return x < y }";
        let candidates = apply_to_binary(src, &LtToLte);
        assert_single_candidate(&candidates, src, "<=", "<");
    }

    #[test]
    fn test_gt_to_gte() {
        let src = "package main\nfunc f() bool { return x > y }";
        let candidates = apply_to_binary(src, &GtToGte);
        assert_single_candidate(&candidates, src, ">=", ">");
    }

    #[test]
    fn test_eq_to_neq() {
        let src = "package main\nfunc f() bool { return x == y }";
        let candidates = apply_to_binary(src, &EqToNeq);
        assert_single_candidate(&candidates, src, "!=", "==");
    }

    #[test]
    fn test_and_to_or() {
        let src = "package main\nfunc f() bool { return x && y }";
        let candidates = apply_to_binary(src, &AndToOr);
        assert_single_candidate(&candidates, src, "||", "&&");
    }

    #[test]
    fn test_or_to_and() {
        let src = "package main\nfunc f() bool { return x || y }";
        let candidates = apply_to_binary(src, &OrToAnd);
        assert_single_candidate(&candidates, src, "&&", "||");
    }

    #[test]
    fn test_mul_to_div() {
        let src = "package main\nfunc f() int { return x * y }";
        let candidates = apply_to_binary(src, &MulToDiv);
        assert_single_candidate(&candidates, src, "/", "*");
    }

    #[test]
    fn test_div_to_mul() {
        let src = "package main\nfunc f() int { return x / y }";
        let candidates = apply_to_binary(src, &DivToMul);
        assert_single_candidate(&candidates, src, "*", "/");
    }

    #[test]
    fn test_mod_to_mul() {
        let src = "package main\nfunc f() int { return x % y }";
        let candidates = apply_to_binary(src, &ModToMul);
        assert_single_candidate(&candidates, src, "*", "%");
    }

    #[test]
    fn test_no_match() {
        let src = "package main\nfunc f() bool { return x + y }";
        let candidates = apply_to_binary(src, &LtToLte);
        assert!(candidates.is_empty());
    }
}
