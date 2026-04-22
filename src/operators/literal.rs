use super::MutationOperator;
use crate::MutationCandidate;

const TRUE_KINDS: &[&str] = &["true", "True", "TRUE"];
const FALSE_KINDS: &[&str] = &["false", "False", "FALSE"];
const INT_LITERAL_KINDS: &[&str] = &["integer_literal", "int_literal", "number", "number_literal"];
const STRING_LITERAL_KINDS: &[&str] = &[
    "interpreted_string_literal",
    "raw_string_literal",
    "string",
    "string_literal",
    "string_content",
    "template_string",
];

pub struct TrueToFalse;

impl MutationOperator for TrueToFalse {
    fn id(&self) -> &str {
        "true_to_false"
    }
    fn description(&self) -> &str {
        "Replace true with false"
    }
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
    fn id(&self) -> &str {
        "false_to_true"
    }
    fn description(&self) -> &str {
        "Replace false with true"
    }
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
    fn id(&self) -> &str {
        "zero_to_one"
    }
    fn description(&self) -> &str {
        "Replace 0 with 1"
    }
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

pub struct StringToEmpty;

impl MutationOperator for StringToEmpty {
    fn id(&self) -> &str {
        "string_to_empty"
    }
    fn description(&self) -> &str {
        "Replace string literal with empty string"
    }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if STRING_LITERAL_KINDS.contains(&node.kind()) {
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
            return vec![MutationCandidate {
                byte_range: node.byte_range(),
                replacement,
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }];
        }
        vec![]
    }
}

pub struct IncrementNumeric;

impl MutationOperator for IncrementNumeric {
    fn id(&self) -> &str {
        "increment_numeric"
    }
    fn description(&self) -> &str {
        "Replace n with n+1"
    }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if !INT_LITERAL_KINDS.contains(&node.kind()) {
            return vec![];
        }
        let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
        if let Ok(n) = text.parse::<i64>() {
            vec![MutationCandidate {
                byte_range: node.byte_range(),
                replacement: (n + 1).to_string(),
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
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
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if !INT_LITERAL_KINDS.contains(&node.kind()) {
            return vec![];
        }
        let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
        if let Ok(n) = text.parse::<i64>() {
            vec![MutationCandidate {
                byte_range: node.byte_range(),
                replacement: (n - 1).to_string(),
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_go(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn find_node_by_kind<'a>(
        node: tree_sitter::Node<'a>,
        kind: &str,
    ) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_node_by_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    fn collect_all_candidates(
        node: tree_sitter::Node,
        source: &[u8],
        op: &dyn MutationOperator,
        out: &mut Vec<MutationCandidate>,
    ) {
        out.extend(op.apply(&node, source));
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
        let candidates = TrueToFalse.apply(&node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "false");
    }

    #[test]
    fn test_false_to_true() {
        let src = "package main\nfunc f() bool { return false }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "false").expect("should find false node");
        let candidates = FalseToTrue.apply(&node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "true");
    }

    #[test]
    fn test_zero_to_one() {
        let src = "package main\nfunc f() int { return 0 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");
        let candidates = ZeroToOne.apply(&node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "1");
    }

    #[test]
    fn test_zero_to_one_skips_nonzero() {
        let src = "package main\nfunc f() int { return 42 }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "int_literal")
            .expect("should find int_literal node");
        let candidates = ZeroToOne.apply(&node, src.as_bytes());
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
}
