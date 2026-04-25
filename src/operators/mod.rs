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
}
