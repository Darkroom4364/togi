use super::MutationOperator;
use crate::MutationCandidate;

const UNARY_EXPR_KINDS: &[&str] = &["unary_expression", "unary_expr", "not_operator"];

pub struct RemoveUnaryNot;

impl MutationOperator for RemoveUnaryNot {
    fn id(&self) -> &str {
        "remove_unary_not"
    }
    fn description(&self) -> &str {
        "Remove ! operator: !x → x"
    }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if !UNARY_EXPR_KINDS.contains(&node.kind()) {
            return vec![];
        }
        let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
        if let Some(inner) = text.strip_prefix('!').or_else(|| text.strip_prefix("not ")) {
            vec![MutationCandidate {
                byte_range: node.byte_range(),
                replacement: inner.to_string(),
                operator_id: self.id().to_string(),
                description: self.description().to_string(),
            }]
        } else {
            vec![]
        }
    }
}

pub struct RemoveUnaryNeg;

impl MutationOperator for RemoveUnaryNeg {
    fn id(&self) -> &str {
        "remove_unary_neg"
    }
    fn description(&self) -> &str {
        "Remove unary -: -x → x"
    }
    fn apply(&self, node: &tree_sitter::Node, source: &[u8]) -> Vec<MutationCandidate> {
        if !UNARY_EXPR_KINDS.contains(&node.kind()) {
            return vec![];
        }
        let text = std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
        if text.starts_with('-') && !text.starts_with("--") {
            let inner = &text[1..];
            vec![MutationCandidate {
                byte_range: node.byte_range(),
                replacement: inner.to_string(),
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

    fn find_first_kind<'a>(
        node: tree_sitter::Node<'a>,
        kind: &str,
    ) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_first_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_remove_unary_not() {
        let src = "package main\nfunc f(x bool) bool { return !x }";
        let tree = parse_go(src);
        let node = find_first_kind(tree.root_node(), "unary_expression").unwrap();
        let candidates = RemoveUnaryNot.apply(&node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "x");
    }

    #[test]
    fn test_remove_unary_neg() {
        let src = "package main\nfunc f(x int) int { return -x }";
        let tree = parse_go(src);
        let node = find_first_kind(tree.root_node(), "unary_expression").unwrap();
        let candidates = RemoveUnaryNeg.apply(&node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "x");
    }

    #[test]
    fn test_remove_unary_not_python() {
        let src = "x = not y";
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_python::LANGUAGE;
        parser.set_language(&lang.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let node = find_first_kind(tree.root_node(), "not_operator").unwrap();
        let candidates = RemoveUnaryNot.apply(&node, src.as_bytes());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "y");
    }

    #[test]
    fn test_unary_not_no_match_on_neg() {
        let src = "package main\nfunc f(x int) int { return -x }";
        let tree = parse_go(src);
        let node = find_first_kind(tree.root_node(), "unary_expression").unwrap();
        let candidates = RemoveUnaryNot.apply(&node, src.as_bytes());
        assert!(candidates.is_empty());
    }
}
