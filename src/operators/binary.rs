use super::MutationOperator;
use crate::MutationCandidate;

pub fn find_operator_child(
    node: &tree_sitter::Node,
    source: &[u8],
    operator_field: &str,
    target: &str,
) -> Option<std::ops::Range<usize>> {
    if let Some(op_node) = node.child_by_field_name(operator_field) {
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

pub fn find_operator_children(
    node: &tree_sitter::Node,
    source: &[u8],
    operator_field: &str,
    target: &str,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let field_range = node
        .child_by_field_name(operator_field)
        .filter(|op_node| &source[op_node.byte_range()] == target.as_bytes())
        .map(|op_node| op_node.byte_range());
    if let Some(range) = field_range.clone() {
        ranges.push(range);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let range = child.byte_range();
        if !child.is_named()
            && field_range.as_ref() != Some(&range)
            && &source[range.clone()] == target.as_bytes()
        {
            ranges.push(range);
        }
    }
    ranges
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
            fn apply(
                &self,
                node: &tree_sitter::Node,
                source: &[u8],
                lang: &dyn crate::languages::LanguageSupport,
            ) -> Vec<MutationCandidate> {
                if !lang.is_binary_operator_node(node.kind()) {
                    return vec![];
                }
                let (from, to) = lang
                    .binary_operator_tokens(self.id())
                    .unwrap_or(($from, $to));
                find_operator_children(node, source, lang.operator_field(), from)
                    .into_iter()
                    .map(|range| MutationCandidate {
                        byte_range: range,
                        replacement: to.to_string(),
                        operator_id: self.id().to_string(),
                        description: format!("Replace {from} with {to}"),
                    })
                    .collect()
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

    use crate::languages::python::Python;
    use crate::test_helpers::{find_node_by_kind, parse_go, parse_python};

    fn apply_to_binary(src: &str, op: &dyn MutationOperator) -> Vec<MutationCandidate> {
        let tree = parse_go(src);
        let root = tree.root_node();
        let bin =
            find_node_by_kind(root, "binary_expression").expect("should find binary_expression");
        op.apply(&bin, src.as_bytes(), &crate::languages::go::Go)
    }

    fn apply_to_python_node(
        src: &str,
        node_kind: &str,
        op: &dyn MutationOperator,
    ) -> Vec<MutationCandidate> {
        let tree = parse_python(src);
        let node =
            find_node_by_kind(tree.root_node(), node_kind).expect("should find Python operator");
        op.apply(&node, src.as_bytes(), &Python)
    }

    fn apply_candidate(src: &str, candidate: &MutationCandidate) -> String {
        let mut mutated = src.as_bytes().to_vec();
        mutated.splice(
            candidate.byte_range.clone(),
            candidate.replacement.as_bytes().iter().copied(),
        );
        String::from_utf8(mutated).unwrap()
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

    #[test]
    fn python_comparison_chain_yields_each_operator_range_and_parses() {
        let src = "def check(low, value, high):\n    return low < value < high\n";
        let candidates = apply_to_python_node(src, "comparison_operator", &LtToLte);
        let expected_ranges = src
            .match_indices('<')
            .map(|(start, _)| start..start + 1)
            .collect::<Vec<_>>();

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.byte_range.clone())
                .collect::<Vec<_>>(),
            expected_ranges
        );
        for candidate in candidates {
            assert_eq!(candidate.replacement, "<=");
            let mutated = apply_candidate(src, &candidate);
            assert!(
                !parse_python(&mutated).root_node().has_error(),
                "candidate should parse as Python:\n{mutated}"
            );
        }
    }

    #[test]
    fn python_logical_operators_use_python_tokens_and_parse() {
        for (src, op, replacement, original) in [
            (
                "def check(left, right):\n    return left and right\n",
                &AndToOr as &dyn MutationOperator,
                "or",
                "and",
            ),
            (
                "def check(left, right):\n    return left or right\n",
                &OrToAnd as &dyn MutationOperator,
                "and",
                "or",
            ),
        ] {
            let candidates = apply_to_python_node(src, "boolean_operator", op);
            assert_single_candidate(&candidates, src, replacement, original);
            assert_eq!(
                candidates[0].description,
                format!("Replace {original} with {replacement}")
            );
            let mutated = apply_candidate(src, &candidates[0]);
            assert!(
                !parse_python(&mutated).root_node().has_error(),
                "candidate should parse as Python:\n{mutated}"
            );
        }
    }

    #[test]
    fn python_arithmetic_binary_operator_stays_supported() {
        let src = "def multiply(left, right):\n    return left * right\n";
        let candidates = apply_to_python_node(src, "binary_operator", &MulToDiv);

        assert_single_candidate(&candidates, src, "/", "*");
    }
}
