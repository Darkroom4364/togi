pub mod c;
pub mod cpp;
pub mod csharp;
pub mod go;
pub mod java;
pub mod python;
pub mod ruby;
pub mod rust_lang;
pub mod typescript;

/// Generates a `LanguageSupport` impl from declarative config.
///
/// Required: struct name, language name, extensions, tree-sitter language path.
/// Optional overrides (with defaults matching C-family languages):
///   binary_expression: "binary_expression"
///   if_statement: "if_statement"
///   return_statement: "return_statement"
///   bool_true: ["true"]
///   bool_false: ["false"]
///   operator_field: "operator"
///   skip_subtree_kinds: []
macro_rules! define_language {
    (
        $struct:ident,
        name: $name:expr,
        extensions: [$($ext:expr),* $(,)?],
        ts_language: $ts_lang:expr
        $(, binary_expression: $bin:expr)?
        $(, if_statement: $if_node:expr)?
        $(, return_statement: $ret:expr)?
        $(, bool_true: [$($bt:expr),* $(,)?])?
        $(, bool_false: [$($bf:expr),* $(,)?])?
        $(, operator_field: $op:expr)?
        $(, skip_subtree_kinds: [$($sk:expr),* $(,)?])?
        $(, empty_block_replacement: $ebr:expr)?
        $(,)?
    ) => {
        pub struct $struct;

        impl $crate::languages::LanguageSupport for $struct {
            fn name(&self) -> &str { $name }
            fn extensions(&self) -> &[&str] { &[$($ext),*] }
            fn tree_sitter_language(&self) -> tree_sitter::Language { $ts_lang.into() }
            fn binary_expression_node(&self) -> &str {
                $crate::languages::define_language!(@first $($bin)? ; "binary_expression")
            }
            fn if_statement_node(&self) -> &str {
                $crate::languages::define_language!(@first $($if_node)? ; "if_statement")
            }
            fn return_statement_node(&self) -> &str {
                $crate::languages::define_language!(@first $($ret)? ; "return_statement")
            }
            fn boolean_true_literals(&self) -> &[&str] {
                $crate::languages::define_language!(@arr [$($($bt),*)?] ; ["true"])
            }
            fn boolean_false_literals(&self) -> &[&str] {
                $crate::languages::define_language!(@arr [$($($bf),*)?] ; ["false"])
            }
            fn operator_field(&self) -> &str {
                $crate::languages::define_language!(@first $($op)? ; "operator")
            }
            fn skip_subtree_kinds(&self) -> &[&str] {
                $crate::languages::define_language!(@arr [$($($sk),*)?] ; [])
            }
            $crate::languages::define_language!(@fixup $($ebr)?);
        }
    };
    // Helper: return first value if present, otherwise default
    (@first $val:expr ; $default:expr) => { $val };
    (@first ; $default:expr) => { $default };
    // Helper: return array if non-empty, otherwise default
    (@arr [$($val:expr),+] ; [$($default:expr),*]) => { &[$($val),+] };
    (@arr [] ; [$($default:expr),*]) => { &[$($default),*] };
    // Helper: override fixup_replacement if empty_block_replacement is set
    (@fixup $replacement:expr) => {
        fn fixup_replacement(&self, candidate: &mut $crate::MutationCandidate) {
            if candidate.operator_id == "remove_if_body" && candidate.replacement == "{}" {
                candidate.replacement = $replacement.to_string();
            }
        }
    };
    (@fixup) => {};
}

pub(crate) use define_language;

/// Language-specific configuration for tree-sitter parsing and node identification
pub trait LanguageSupport: Send + Sync {
    fn name(&self) -> &str;
    fn extensions(&self) -> &[&str];
    fn tree_sitter_language(&self) -> tree_sitter::Language;
    fn binary_expression_node(&self) -> &str;
    fn if_statement_node(&self) -> &str;
    fn boolean_true_literals(&self) -> &[&str];
    fn boolean_false_literals(&self) -> &[&str];
    fn return_statement_node(&self) -> &str;
    fn operator_field(&self) -> &str;

    /// AST node kinds that should suppress mutation of any descendant.
    /// Nodes whose ancestor matches any of these kinds will be skipped.
    fn skip_subtree_kinds(&self) -> &[&str] {
        &[]
    }

    /// Content-aware node skip check. Called during tree walking to skip
    /// entire subtrees (e.g., test modules, test functions).
    fn should_skip_node(&self, _node: &tree_sitter::Node, _source: &[u8]) -> bool {
        false
    }

    /// Return true when this language should suppress a generated mutation.
    fn should_filter_candidate(
        &self,
        _candidate: &crate::MutationCandidate,
        _node: &tree_sitter::Node,
        _source: &[u8],
    ) -> bool {
        false
    }

    /// Adjust a mutation candidate's replacement for language-specific syntax.
    /// For example, Python replaces `{}` (empty block) with `pass`.
    fn fixup_replacement(&self, _candidate: &mut crate::MutationCandidate) {}
}

/// Returns instances of all supported languages.
pub fn all() -> Vec<Box<dyn LanguageSupport>> {
    vec![
        Box::new(c::C),
        Box::new(cpp::Cpp),
        Box::new(csharp::CSharp),
        Box::new(go::Go),
        Box::new(java::Java),
        Box::new(python::Python),
        Box::new(ruby::Ruby),
        Box::new(rust_lang::Rust),
        Box::new(typescript::TypeScript),
    ]
}
