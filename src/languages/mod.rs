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
///   filter_candidate: function path
///   condition_negation: function path
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
        $(, filter_candidate: $filter:expr)?
        $(, condition_negation: $negation:expr)?
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
            $crate::languages::define_language!(@negation $($negation)?);
            $crate::languages::define_language!(@filter $($filter)?);
            $crate::languages::define_language!(@fixup $($ebr)?);
        }
    };
    // Helper: return first value if present, otherwise default
    (@first $val:expr ; $default:expr) => { $val };
    (@first ; $default:expr) => { $default };
    // Helper: return array if non-empty, otherwise default
    (@arr [$($val:expr),+] ; [$($default:expr),*]) => { &[$($val),+] };
    (@arr [] ; [$($default:expr),*]) => { &[$($default),*] };
    // Helper: override should_filter_candidate if filter_candidate is set
    (@filter $filter:expr) => {
        fn should_filter_candidate(
            &self,
            candidate: &$crate::MutationCandidate,
            node: &tree_sitter::Node,
            source: &[u8],
        ) -> bool {
            $filter(candidate, node, source)
        }
    };
    (@filter) => {};
    // Helper: override condition negation formatting if condition_negation is set
    (@negation $negation:expr) => {
        fn negate_condition_replacement(&self, condition: &str) -> String {
            $negation(condition)
        }
    };
    (@negation) => {};
    // Helper: override fixup_replacement if empty_block_replacement is set
    (@fixup $replacement:expr) => {
        fn fixup_replacement(&self, candidate: &mut $crate::MutationCandidate) {
            $crate::languages::fixup_language_replacement(self, candidate);
            if candidate.operator_id == "remove_if_body" && candidate.replacement == "{}" {
                candidate.replacement = $replacement.to_string();
            }
        }
    };
    (@fixup) => {};
}

pub(crate) use define_language;

const FUNC_NODE_KINDS: &[&str] = &[
    "function_item",
    "function_declaration",
    "method_declaration",
    "function_definition",
    "method",
];

const SIMPLE_RETURN_TYPE_KINDS: &[&str] = &[
    "primitive_type",
    "type_identifier",
    "predefined_type",
    "boolean_type",
    "void_type",
    "unit_type",
    "integral_type",
    "floating_point_type",
];

pub(crate) fn should_skip_return_empty_for_type(
    node: &tree_sitter::Node,
    skip_go_multi_return: bool,
) -> bool {
    let mut parent = node.parent();
    let func_node = loop {
        match parent {
            Some(p) if FUNC_NODE_KINDS.contains(&p.kind()) => break p,
            Some(p) => parent = p.parent(),
            None => return false,
        }
    };

    let ret_type = func_node
        .child_by_field_name("return_type")
        .or_else(|| func_node.child_by_field_name("type"))
        .or_else(|| func_node.child_by_field_name("result"));

    match ret_type {
        None => false,
        Some(rt) => {
            if skip_go_multi_return && rt.kind() == "parameter_list" {
                return true;
            }
            !SIMPLE_RETURN_TYPE_KINDS.contains(&rt.kind())
        }
    }
}

pub(crate) fn should_skip_string_to_empty_in_compiled_context(node: &tree_sitter::Node) -> bool {
    let mut parent = node.parent();
    while let Some(p) = parent {
        match p.kind() {
            "const_item" | "const_declaration" | "static_item" => return true,
            "match_arm" => return true,
            "function_item"
            | "function_declaration"
            | "method_declaration"
            | "function_definition"
            | "method" => return false,
            _ => parent = p.parent(),
        }
    }
    false
}

pub(crate) fn fixup_language_replacement<L: LanguageSupport + ?Sized>(
    lang: &L,
    candidate: &mut crate::MutationCandidate,
) {
    match candidate.operator_id.as_str() {
        "true_to_false" => {
            if let Some(literal) = lang.boolean_false_literals().first() {
                candidate.replacement = (*literal).to_string();
            }
        }
        "false_to_true" => {
            if let Some(literal) = lang.boolean_true_literals().first() {
                candidate.replacement = (*literal).to_string();
            }
        }
        "return_empty" if candidate.replacement == "false" => {
            if let Some(literal) = lang.boolean_false_literals().first() {
                candidate.replacement = (*literal).to_string();
            }
        }
        "negate_condition" => {
            if let Some(condition) = candidate
                .replacement
                .strip_prefix("!(")
                .and_then(|s| s.strip_suffix(')'))
            {
                candidate.replacement = lang.negate_condition_replacement(condition);
            }
        }
        _ => {}
    }
}

/// Describes how to parse and mutate one supported language.
///
/// The mutator uses this trait to choose the tree-sitter grammar, identify
/// language-specific node kinds, skip non-production subtrees, and suppress
/// mutation candidates that would be invalid or noisy for the language.
pub trait LanguageSupport: Send + Sync {
    /// Stable language name used in config keys and reports.
    fn name(&self) -> &str;

    /// File extensions handled by this language, without leading dots.
    fn extensions(&self) -> &[&str];

    /// Tree-sitter grammar used for parsing source files.
    fn tree_sitter_language(&self) -> tree_sitter::Language;

    /// Primary binary-expression node kind for this grammar.
    fn binary_expression_node(&self) -> &str;

    /// Primary if-statement or if-expression node kind for this grammar.
    fn if_statement_node(&self) -> &str;

    /// Node texts/kinds that represent boolean true.
    fn boolean_true_literals(&self) -> &[&str];

    /// Node texts/kinds that represent boolean false.
    fn boolean_false_literals(&self) -> &[&str];

    /// Primary return-statement or return-expression node kind.
    fn return_statement_node(&self) -> &str;

    /// Tree-sitter field name used to find operators when available.
    fn operator_field(&self) -> &str;

    /// Format a replacement that negates an unnegated condition expression.
    fn negate_condition_replacement(&self, condition: &str) -> String {
        format!("!({condition})")
    }

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
    fn fixup_replacement(&self, candidate: &mut crate::MutationCandidate) {
        fixup_language_replacement(self, candidate);
    }
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
        Box::new(typescript::Tsx),
    ]
}
