pub mod binary;
pub mod boundary;
pub mod literal;
pub mod removal;
pub mod unary;

/// A mutation operator that generates candidate mutations from AST nodes
pub trait MutationOperator: Send + Sync {
    fn id(&self) -> &str;
    fn description(&self) -> &str;
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<crate::MutationCandidate>;
}

pub(crate) const IF_STMT_KINDS: &[&str] = &[
    "if_statement",
    "if_expression",
    "if_expr",
    "if", // Ruby
];
const RETURN_KINDS: &[&str] = &[
    "return_statement",
    "return_expression",
    "return", // Ruby
];

/// Negate a condition: remove `!` if present, otherwise wrap with `!(...)`
pub struct NegateCondition;

impl MutationOperator for NegateCondition {
    fn id(&self) -> &str {
        "negate_condition"
    }
    fn description(&self) -> &str {
        "Negate condition expression"
    }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<crate::MutationCandidate> {
        if !IF_STMT_KINDS.contains(&node.kind()) {
            return vec![];
        }
        let cond = node.child_by_field_name("condition");
        if let Some(cond_node) = cond {
            let text = std::str::from_utf8(&source[cond_node.byte_range()]).unwrap_or("");
            let replacement = if let Some(stripped) = text.strip_prefix('!') {
                stripped
                    .trim_start_matches('(')
                    .trim_end_matches(')')
                    .to_string()
            } else {
                format!("!({})", text)
            };
            vec![crate::MutationCandidate {
                byte_range: cond_node.byte_range(),
                replacement,
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
        } else {
            vec![]
        }
    }
}

/// Replace a return statement's value with a default
pub struct ReturnEmpty;

impl MutationOperator for ReturnEmpty {
    fn id(&self) -> &str {
        "return_empty"
    }
    fn description(&self) -> &str {
        "Replace return value with default"
    }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<crate::MutationCandidate> {
        if !RETURN_KINDS.contains(&node.kind()) {
            return vec![];
        }
        // Find the expression list or value child
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        if children.is_empty() {
            return vec![];
        }
        // The return value spans from first named child to end of last named child
        let first = &children[0];
        let last = &children[children.len() - 1];
        let value_range = first.start_byte()..last.end_byte();
        let text = std::str::from_utf8(&source[value_range.clone()]).unwrap_or("");

        // Use tree-sitter node kind of the first child for more accurate replacement
        let first_kind = first.kind();
        let replacement = match first_kind {
            // String literals
            "interpreted_string_literal"
            | "raw_string_literal"
            | "string"
            | "string_literal"
            | "template_string" => "\"\"".to_string(),
            // Boolean literals
            "true" | "false" | "boolean" => "false".to_string(),
            // Null/nil/None
            "null" | "nil" | "none" | "None" => return vec![], // already a zero-value, skip
            // Numeric literals
            "integer_literal" | "int_literal" | "float_literal" | "number" | "integer"
            | "float" => "0".to_string(),
            // Fallback: use text-based heuristic
            _ => {
                if text == "nil" || text == "null" || text == "None" {
                    return vec![]; // Already a zero-value, mutation not useful
                } else if text == "true" || text == "false" {
                    "false".to_string()
                } else if text.starts_with('"') || text.starts_with('\'') || text.starts_with('`') {
                    "\"\"".to_string()
                } else {
                    "0".to_string()
                }
            }
        };

        vec![crate::MutationCandidate {
            byte_range: value_range,
            replacement,
            operator_id: self.id().to_string(),
            description: self.description().to_string(),
        }]
    }
}

/// Returns all mutation operators
/// Return the category for an operator ID.
pub fn operator_category(id: &str) -> &str {
    match id {
        "lt_to_lte" | "gt_to_gte" | "eq_to_neq" | "and_to_or" | "or_to_and" | "mul_to_div"
        | "div_to_mul" | "mod_to_mul" => "binary",
        "true_to_false" | "false_to_true" | "zero_to_one" | "string_to_empty"
        | "increment_numeric" | "decrement_numeric" => "literal",
        "plus_to_minus" | "minus_to_plus" => "boundary",
        "remove_if_body" | "remove_else" | "remove_call_statement" | "remove_assignment" => {
            "removal"
        }
        "remove_unary_not" | "remove_unary_neg" => "unary",
        "negate_condition" => "negate",
        "return_empty" => "return",
        _ => "other",
    }
}

/// All known category names.
const CATEGORIES: &[&str] = &[
    "binary", "literal", "boundary", "removal", "unary", "negate", "return",
];

/// Validate that every pattern in `patterns` matches a known operator ID or category.
/// Returns the first unknown pattern as an error with did-you-mean suggestions.
pub fn validate_patterns(
    operators: &[Box<dyn MutationOperator>],
    patterns: &[String],
) -> Result<(), String> {
    let known_ids: Vec<&str> = operators.iter().map(|o| o.id()).collect();
    for raw in patterns {
        let trimmed = raw.trim().trim_start_matches('-');
        if trimmed.is_empty() {
            continue;
        }
        if known_ids.contains(&trimmed) || CATEGORIES.contains(&trimmed) {
            continue;
        }
        let mut candidates: Vec<(&str, usize)> = known_ids
            .iter()
            .chain(CATEGORIES.iter())
            .map(|k| (*k, edit_distance(trimmed, k)))
            .filter(|(_, d)| *d <= 3)
            .collect();
        candidates.sort_by_key(|(_, d)| *d);
        let suggestion = candidates
            .first()
            .map(|(k, _)| format!(" Did you mean '{k}'?"))
            .unwrap_or_default();
        return Err(format!(
            "unknown operator or category '{trimmed}'.{suggestion}"
        ));
    }
    Ok(())
}

/// Simple Levenshtein distance for did-you-mean suggestions.
fn edit_distance(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Filter operators based on include/exclude patterns.
/// Patterns can be operator IDs or category names.
/// Prefix with `-` to exclude. If any non-exclude pattern exists,
/// only matching operators are included.
pub fn filter_operators(
    operators: Vec<Box<dyn MutationOperator>>,
    patterns: &[String],
) -> Vec<Box<dyn MutationOperator>> {
    if patterns.is_empty() {
        return operators;
    }

    let excludes: Vec<&str> = patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .filter(|p| p.starts_with('-'))
        .map(|p| p.trim_start_matches('-'))
        .collect();
    let includes: Vec<&str> = patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .filter(|p| !p.starts_with('-'))
        .collect();

    operators
        .into_iter()
        .filter(|op| {
            let id = op.id();
            let cat = operator_category(id);

            // Check excludes first
            if excludes.contains(&id) || excludes.contains(&cat) {
                return false;
            }
            // If includes specified, must match
            if !includes.is_empty() {
                return includes.contains(&id) || includes.contains(&cat);
            }
            true
        })
        .collect()
}

pub fn all_operators() -> Vec<Box<dyn MutationOperator>> {
    vec![
        Box::new(binary::LtToLte),
        Box::new(binary::GtToGte),
        Box::new(binary::EqToNeq),
        Box::new(binary::AndToOr),
        Box::new(binary::OrToAnd),
        Box::new(binary::MulToDiv),
        Box::new(binary::DivToMul),
        Box::new(binary::ModToMul),
        Box::new(literal::TrueToFalse),
        Box::new(literal::FalseToTrue),
        Box::new(literal::ZeroToOne),
        Box::new(literal::StringToEmpty),
        Box::new(literal::IncrementNumeric),
        Box::new(literal::DecrementNumeric),
        Box::new(boundary::PlusToMinus),
        Box::new(boundary::MinusToPlus),
        Box::new(removal::RemoveIfBody),
        Box::new(removal::RemoveElse),
        Box::new(removal::RemoveCallStatement),
        Box::new(removal::RemoveAssignment),
        Box::new(unary::RemoveUnaryNot),
        Box::new(unary::RemoveUnaryNeg),
        Box::new(NegateCondition),
        Box::new(ReturnEmpty),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{find_node_by_kind, parse_go};

    #[test]
    fn test_negate_simple_condition() {
        let src = "package main\nfunc f(x int) { if x > 0 { return } }";
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = NegateCondition.apply(&if_node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "!(x > 0)");
    }

    #[test]
    fn test_negate_already_negated() {
        let src = "package main\nfunc f(x bool) { if !x { return } }";
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = NegateCondition.apply(&if_node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "x");
    }

    #[test]
    fn test_negate_no_match_on_non_if() {
        let src = "package main\nfunc f() int { return 42 }";
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = NegateCondition.apply(&ret_node, src.as_bytes());
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_return_empty_numeric() {
        let src = "package main\nfunc f() int { return 42 }";
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = ReturnEmpty.apply(&ret_node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "0");
    }

    #[test]
    fn test_return_empty_string() {
        let src = r#"package main
func f() string { return "hello" }"#;
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = ReturnEmpty.apply(&ret_node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, r#""""#);
    }

    #[test]
    fn test_return_empty_bool() {
        let src = "package main\nfunc f() bool { return true }";
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = ReturnEmpty.apply(&ret_node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "false");
    }

    #[test]
    fn test_return_empty_bare_return() {
        let src = "package main\nfunc f() { return }";
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = ReturnEmpty.apply(&ret_node, src.as_bytes());
        assert!(candidates.is_empty());
    }

    // Stub operator for filter tests
    struct StubOp(&'static str);
    impl MutationOperator for StubOp {
        fn id(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            ""
        }
        fn apply(&self, _: &tree_sitter::Node, _: &[u8]) -> Vec<crate::MutationCandidate> {
            vec![]
        }
    }

    fn stub_ops() -> Vec<Box<dyn MutationOperator>> {
        vec![
            Box::new(StubOp("lt_to_lte")),        // binary
            Box::new(StubOp("string_to_empty")),  // literal
            Box::new(StubOp("plus_to_minus")),    // boundary
            Box::new(StubOp("remove_if_body")),   // removal
            Box::new(StubOp("remove_unary_not")), // unary
            Box::new(StubOp("negate_condition")), // negate
            Box::new(StubOp("return_empty")),     // return
        ]
    }

    fn ids(ops: &[Box<dyn MutationOperator>]) -> Vec<&str> {
        ops.iter().map(|o| o.id()).collect()
    }

    #[test]
    fn filter_include_only() {
        let ops = filter_operators(stub_ops(), &["binary".into(), "removal".into()]);
        assert_eq!(ids(&ops), vec!["lt_to_lte", "remove_if_body"]);
    }

    #[test]
    fn filter_exclude_only() {
        let ops = filter_operators(stub_ops(), &["-literal".into(), "-boundary".into()]);
        let result = ids(&ops);
        assert!(!result.contains(&"string_to_empty"));
        assert!(!result.contains(&"plus_to_minus"));
        assert!(result.contains(&"lt_to_lte"));
        assert!(result.contains(&"return_empty"));
    }

    #[test]
    fn filter_mixed_exclude_wins() {
        // Include binary category but exclude lt_to_lte specifically
        let ops = filter_operators(stub_ops(), &["binary".into(), "-lt_to_lte".into()]);
        assert!(ids(&ops).is_empty());
    }

    #[test]
    fn filter_category_matches() {
        let ops = filter_operators(stub_ops(), &["literal".into()]);
        assert_eq!(ids(&ops), vec!["string_to_empty"]);
    }

    #[test]
    fn filter_whitespace_trimmed() {
        let ops = filter_operators(stub_ops(), &[" -literal ".into()]);
        assert!(!ids(&ops).contains(&"string_to_empty"));
        assert!(ids(&ops).contains(&"lt_to_lte"));
    }

    #[test]
    fn validate_accepts_known_ids_and_categories() {
        assert!(validate_patterns(&stub_ops(), &["lt_to_lte".into(), "binary".into()]).is_ok());
        assert!(validate_patterns(&stub_ops(), &["-removal".into()]).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_pattern() {
        let err = validate_patterns(&stub_ops(), &["invalid_name".into()]).unwrap_err();
        assert!(err.contains("unknown operator or category 'invalid_name'"));
    }

    #[test]
    fn validate_suggests_similar_name() {
        let err = validate_patterns(&stub_ops(), &["litral".into()]).unwrap_err();
        assert!(err.contains("Did you mean 'literal'?"), "{err}");
    }
}
