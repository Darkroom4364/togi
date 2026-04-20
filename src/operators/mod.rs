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
