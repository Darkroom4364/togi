use super::MutationOperator;
use crate::MutationCandidate;

const BREAK_KINDS: &[&str] = &[
    "break_statement",
    "break_expression", // Rust
    "break",            // Ruby
];

const CONTINUE_KINDS: &[&str] = &[
    "continue_statement",
    "continue_expression", // Rust
    "next",                // Ruby
];

pub struct RemoveBreak;

impl MutationOperator for RemoveBreak {
    fn id(&self) -> &str {
        "remove_break"
    }
    fn description(&self) -> &str {
        "Remove break statement from loop"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        _source: &[u8],
        _lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<MutationCandidate> {
        if !BREAK_KINDS.contains(&node.kind()) {
            return vec![];
        }
        vec![MutationCandidate {
            byte_range: node.byte_range(),
            replacement: String::new(),
            operator_id: self.id().to_string(),
            description: self.description().to_string(),
        }]
    }
}

pub struct RemoveContinue;

impl MutationOperator for RemoveContinue {
    fn id(&self) -> &str {
        "remove_continue"
    }
    fn description(&self) -> &str {
        "Remove continue statement from loop"
    }
    fn apply(
        &self,
        node: &tree_sitter::Node,
        _source: &[u8],
        _lang: &dyn crate::languages::LanguageSupport,
    ) -> Vec<MutationCandidate> {
        if !CONTINUE_KINDS.contains(&node.kind()) {
            return vec![];
        }
        vec![MutationCandidate {
            byte_range: node.byte_range(),
            replacement: String::new(),
            operator_id: self.id().to_string(),
            description: self.description().to_string(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{find_node_by_kind, parse_go, parse_ruby, parse_rust};

    // Go tests
    #[test]
    fn test_remove_break_go() {
        let src = "package main\nfunc f() { for { break } }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "break_statement")
            .expect("should find break_statement");
        let candidates = RemoveBreak.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "");
    }

    #[test]
    fn test_remove_break_no_match_on_continue() {
        let src = "package main\nfunc f() { for { continue } }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "continue_statement")
            .expect("should find continue_statement");
        let candidates = RemoveBreak.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_remove_continue_go() {
        let src = "package main\nfunc f() { for i := 0; i < 10; i++ { continue } }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "continue_statement")
            .expect("should find continue_statement");
        let candidates = RemoveContinue.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "");
    }

    #[test]
    fn test_remove_continue_no_match_on_break() {
        let src = "package main\nfunc f() { for { break } }";
        let tree = parse_go(src);
        let node = find_node_by_kind(tree.root_node(), "break_statement")
            .expect("should find break_statement");
        let candidates = RemoveContinue.apply(&node, src.as_bytes(), &crate::languages::go::Go);
        assert!(candidates.is_empty());
    }

    // Rust tests
    #[test]
    fn test_remove_break_rust() {
        let src = "fn f() { loop { break; } }";
        let tree = parse_rust(src);
        let node = find_node_by_kind(tree.root_node(), "break_expression")
            .expect("should find break_expression");
        let candidates =
            RemoveBreak.apply(&node, src.as_bytes(), &crate::languages::rust_lang::Rust);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "");
    }

    #[test]
    fn test_remove_continue_rust() {
        let src = "fn f() { for i in 0..10 { continue; } }";
        let tree = parse_rust(src);
        let node = find_node_by_kind(tree.root_node(), "continue_expression")
            .expect("should find continue_expression");
        let candidates =
            RemoveContinue.apply(&node, src.as_bytes(), &crate::languages::rust_lang::Rust);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "");
    }

    // Ruby tests
    #[test]
    fn test_remove_break_ruby() {
        let src = "loop do\n  break\nend";
        let tree = parse_ruby(src);
        let node = find_node_by_kind(tree.root_node(), "break").expect("should find break");
        let candidates = RemoveBreak.apply(&node, src.as_bytes(), &crate::languages::ruby::Ruby);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "");
    }

    #[test]
    fn test_remove_continue_ruby() {
        let src = "[1,2,3].each do |x|\n  next\nend";
        let tree = parse_ruby(src);
        let node = find_node_by_kind(tree.root_node(), "next").expect("should find next");
        let candidates = RemoveContinue.apply(&node, src.as_bytes(), &crate::languages::ruby::Ruby);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].replacement, "");
    }
}
