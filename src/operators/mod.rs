pub mod binary;
pub mod boundary;
pub mod literal;
pub mod loop_control;
pub mod removal;
pub mod unary;

/// Generates candidate source edits for one mutation operator.
///
/// Implementations inspect a tree-sitter node and return zero or more
/// [`MutationCandidate`](crate::MutationCandidate) values. Candidates are still
/// language-filtered and converted into concrete [`Mutation`](crate::Mutation)
/// values before the runner applies them.
pub trait MutationOperator: Send + Sync {
    /// Stable CLI/config identifier, such as `lt_to_lte`.
    fn id(&self) -> &str;

    /// Human-readable description used in reports.
    fn description(&self) -> &str;

    /// Return mutation candidates for `node` using byte ranges from `source`.
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<crate::MutationCandidate>;
}

pub(crate) fn mutation_candidate(
    operator: &dyn MutationOperator,
    byte_range: std::ops::Range<usize>,
    replacement: impl Into<String>,
) -> crate::MutationCandidate {
    crate::MutationCandidate {
        byte_range,
        replacement: replacement.into(),
        operator_id: operator.id().to_string(),
        description: operator.description().to_string(),
    }
}

/// Negate a condition: remove `!` if present, otherwise wrap with `!(...)`
pub struct NegateCondition;

impl MutationOperator for NegateCondition {
    fn id(&self) -> &str {
        "negate_condition"
    }
    fn description(&self) -> &str {
        "Negate condition expression"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<crate::MutationCandidate> {
        if node.kind() != lang.if_statement_node() {
            return vec![];
        }
        let cond = node.child_by_field_name("condition");
        if let Some(cond_node) = cond {
            let text = std::str::from_utf8(&source[cond_node.byte_range()]).unwrap_or("");
            let replacement = if let Some(stripped) = text.strip_prefix('!') {
                let stripped = stripped.trim();
                let fully_wrapped = stripped.starts_with('(') && {
                    let mut depth = 0;
                    let mut closes_at_end = false;
                    for (idx, ch) in stripped.char_indices() {
                        match ch {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    closes_at_end = idx + ch.len_utf8() == stripped.len();
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    closes_at_end
                };
                if fully_wrapped {
                    stripped[1..stripped.len() - 1].to_string()
                } else {
                    stripped.to_string()
                }
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
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<crate::MutationCandidate> {
        if node.kind() != lang.return_statement_node() {
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
        "remove_break" | "remove_continue" => "loop",
        "negate_condition" => "negate",
        "return_empty" => "return",
        _ => "other",
    }
}

/// All known category names.
const CATEGORIES: &[&str] = &[
    "binary", "literal", "boundary", "removal", "unary", "loop", "negate", "return",
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
        Box::new(loop_control::RemoveBreak),
        Box::new(loop_control::RemoveContinue),
        Box::new(NegateCondition),
        Box::new(ReturnEmpty),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::LanguageSupport;
    use crate::test_helpers::{
        find_node_by_kind, parse_go, parse_python, parse_ruby, parse_rust, parse_typescript,
    };

    fn apply_to_first_node(
        src: &str,
        parse: fn(&str) -> tree_sitter::Tree,
        node_kind: &str,
        op: &dyn MutationOperator,
        lang: &dyn LanguageSupport,
    ) -> Vec<crate::MutationCandidate> {
        let tree = parse(src);
        let node = find_node_by_kind(tree.root_node(), node_kind)
            .unwrap_or_else(|| panic!("should find {node_kind} node"));
        op.apply(&node, src.as_bytes(), lang)
    }

    fn collect_candidates(
        node: tree_sitter::Node,
        source: &[u8],
        op: &dyn MutationOperator,
        lang: &dyn LanguageSupport,
        out: &mut Vec<crate::MutationCandidate>,
    ) {
        out.extend(op.apply(&node, source, lang));
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_candidates(child, source, op, lang, out);
        }
    }

    fn apply_to_tree(
        src: &str,
        parse: fn(&str) -> tree_sitter::Tree,
        op: &dyn MutationOperator,
        lang: &dyn LanguageSupport,
    ) -> Vec<crate::MutationCandidate> {
        let tree = parse(src);
        let mut candidates = Vec::new();
        collect_candidates(tree.root_node(), src.as_bytes(), op, lang, &mut candidates);
        candidates
    }

    fn assert_single_candidate(
        candidates: &[crate::MutationCandidate],
        src: &str,
        replacement: &str,
        original: &str,
    ) {
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, replacement);
        assert_eq!(&src[candidates[0].byte_range.clone()], original);
    }

    #[test]
    fn test_negate_simple_condition() {
        let src = "package main\nfunc f(x int) { if x > 0 { return } }";
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = NegateCondition.apply(&if_node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "!(x > 0)", "x > 0");
    }

    #[test]
    fn test_negate_already_negated() {
        let src = "package main\nfunc f(x bool) { if !x { return } }";
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = NegateCondition.apply(&if_node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "x", "!x");
    }

    #[test]
    fn test_negate_already_negated_call() {
        let src = "package main\nfunc f() { if !foo() { return } }";
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = NegateCondition.apply(&if_node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "foo()", "!foo()");
    }

    #[test]
    fn test_negate_already_negated_partial_grouping() {
        let src = "package main\nfunc f(a, b bool) { if !(a) || (b) { return } }";
        let tree = parse_go(src);
        let if_node = find_node_by_kind(tree.root_node(), "if_statement").unwrap();
        let candidates = NegateCondition.apply(&if_node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "(a) || (b)", "!(a) || (b)");
    }

    #[test]
    fn test_negate_no_match_on_non_if() {
        let src = "package main\nfunc f() int { return 42 }";
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates =
            NegateCondition.apply(&ret_node, src.as_bytes(), &crate::languages::go::Go);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_return_empty_numeric() {
        let src = "package main\nfunc f() int { return 42 }";
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = ReturnEmpty.apply(&ret_node, src.as_bytes(), &crate::languages::go::Go);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "0");
    }

    #[test]
    fn test_return_empty_string() {
        let src = r#"package main
func f() string { return "hello" }"#;
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = ReturnEmpty.apply(&ret_node, src.as_bytes(), &crate::languages::go::Go);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, r#""""#);
    }

    #[test]
    fn test_return_empty_bool() {
        let src = "package main\nfunc f() bool { return true }";
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = ReturnEmpty.apply(&ret_node, src.as_bytes(), &crate::languages::go::Go);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "false");
    }

    #[test]
    fn test_return_empty_bare_return() {
        let src = "package main\nfunc f() { return }";
        let tree = parse_go(src);
        let ret_node = find_node_by_kind(tree.root_node(), "return_statement").unwrap();
        let candidates = ReturnEmpty.apply(&ret_node, src.as_bytes(), &crate::languages::go::Go);
        assert!(candidates.is_empty());
    }

    #[test]
    fn mul_to_div_works_across_binary_node_kinds() {
        for (src, parse, node_kind, lang) in [
            (
                "package main\nfunc f(a, b int) int { return a * b }",
                parse_go as fn(&str) -> tree_sitter::Tree,
                "binary_expression",
                Box::new(crate::languages::go::Go) as Box<dyn LanguageSupport>,
            ),
            (
                "fn f(a: i32, b: i32) -> i32 { a * b }",
                parse_rust as fn(&str) -> tree_sitter::Tree,
                "binary_expression",
                Box::new(crate::languages::rust_lang::Rust),
            ),
            (
                "def f(a, b):\n    return a * b\n",
                parse_python as fn(&str) -> tree_sitter::Tree,
                "binary_operator",
                Box::new(crate::languages::python::Python),
            ),
            (
                "def f(a, b)\n  a * b\nend\n",
                parse_ruby as fn(&str) -> tree_sitter::Tree,
                "binary",
                Box::new(crate::languages::ruby::Ruby),
            ),
            (
                "function f(a: number, b: number): number { return a * b; }",
                parse_typescript as fn(&str) -> tree_sitter::Tree,
                "binary_expression",
                Box::new(crate::languages::typescript::TypeScript),
            ),
        ] {
            let candidates =
                apply_to_first_node(src, parse, node_kind, &binary::MulToDiv, lang.as_ref());
            assert_single_candidate(&candidates, src, "/", "*");
        }
    }

    #[test]
    fn true_to_false_works_across_literal_node_kinds() {
        for (src, parse, original, lang) in [
            (
                "package main\nfunc f() bool { return true }",
                parse_go as fn(&str) -> tree_sitter::Tree,
                "true",
                Box::new(crate::languages::go::Go) as Box<dyn LanguageSupport>,
            ),
            (
                "fn f() -> bool { true }",
                parse_rust as fn(&str) -> tree_sitter::Tree,
                "true",
                Box::new(crate::languages::rust_lang::Rust),
            ),
            (
                "def f():\n    return True\n",
                parse_python as fn(&str) -> tree_sitter::Tree,
                "True",
                Box::new(crate::languages::python::Python),
            ),
            (
                "def f\n  true\nend\n",
                parse_ruby as fn(&str) -> tree_sitter::Tree,
                "true",
                Box::new(crate::languages::ruby::Ruby),
            ),
            (
                "const value = true;",
                parse_typescript as fn(&str) -> tree_sitter::Tree,
                "true",
                Box::new(crate::languages::typescript::TypeScript),
            ),
        ] {
            let candidates = apply_to_tree(src, parse, &literal::TrueToFalse, lang.as_ref());
            assert!(
                candidates.iter().any(|c| {
                    c.replacement == "false" && &src[c.byte_range.clone()] == original
                }),
                "expected true_to_false for {original:?}, got {candidates:?}"
            );
        }
    }

    #[test]
    fn return_empty_works_across_return_node_kinds() {
        for (src, parse, node_kind, lang) in [
            (
                "package main\nfunc f() int { return 42 }",
                parse_go as fn(&str) -> tree_sitter::Tree,
                "return_statement",
                Box::new(crate::languages::go::Go) as Box<dyn LanguageSupport>,
            ),
            (
                "fn f() -> i32 { return 42; }",
                parse_rust as fn(&str) -> tree_sitter::Tree,
                "return_expression",
                Box::new(crate::languages::rust_lang::Rust),
            ),
            (
                "def f():\n    return 42\n",
                parse_python as fn(&str) -> tree_sitter::Tree,
                "return_statement",
                Box::new(crate::languages::python::Python),
            ),
            (
                "def f\n  return 42\nend\n",
                parse_ruby as fn(&str) -> tree_sitter::Tree,
                "return",
                Box::new(crate::languages::ruby::Ruby),
            ),
            (
                "function f(): number { return 42; }",
                parse_typescript as fn(&str) -> tree_sitter::Tree,
                "return_statement",
                Box::new(crate::languages::typescript::TypeScript),
            ),
        ] {
            let candidates =
                apply_to_first_node(src, parse, node_kind, &ReturnEmpty, lang.as_ref());
            assert_single_candidate(&candidates, src, "0", "42");
        }
    }

    #[test]
    fn remove_if_body_works_across_if_node_kinds() {
        for (src, parse, node_kind, lang) in [
            (
                "package main\nfunc f(x bool) { if x { println(x) } }",
                parse_go as fn(&str) -> tree_sitter::Tree,
                "if_statement",
                Box::new(crate::languages::go::Go) as Box<dyn LanguageSupport>,
            ),
            (
                "fn f(x: bool) { if x { call(); } }",
                parse_rust as fn(&str) -> tree_sitter::Tree,
                "if_expression",
                Box::new(crate::languages::rust_lang::Rust),
            ),
            (
                "def f(x):\n    if x:\n        call()\n",
                parse_python as fn(&str) -> tree_sitter::Tree,
                "if_statement",
                Box::new(crate::languages::python::Python),
            ),
            (
                "def f(x)\n  if x\n    call\n  end\nend\n",
                parse_ruby as fn(&str) -> tree_sitter::Tree,
                "if",
                Box::new(crate::languages::ruby::Ruby),
            ),
            (
                "function f(x: boolean) { if (x) { call(); } }",
                parse_typescript as fn(&str) -> tree_sitter::Tree,
                "if_statement",
                Box::new(crate::languages::typescript::TypeScript),
            ),
        ] {
            let candidates =
                apply_to_first_node(src, parse, node_kind, &removal::RemoveIfBody, lang.as_ref());
            assert_eq!(candidates.len(), 1, "{src}");
            assert_eq!(candidates[0].replacement, "{}");
        }
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
        fn apply(
            &self,
            _: &tree_sitter::Node,
            _: &[u8],
            _: &dyn crate::languages::LanguageSupport,
        ) -> Vec<crate::MutationCandidate> {
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

    #[test]
    fn all_operator_ids_are_unique() {
        let ops = all_operators();
        let unique: std::collections::HashSet<&str> = ops.iter().map(|o| o.id()).collect();
        assert_eq!(
            unique.len(),
            ops.len(),
            "duplicate operator id in all_operators()"
        );
    }

    #[test]
    fn every_operator_has_known_category() {
        for op in all_operators() {
            let cat = operator_category(op.id());
            assert_ne!(
                cat,
                "other",
                "operator '{}' is missing a category in operator_category()",
                op.id()
            );
            assert!(
                CATEGORIES.contains(&cat),
                "operator '{}' has unknown category '{}'",
                op.id(),
                cat
            );
        }
    }
}
