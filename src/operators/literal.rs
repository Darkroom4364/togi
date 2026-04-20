use crate::MutationCandidate;
use super::MutationOperator;

const TRUE_KINDS: &[&str] = &["true", "True", "TRUE"];
const FALSE_KINDS: &[&str] = &["false", "False", "FALSE"];
const INT_LITERAL_KINDS: &[&str] = &["integer_literal", "int_literal", "number", "number_literal"];

pub struct TrueToFalse;

impl MutationOperator for TrueToFalse {
    fn id(&self) -> &str { "true_to_false" }
    fn description(&self) -> &str { "Replace true with false" }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if TRUE_KINDS.contains(&node.kind()) {
            let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
            if text == "true" || text == "True" || text == "TRUE" {
                return vec![MutationCandidate {
                    byte_range: node.byte_range(),
                    replacement: "false".to_string(),
                    operator_id: self.id().to_string(),
                    description: self.description().to_string(),
                }];
            }
        }
        vec![]
    }
}

pub struct FalseToTrue;

impl MutationOperator for FalseToTrue {
    fn id(&self) -> &str { "false_to_true" }
    fn description(&self) -> &str { "Replace false with true" }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if FALSE_KINDS.contains(&node.kind()) {
            let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
            if text == "false" || text == "False" || text == "FALSE" {
                return vec![MutationCandidate {
                    byte_range: node.byte_range(),
                    replacement: "true".to_string(),
                    operator_id: self.id().to_string(),
                    description: self.description().to_string(),
                }];
            }
        }
        vec![]
    }
}

pub struct ZeroToOne;

impl MutationOperator for ZeroToOne {
    fn id(&self) -> &str { "zero_to_one" }
    fn description(&self) -> &str { "Replace 0 with 1" }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if INT_LITERAL_KINDS.contains(&node.kind()) {
            let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
            if text == "0" {
                return vec![MutationCandidate {
                    byte_range: node.byte_range(),
                    replacement: "1".to_string(),
                    operator_id: self.id().to_string(),
                    description: self.description().to_string(),
                }];
            }
        }
        vec![]
    }
}
