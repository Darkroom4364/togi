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
///   bool_true: ["true", "True", "TRUE"]
///   bool_false: ["false", "False", "FALSE"]
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
                $crate::languages::define_language!(@arr [$($($bt),*)?] ; ["true", "True", "TRUE"])
            }
            fn boolean_false_literals(&self) -> &[&str] {
                $crate::languages::define_language!(@arr [$($($bf),*)?] ; ["false", "False", "FALSE"])
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

const INTEGER_LITERAL_NODE_KINDS: &[&str] = &[
    "integer_literal",
    "int_literal",
    "integer",
    "number",
    "number_literal",
];

const STRING_LITERAL_NODE_KINDS: &[&str] = &[
    "interpreted_string_literal",
    "raw_string_literal",
    "string",
    "string_literal",
    "template_string",
];

const UNARY_EXPRESSION_NODE_KINDS: &[&str] = &["unary_expression", "unary_expr", "not_operator"];

const EXPRESSION_STATEMENT_NODE_KINDS: &[&str] = &["expression_statement", "expression_stmt"];

const CALL_EXPRESSION_NODE_KINDS: &[&str] = &[
    "call_expression",
    "call",
    "method_invocation",
    "invocation_expression",
];

const ASSIGNMENT_NODE_KINDS: &[&str] = &[
    "assignment_statement",
    "assignment_expression",
    "assignment",
    "augmented_assignment",
    "augmented_assignment_expression",
];

pub(crate) fn is_default_mutable_node_kind<L: LanguageSupport + ?Sized>(
    lang: &L,
    kind: &str,
) -> bool {
    kind == lang.binary_expression_node()
        || kind == lang.if_statement_node()
        || kind == lang.return_statement_node()
        || kind == "binary_expr"
        || kind == "comparison_expression"
        || kind == "if_expr"
        || lang.is_boolean_true_literal_node(kind)
        || lang.is_boolean_false_literal_node(kind)
        || lang.is_integer_literal_node(kind)
        || lang.is_string_literal_node(kind)
        || lang.is_unary_expression_node(kind)
        || lang.is_expression_statement_node(kind)
        || lang.is_assignment_node(kind)
        || lang.is_break_node(kind)
        || lang.is_continue_node(kind)
}

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

    /// Return true when `kind` can hold a boolean true literal for this grammar.
    fn is_boolean_true_literal_node(&self, kind: &str) -> bool {
        matches!(kind, "true" | "True" | "TRUE" | "boolean_literal")
            || self.boolean_true_literals().contains(&kind)
    }

    /// Return true when `kind` can hold a boolean false literal for this grammar.
    fn is_boolean_false_literal_node(&self, kind: &str) -> bool {
        matches!(kind, "false" | "False" | "FALSE" | "boolean_literal")
            || self.boolean_false_literals().contains(&kind)
    }

    /// Return true when `kind` can hold an integer-style numeric literal.
    fn is_integer_literal_node(&self, kind: &str) -> bool {
        INTEGER_LITERAL_NODE_KINDS.contains(&kind)
    }

    /// Return true when `kind` can hold a string literal.
    fn is_string_literal_node(&self, kind: &str) -> bool {
        STRING_LITERAL_NODE_KINDS.contains(&kind)
    }

    /// Return true when `kind` is a unary expression node.
    fn is_unary_expression_node(&self, kind: &str) -> bool {
        UNARY_EXPRESSION_NODE_KINDS.contains(&kind)
    }

    /// Return true when `kind` is a loop break node.
    fn is_break_node(&self, kind: &str) -> bool {
        matches!(kind, "break_statement" | "break_expression" | "break")
    }

    /// Return true when `kind` is a loop continue node.
    fn is_continue_node(&self, kind: &str) -> bool {
        matches!(kind, "continue_statement" | "continue_expression" | "next")
    }

    /// Return true when `kind` is an expression statement node.
    fn is_expression_statement_node(&self, kind: &str) -> bool {
        EXPRESSION_STATEMENT_NODE_KINDS.contains(&kind)
    }

    /// Return true when `kind` is a call expression node.
    fn is_call_expression_node(&self, kind: &str) -> bool {
        CALL_EXPRESSION_NODE_KINDS.contains(&kind)
    }

    /// Return true when `kind` is an assignment node.
    fn is_assignment_node(&self, kind: &str) -> bool {
        ASSIGNMENT_NODE_KINDS.contains(&kind)
    }

    /// Return the replacement for a return value, or None if it is already empty.
    fn return_empty_replacement(&self, kind: &str, text: &str) -> Option<String> {
        if self.is_string_literal_node(kind) {
            Some("\"\"".to_string())
        } else if self.is_boolean_true_literal_node(kind)
            || self.is_boolean_false_literal_node(kind)
            || kind == "boolean"
        {
            Some("false".to_string())
        } else if matches!(kind, "null" | "nil" | "none" | "None")
            || matches!(text, "nil" | "null" | "None")
        {
            None
        } else if self.is_integer_literal_node(kind) || matches!(kind, "float_literal" | "float") {
            Some("0".to_string())
        } else if text.starts_with('"') || text.starts_with('\'') || text.starts_with('`') {
            Some("\"\"".to_string())
        } else {
            Some("0".to_string())
        }
    }

    /// Return true when `kind` is a mutation-relevant AST node kind for this language.
    fn is_mutable_node_kind(&self, kind: &str) -> bool {
        is_default_mutable_node_kind(self, kind)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_language_primary_nodes_are_mutable() {
        for lang in all() {
            assert!(
                lang.is_mutable_node_kind(lang.binary_expression_node()),
                "{} binary node should be mutable",
                lang.name()
            );
            assert!(
                lang.is_mutable_node_kind(lang.if_statement_node()),
                "{} if node should be mutable",
                lang.name()
            );
            assert!(
                lang.is_mutable_node_kind(lang.return_statement_node()),
                "{} return node should be mutable",
                lang.name()
            );
        }
    }

    #[test]
    fn shared_operator_nodes_are_mutable() {
        let lang = go::Go;

        for kind in [
            "int_literal",
            "boolean_literal",
            "string_literal",
            "expression_statement",
            "assignment_statement",
            "break_statement",
            "continue_statement",
        ] {
            assert!(lang.is_mutable_node_kind(kind), "{kind} should be mutable");
        }
        assert!(!lang.is_mutable_node_kind("identifier"));
    }
}
