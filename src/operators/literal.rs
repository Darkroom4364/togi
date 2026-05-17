use super::{MutationOperator, mutation_candidate};
use crate::MutationCandidate;

pub struct TrueToFalse;

impl MutationOperator for TrueToFalse {
    fn id(&self) -> &str {
        "true_to_false"
    }
    fn description(&self) -> &str {
        "Replace true with false"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<MutationCandidate> {
        if lang.is_boolean_true_literal_node(node.kind()) {
            let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
            if lang.boolean_true_literals().contains(&text) {
                return vec![mutation_candidate(self, node.byte_range(), "false")];
            }
        }
        vec![]
    }
}

pub struct FalseToTrue;

impl MutationOperator for FalseToTrue {
    fn id(&self) -> &str {
        "false_to_true"
    }
    fn description(&self) -> &str {
        "Replace false with true"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<MutationCandidate> {
        if lang.is_boolean_false_literal_node(node.kind()) {
            let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
            if lang.boolean_false_literals().contains(&text) {
                return vec![mutation_candidate(self, node.byte_range(), "true")];
            }
        }
        vec![]
    }
}

pub struct ZeroToOne;

impl MutationOperator for ZeroToOne {
    fn id(&self) -> &str {
        "zero_to_one"
    }
    fn description(&self) -> &str {
        "Replace 0 with 1"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<MutationCandidate> {
        if lang.is_integer_literal_node(node.kind()) {
            let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
            if text == "0" {
                return vec![mutation_candidate(self, node.byte_range(), "1")];
            }
        }
        vec![]
    }
}

pub struct StringToEmpty;

impl MutationOperator for StringToEmpty {
    fn id(&self) -> &str {
        "string_to_empty"
    }
    fn description(&self) -> &str {
        "Replace string literal with empty string"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<MutationCandidate> {
        if lang.is_string_literal_node(node.kind()) {
            let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
            if text == "\"\"" || text == "''" || text == "``" {
                return vec![];
            }
            let replacement = if text.starts_with('`') {
                "``".to_string()
            } else if text.starts_with('\'') {
                "''".to_string()
            } else {
                "\"\"".to_string()
            };
            return vec![mutation_candidate(self, node.byte_range(), replacement)];
        }
        vec![]
    }
}

pub struct IncrementNumeric;

fn integer_literal_value_and_range(
    node: &tree_sitter::Node,
    source: &[u8],
    lang: &dyn crate::languages::LanguageSupport,
) -> Option<(i64, std::ops::Range<usize>)> {
    if !lang.is_integer_literal_node(node.kind()) {
        return None;
    }

    if let Some(range) = unary_minus_literal_range(node, source, lang) {
        return parse_integer_range(source, range.clone()).map(|n| (n, range));
    }

    let range = node.byte_range();
    parse_integer_range(source, range.clone()).map(|n| (n, range))
}

fn unary_minus_literal_range(
    node: &tree_sitter::Node,
    source: &[u8],
    lang: &dyn crate::languages::LanguageSupport,
) -> Option<std::ops::Range<usize>> {
    let parent = node.parent()?;
    let parent_kind = parent.kind();
    if !lang.is_unary_expression_node(parent_kind)
        && !matches!(parent_kind, "negative_literal" | "unary_operator")
    {
        return None;
    }

    let parent_range = parent.byte_range();
    let node_range = node.byte_range();
    if parent_range.end != node_range.end || parent_range.start >= node_range.start {
        return None;
    }

    let prefix = std::str::from_utf8(source.get(parent_range.start..node_range.start)?).ok()?;
    if prefix.trim() == "-" {
        Some(parent_range)
    } else {
        None
    }
}

fn parse_integer_range(source: &[u8], range: std::ops::Range<usize>) -> Option<i64> {
    let text = std::str::from_utf8(source.get(range)?).ok()?;
    text.parse::<i64>().ok().or_else(|| {
        if !text.chars().any(char::is_whitespace) {
            return None;
        }
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        compact.parse::<i64>().ok()
    })
}

impl MutationOperator for IncrementNumeric {
    fn id(&self) -> &str {
        "increment_numeric"
    }
    fn description(&self) -> &str {
        "Replace n with n+1"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<MutationCandidate> {
        if let Some((n, range)) = integer_literal_value_and_range(node, source, lang) {
            n.checked_add(1)
                .map(|n| mutation_candidate(self, range, n.to_string()))
                .into_iter()
                .collect()
        } else {
            vec![]
        }
    }
}

pub struct DecrementNumeric;

impl MutationOperator for DecrementNumeric {
    fn id(&self) -> &str {
        "decrement_numeric"
    }
    fn description(&self) -> &str {
        "Replace n with n-1"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
        lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<MutationCandidate> {
        if let Some((n, range)) = integer_literal_value_and_range(node, source, lang) {
            n.checked_sub(1)
                .map(|n| mutation_candidate(self, range, n.to_string()))
                .into_iter()
                .collect()
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_helpers::{find_node_by_kind, parse_go, parse_python, parse_rust};

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

    fn collect_all_candidates(
        node: tree_sitter::Node,
        source: &[u8],
        op: &dyn MutationOperator,
        out: &mut Vec<MutationCandidate>,
    ) {
        out.extend(op.apply(&node, source, &crate::languages::go::Go));
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_all_candidates(child, source, op, out);
        }
    }

    #[test]
    fn test_true_to_false() {
        let src = "package main\nfunc f() bool { return true }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "true").expect("should find true node");
        let candidates = TrueToFalse.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "false", "true");
    }

    #[test]
    fn test_false_to_true() {
        let src = "package main\nfunc f() bool { return false }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "false").expect("should find false node");
        let candidates = FalseToTrue.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "true", "false");
    }

    #[test]
    fn test_zero_to_one() {
        let src = "package main\nfunc f() int { return 0 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");
        let candidates = ZeroToOne.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "1", "0");
    }

    #[test]
    fn test_zero_to_one_skips_nonzero() {
        let src = "package main\nfunc f() int { return 42 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");
        let candidates = ZeroToOne.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_true_to_false_no_match_on_identifier() {
        let src = "package main\nvar trueValue = 1";
        let tree = parse_go(src);
        let mut candidates = vec![];
        collect_all_candidates(
            tree.root_node(),
            src.as_bytes(),
            &TrueToFalse,
            &mut candidates,
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_string_to_empty_double_quoted() {
        let src = r#"package main
func f() string { return "hello" }"#;
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "interpreted_string_literal")
            .expect("should find interpreted_string_literal node");
        let candidates = StringToEmpty.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, r#""""#, r#""hello""#);
    }

    #[test]
    fn test_string_to_empty_raw_string() {
        let src = "package main\nfunc f() string { return `hello` }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "raw_string_literal")
            .expect("should find raw_string_literal node");
        let candidates = StringToEmpty.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "``", "`hello`");
    }

    #[test]
    fn test_string_to_empty_skips_empty_string() {
        let src = r#"package main
func f() string { return "" }"#;
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "interpreted_string_literal")
            .expect("should find interpreted_string_literal node");
        let candidates = StringToEmpty.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_increment_numeric() {
        let src = "package main\nfunc f() int { return 41 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");
        let candidates = IncrementNumeric.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "42", "41");
    }

    #[test]
    fn increment_numeric_skips_i64_max() {
        let src = "package main\nfunc f() int { return 9223372036854775807 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");
        let candidates = IncrementNumeric.apply(&node, src.as_bytes(), &crate::languages::go::Go);

        assert!(candidates.is_empty());
    }

    #[test]
    fn test_decrement_numeric() {
        let src = "package main\nfunc f() int { return 41 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");
        let candidates = DecrementNumeric.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "40", "41");
    }

    #[test]
    fn decrement_numeric_skips_i64_min() {
        let src = "package main\nfunc f() int { return -9223372036854775808 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");
        let candidates = DecrementNumeric.apply(&node, src.as_bytes(), &crate::languages::go::Go);

        assert!(candidates.is_empty());
    }

    #[test]
    fn numeric_mutators_skip_float_literals() {
        let src = "package main\nfunc f() float64 { return 4.5 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "float_literal")
            .expect("should find float_literal node");

        assert!(
            IncrementNumeric
                .apply(&node, src.as_bytes(), &crate::languages::go::Go)
                .is_empty()
        );
        assert!(
            DecrementNumeric
                .apply(&node, src.as_bytes(), &crate::languages::go::Go)
                .is_empty()
        );
    }

    #[test]
    fn go_negative_numeric_mutators_replace_signed_literal() {
        let src = "package main\nfunc f() int { return -1 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");

        let candidates = IncrementNumeric.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "0", "-1");

        let candidates = DecrementNumeric.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_single_candidate(&candidates, src, "-2", "-1");
    }

    #[test]
    fn rust_negative_numeric_mutators_replace_signed_literal() {
        let src = "fn f() -> i32 { -1 }";
        let tree = parse_rust(src);
        let node = find_node_by_kind(tree.root_node(), "integer_literal")
            .expect("should find integer_literal node");

        let candidates =
            IncrementNumeric.apply(&node, src.as_bytes(), &crate::languages::rust_lang::Rust);
        assert_single_candidate(&candidates, src, "0", "-1");

        let candidates =
            DecrementNumeric.apply(&node, src.as_bytes(), &crate::languages::rust_lang::Rust);
        assert_single_candidate(&candidates, src, "-2", "-1");
    }

    #[test]
    fn python_negative_numeric_mutators_replace_signed_literal() {
        let src = "def f():\n    return -1\n";
        let tree = parse_python(src);
        let node =
            find_node_by_kind(tree.root_node(), "integer").expect("should find integer node");

        let candidates =
            IncrementNumeric.apply(&node, src.as_bytes(), &crate::languages::python::Python);
        assert_single_candidate(&candidates, src, "0", "-1");

        let candidates =
            DecrementNumeric.apply(&node, src.as_bytes(), &crate::languages::python::Python);
        assert_single_candidate(&candidates, src, "-2", "-1");
    }
}
