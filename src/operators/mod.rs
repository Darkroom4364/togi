pub mod binary;
pub mod boundary;
pub mod literal;
pub mod removal;

/// A mutation operator that generates candidate mutations from AST nodes
pub trait MutationOperator: Send + Sync {
    fn id(&self) -> &str;
    fn description(&self) -> &str;
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<crate::MutationCandidate>;
}

const IF_STMT_KINDS: &[&str] = &["if_statement", "if_expression", "if_expr"];
const RETURN_KINDS: &[&str] = &["return_statement", "return_expression"];

/// Negate a condition: remove `!` if present, otherwise wrap with `!(...)`
pub struct NegateCondition;

impl MutationOperator for NegateCondition {
    fn id(&self) -> &str { "negate_condition" }
    fn description(&self) -> &str { "Negate condition expression" }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<crate::MutationCandidate> {
        if !IF_STMT_KINDS.contains(&node.kind()) {
            return vec![];
        }
        let cond = node.child_by_field_name("condition");
        if let Some(cond_node) = cond {
            let text = std::str::from_utf8(&source[cond_node.byte_range()]).unwrap_or("");
            let replacement = if text.starts_with('!') {
                text[1..].trim_start_matches('(').trim_end_matches(')').to_string()
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
    fn id(&self) -> &str { "return_empty" }
    fn description(&self) -> &str { "Replace return value with default" }
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
        let first = children.first().unwrap();
        let last = children.last().unwrap();
        let value_range = first.start_byte()..last.end_byte();
        let text = std::str::from_utf8(&source[value_range.clone()]).unwrap_or("");

        let replacement = if text.contains('"') || text.contains('\'') {
            "\"\"".to_string()
        } else if text == "true" || text == "false" {
            "false".to_string()
        } else {
            "0".to_string()
        };

        vec![crate::MutationCandidate {
            byte_range: value_range,
            replacement,
            operator_id: self.id().to_string(),
            description: self.description().to_string(),
        }]
    }
}

/// Returns all 14 mutation operators
pub fn all_operators() -> Vec<Box<dyn MutationOperator>> {
    vec![
        Box::new(binary::LtToLte),
        Box::new(binary::GtToGte),
        Box::new(binary::EqToNeq),
        Box::new(binary::AndToOr),
        Box::new(binary::OrToAnd),
        Box::new(literal::TrueToFalse),
        Box::new(literal::FalseToTrue),
        Box::new(literal::ZeroToOne),
        Box::new(boundary::PlusToMinus),
        Box::new(boundary::MinusToPlus),
        Box::new(removal::RemoveIfBody),
        Box::new(removal::RemoveElse),
        Box::new(NegateCondition),
        Box::new(ReturnEmpty),
    ]
}
