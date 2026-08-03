//! Conservative, advisory hints for surviving mutants that are likely equivalent.
//!
//! These heuristics deliberately use only local syntax. They never alter a mutation verdict,
//! score, or gate: a false negative is preferable to hiding a real test gap.

use crate::languages::LanguageSupport;
use crate::{Mutation, MutationReport, MutationResult};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Tree};

/// Why a surviving mutant is likely equivalent under a narrow syntax-only rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LikelyEquivalentReason {
    SameBooleanLiterals,
    RedundantBoundary,
}

impl LikelyEquivalentReason {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::SameBooleanLiterals => {
                "both operands are the same boolean literal, so either logical operator produces the same value"
            }
            Self::RedundantBoundary => {
                "a stricter `&&` sibling check already excludes the changed boundary"
            }
        }
    }
}

struct ParsedSource {
    source: String,
    tree: Tree,
    language: Box<dyn LanguageSupport>,
}

impl ParsedSource {
    fn load(path: &Path) -> Option<Self> {
        let source = fs::read_to_string(path).ok()?;
        let (tree, language) = crate::parser::parse_file(path, source.as_bytes()).ok()?;
        Some(Self {
            source,
            tree,
            language,
        })
    }
}

/// Return advisory reasons for surviving mutations, keyed by mutation id.
///
/// Source files are read and parsed once per file. If the current source cannot be
/// read or parsed, the mutation stays a plain survivor rather than receiving a hint.
pub(crate) fn advisories_for(report: &MutationReport) -> BTreeMap<u32, LikelyEquivalentReason> {
    let mut sources = BTreeMap::<PathBuf, Option<ParsedSource>>::new();
    let mut advisories = BTreeMap::new();

    for (mutation, result) in &report.results {
        if *result != MutationResult::Survived {
            continue;
        }
        let parsed = sources
            .entry(mutation.file.clone())
            .or_insert_with(|| ParsedSource::load(&mutation.file));
        let Some(parsed) = parsed.as_ref() else {
            continue;
        };
        if let Some(reason) = likely_equivalent_reason(mutation, parsed) {
            advisories.insert(mutation.id, reason);
        }
    }

    advisories
}

fn likely_equivalent_reason(
    mutation: &Mutation,
    parsed: &ParsedSource,
) -> Option<LikelyEquivalentReason> {
    // The first rules rely on Rust's exact AST shapes. Do not infer intent for
    // other languages until they have equally narrow, tested rules.
    if mutation.language != "rust" || parsed.language.name() != "rust" {
        return None;
    }
    if parsed.source.as_bytes().get(mutation.byte_range.clone())
        != Some(mutation.original.as_bytes())
    {
        return None;
    }

    if has_identical_boolean_literals(mutation, parsed) {
        return Some(LikelyEquivalentReason::SameBooleanLiterals);
    }
    if has_redundant_boundary(mutation, parsed) {
        return Some(LikelyEquivalentReason::RedundantBoundary);
    }
    None
}

fn has_identical_boolean_literals(mutation: &Mutation, parsed: &ParsedSource) -> bool {
    let expected_operator = match mutation.operator.as_str() {
        "and_to_or" => "&&",
        "or_to_and" => "||",
        _ => return false,
    };
    if mutation.original != expected_operator {
        return false;
    }

    let Some(binary) = binary_mutation_node(mutation, parsed) else {
        return false;
    };
    let source = parsed.source.as_bytes();
    let Some((left, _, right)) = binary_parts(binary, source) else {
        return false;
    };
    let (Some(left), Some(right)) = (
        source
            .get(left.byte_range())
            .and_then(|value| std::str::from_utf8(value).ok())
            .map(str::trim),
        source
            .get(right.byte_range())
            .and_then(|value| std::str::from_utf8(value).ok())
            .map(str::trim),
    ) else {
        return false;
    };

    left == right && matches!(left, "true" | "false")
}

fn has_redundant_boundary(mutation: &Mutation, parsed: &ParsedSource) -> bool {
    let expected_operator = match mutation.operator.as_str() {
        "lt_to_lte" => "<",
        "gt_to_gte" => ">",
        _ => return false,
    };
    if mutation.original != expected_operator {
        return false;
    }

    let Some(comparison) = binary_mutation_node(mutation, parsed) else {
        return false;
    };
    let Some(mut logical_and) = comparison.parent() else {
        return false;
    };
    while logical_and.kind() == "parenthesized_expression" {
        let Some(parent) = logical_and.parent() else {
            return false;
        };
        logical_and = parent;
    }
    if logical_and.kind() != parsed.language.binary_expression_node() {
        return false;
    }

    let source = parsed.source.as_bytes();
    let Some((left, and_range, right)) = binary_parts(logical_and, source) else {
        return false;
    };
    if source.get(and_range.clone()) != Some(&b"&&"[..]) {
        return false;
    }
    let comparison_range = comparison.byte_range();
    let sibling_range = if comparison_range == left.byte_range() {
        right.byte_range()
    } else if comparison_range == right.byte_range() {
        left.byte_range()
    } else {
        return false;
    };

    let Some(mutated) = bound_comparison(
        source.get(comparison_range.start..mutation.byte_range.start),
        expected_operator,
        source.get(mutation.byte_range.end..comparison_range.end),
    ) else {
        return false;
    };
    let Some(sibling) = bound_from_source(source.get(sibling_range)) else {
        return false;
    };

    mutated.subject == sibling.subject
        && is_direct_primitive_parameter_bound(logical_and, mutated.subject, parsed)
        && sibling_excludes_changed_boundary(mutated, sibling)
}

fn is_direct_primitive_parameter_bound(
    logical_and: Node<'_>,
    subject: &str,
    parsed: &ParsedSource,
) -> bool {
    let Some(block) = logical_and.parent() else {
        return false;
    };
    if block.kind() != "block"
        || block.named_child_count() != 1
        || block
            .named_child(0)
            .is_none_or(|child| child.byte_range() != logical_and.byte_range())
    {
        return false;
    }
    let Some(function) = block.parent() else {
        return false;
    };
    if function.kind() != "function_item" {
        return false;
    }
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return false;
    };

    let source = parsed.source.as_bytes();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if parameter.kind() != "parameter" {
            continue;
        }
        let (Some(pattern), Some(type_name)) = (
            parameter.child_by_field_name("pattern"),
            parameter.child_by_field_name("type"),
        ) else {
            continue;
        };
        if source.get(pattern.byte_range()) == Some(subject.as_bytes())
            && source
                .get(type_name.byte_range())
                .and_then(|value| std::str::from_utf8(value).ok())
                .is_some_and(is_primitive_integer)
        {
            return true;
        }
    }
    false
}

fn is_primitive_integer(type_name: &str) -> bool {
    matches!(
        type_name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
    )
}

fn binary_mutation_node<'tree>(
    mutation: &Mutation,
    parsed: &'tree ParsedSource,
) -> Option<Node<'tree>> {
    let source = parsed.source.as_bytes();
    let mut node = parsed
        .tree
        .root_node()
        .descendant_for_byte_range(mutation.byte_range.start, mutation.byte_range.end)?;
    loop {
        if node.kind() == parsed.language.binary_expression_node()
            && binary_parts(node, source).is_some_and(|(_, operator_range, _)| {
                operator_range == mutation.byte_range
                    && source.get(operator_range) == Some(mutation.original.as_bytes())
            })
        {
            return Some(node);
        }
        node = node.parent()?;
    }
}
fn binary_parts<'tree>(
    node: Node<'tree>,
    source: &[u8],
) -> Option<(Node<'tree>, std::ops::Range<usize>, Node<'tree>)> {
    let left = node.child_by_field_name("left")?;
    let right = node.child_by_field_name("right")?;
    let mut start = left.end_byte();
    let mut end = right.start_byte();
    while start < end && source.get(start).is_some_and(u8::is_ascii_whitespace) {
        start += 1;
    }
    while end > start && source.get(end - 1).is_some_and(u8::is_ascii_whitespace) {
        end -= 1;
    }
    (start < end).then_some((left, start..end, right))
}

#[derive(Clone, Copy)]
struct Bound<'a> {
    subject: &'a str,
    operator: BoundOperator,
    value: i128,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BoundOperator {
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

fn bound_from_source(source: Option<&[u8]>) -> Option<Bound<'_>> {
    let source = std::str::from_utf8(source?).ok()?.trim();
    for (token, operator) in [
        ("<=", BoundOperator::LessEqual),
        (">=", BoundOperator::GreaterEqual),
        ("<", BoundOperator::Less),
        (">", BoundOperator::Greater),
    ] {
        if let Some((subject, value)) = source.split_once(token) {
            return bound_comparison(Some(subject.as_bytes()), token, Some(value.as_bytes())).map(
                |mut bound| {
                    bound.operator = operator;
                    bound
                },
            );
        }
    }
    None
}

fn bound_comparison<'a>(
    subject: Option<&'a [u8]>,
    operator: &str,
    value: Option<&'a [u8]>,
) -> Option<Bound<'a>> {
    let subject = std::str::from_utf8(subject?).ok()?.trim();
    let value = std::str::from_utf8(value?).ok()?.trim();
    let operator = match operator {
        "<" => BoundOperator::Less,
        "<=" => BoundOperator::LessEqual,
        ">" => BoundOperator::Greater,
        ">=" => BoundOperator::GreaterEqual,
        _ => return None,
    };
    is_simple_identifier(subject).then_some(Bound {
        subject,
        operator,
        value: parse_i128(value)?,
    })
}

fn is_simple_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn parse_i128(value: &str) -> Option<i128> {
    value.parse().ok()
}

fn sibling_excludes_changed_boundary(mutated: Bound<'_>, sibling: Bound<'_>) -> bool {
    match (mutated.operator, sibling.operator) {
        (BoundOperator::Less, BoundOperator::Less) => sibling.value <= mutated.value,
        (BoundOperator::Less, BoundOperator::LessEqual) => sibling.value < mutated.value,
        (BoundOperator::Greater, BoundOperator::Greater) => sibling.value >= mutated.value,
        (BoundOperator::Greater, BoundOperator::GreaterEqual) => sibling.value > mutated.value,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mutation(source: &str, operator: &str, original: &str) -> Mutation {
        let start = source.find(original).unwrap();
        Mutation {
            id: 0,
            file: PathBuf::from("fixture.rs"),
            language: "rust".into(),
            line: 1,
            column: start + 1,
            operator: operator.into(),
            description: String::new(),
            original: original.into(),
            replacement: match operator {
                "lt_to_lte" => "<=".into(),
                "gt_to_gte" => ">=".into(),
                "and_to_or" => "||".into(),
                "or_to_and" => "&&".into(),
                _ => String::new(),
            },
            byte_range: start..start + original.len(),
        }
    }

    fn reason(source: &str, operator: &str, original: &str) -> Option<LikelyEquivalentReason> {
        let mutation = mutation(source, operator, original);
        let (tree, language) =
            crate::parser::parse_file(&mutation.file, source.as_bytes()).unwrap();
        let parsed = ParsedSource {
            source: source.into(),
            tree,
            language,
        };
        likely_equivalent_reason(&mutation, &parsed)
    }

    #[test]
    fn marks_identical_boolean_literals() {
        let source = "fn always() -> bool { true && true }";
        assert_eq!(
            reason(source, "and_to_or", "&&"),
            Some(LikelyEquivalentReason::SameBooleanLiterals)
        );
    }

    #[test]
    fn leaves_different_boolean_literals_unannotated() {
        let source = "fn sometimes() -> bool { true && false }";
        assert_eq!(reason(source, "and_to_or", "&&"), None);
    }

    #[test]
    fn marks_weakened_bound_masked_by_stricter_conjunction() {
        let source = "fn valid(value: i32) -> bool { value < 10 && value <= 9 }";
        assert_eq!(
            reason(source, "lt_to_lte", "<"),
            Some(LikelyEquivalentReason::RedundantBoundary)
        );
    }

    #[test]
    fn leaves_equal_sibling_bound_unannotated() {
        let source = "fn valid(value: i32) -> bool { value < 10 && value <= 10 }";
        assert_eq!(reason(source, "lt_to_lte", "<"), None);
    }

    #[test]
    fn leaves_custom_comparison_types_unannotated() {
        let source = "fn valid(value: Wrapped) -> bool { value < 10 && value <= 9 }";
        assert_eq!(reason(source, "lt_to_lte", "<"), None);
    }
}
