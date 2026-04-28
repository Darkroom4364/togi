//! Mutation switching for interpreted languages.
//!
//! Instead of rewriting a file N times (once per mutation), rewrite it once
//! with all mutations embedded as conditional expressions, then run the test
//! suite N times toggling `TOGI_MUTANT=<id>`.

use crate::Mutation;

/// Languages that support mutation switching (interpreted, no compile step).
const SWITCHABLE_LANGUAGES: &[&str] = &["python", "ruby", "typescript"];

/// Operators whose byte range covers a full expression value, making it safe
/// to wrap in a conditional. Token-level operators (e.g., `<` → `<=` inside
/// a binary expression) produce invalid code when wrapped and must use the
/// one-at-a-time path.
const EXPRESSION_LEVEL_OPERATORS: &[&str] = &[
    "true_to_false",
    "false_to_true",
    "zero_to_one",
    "string_to_empty",
    "increment_numeric",
    "decrement_numeric",
    "return_empty",
    "negate_condition",
];

/// Check if a language supports mutation switching.
pub fn is_switchable_language(language: &str) -> bool {
    SWITCHABLE_LANGUAGES.contains(&language)
}

/// Check if a mutation can be embedded as a conditional expression.
/// Only expression-level operators (where the byte range covers a complete
/// expression) are safe. Token-level operators like `lt_to_lte` replace
/// a token inside a larger expression, producing invalid code if wrapped.
pub fn is_switchable_mutation(mutation: &Mutation) -> bool {
    EXPRESSION_LEVEL_OPERATORS.contains(&mutation.operator.as_str())
}

/// Partition a file's mutations into (switchable, fallback).
/// Statement-level mutations and those with overlapping non-identical byte
/// ranges go to fallback; the rest are switchable.
pub fn partition_mutations(mutations: Vec<Mutation>) -> (Vec<Mutation>, Vec<Mutation>) {
    let mut switchable = Vec::new();
    let mut fallback = Vec::new();

    for m in mutations {
        if is_switchable_mutation(&m) {
            switchable.push(m);
        } else {
            fallback.push(m);
        }
    }

    // Detect overlapping non-identical byte ranges among switchable mutations.
    // Adjacent ranges (e.g. [10..15) and [15..20)) are NOT overlapping.
    let mut overlap_indices = std::collections::HashSet::new();
    for i in 0..switchable.len() {
        for j in (i + 1)..switchable.len() {
            let a = &switchable[i].byte_range;
            let b = &switchable[j].byte_range;
            if a != b && a.start < b.end && b.start < a.end {
                overlap_indices.insert(i);
                overlap_indices.insert(j);
            }
        }
    }

    if !overlap_indices.is_empty() {
        let mut new_switchable = Vec::new();
        for (i, m) in switchable.into_iter().enumerate() {
            if overlap_indices.contains(&i) {
                fallback.push(m);
            } else {
                new_switchable.push(m);
            }
        }
        switchable = new_switchable;
    }

    (switchable, fallback)
}

/// Generate the conditional expression for a single mutation in the given language.
fn conditional_expr(language: &str, mutation_id: u32, replacement: &str, original: &str) -> String {
    match language {
        "python" => format!(
            "(({replacement}) if __import__('os').environ.get('TOGI_MUTANT')=='{mutation_id}' else ({original}))"
        ),
        "ruby" => format!("(ENV['TOGI_MUTANT']=='{mutation_id}'?({replacement}):({original}))"),
        // TypeScript / JavaScript
        _ => format!("(process.env.TOGI_MUTANT==='{mutation_id}'?({replacement}):({original}))"),
    }
}

/// Rewrite source bytes with all switchable mutations embedded as conditionals.
/// Returns the rewritten source. Mutations must be for the same file.
pub fn rewrite_source(source: &[u8], mutations: &[Mutation], language: &str) -> Vec<u8> {
    if mutations.is_empty() {
        return source.to_vec();
    }

    // Group mutations by byte range
    let mut groups: std::collections::BTreeMap<(usize, usize), Vec<&Mutation>> =
        std::collections::BTreeMap::new();
    for m in mutations {
        groups
            .entry((m.byte_range.start, m.byte_range.end))
            .or_default()
            .push(m);
    }

    // Process groups in reverse byte order to avoid offset shifts
    let mut result = source.to_vec();
    for ((start, end), group) in groups.into_iter().rev() {
        let original = std::str::from_utf8(&source[start..end]).unwrap_or("");

        // Build chained conditional: cond(A) ? repl_A : (cond(B) ? repl_B : original)
        let mut expr = original.to_string();
        for m in group.iter().rev() {
            expr = conditional_expr(language, m.id, &m.replacement, &expr);
        }

        // Splice the conditional into the result
        let expr_bytes = expr.into_bytes();
        result.splice(start..end, expr_bytes);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_mutation(
        id: u32,
        operator: &str,
        start: usize,
        end: usize,
        replacement: &str,
    ) -> Mutation {
        Mutation {
            id,
            file: PathBuf::from("test.py"),
            language: "python".into(),
            line: 1,
            column: 1,
            operator: operator.into(),
            description: "test".into(),
            original: "x".into(),
            replacement: replacement.into(),
            byte_range: start..end,
        }
    }

    #[test]
    fn switchable_mutation_classification() {
        // Expression-level: safe to wrap in conditional
        let literal = make_mutation(0, "true_to_false", 0, 4, "false");
        assert!(is_switchable_mutation(&literal));

        let numeric = make_mutation(0, "zero_to_one", 0, 1, "1");
        assert!(is_switchable_mutation(&numeric));

        let ret = make_mutation(0, "return_empty", 0, 5, "0");
        assert!(is_switchable_mutation(&ret));

        // Token-level: NOT safe (replaces operator token inside expression)
        let token = make_mutation(0, "lt_to_lte", 0, 1, "<=");
        assert!(!is_switchable_mutation(&token));

        let boundary = make_mutation(0, "plus_to_minus", 0, 1, "-");
        assert!(!is_switchable_mutation(&boundary));

        // Statement-level: NOT safe
        let stmt = make_mutation(0, "remove_if_body", 0, 10, "{}");
        assert!(!is_switchable_mutation(&stmt));

        let brk = make_mutation(0, "remove_break", 0, 5, "");
        assert!(!is_switchable_mutation(&brk));
    }

    #[test]
    fn partition_separates_non_expression_level() {
        let mutations = vec![
            make_mutation(0, "lt_to_lte", 0, 1, "<="), // token-level → fallback
            make_mutation(1, "remove_if_body", 5, 15, "{}"), // statement-level → fallback
            make_mutation(2, "true_to_false", 20, 24, "false"), // expression-level → switchable
        ];
        let (switchable, fallback) = partition_mutations(mutations);
        assert_eq!(switchable.len(), 1);
        assert_eq!(switchable[0].operator, "true_to_false");
        assert_eq!(fallback.len(), 2);
    }

    #[test]
    fn partition_moves_overlapping_to_fallback() {
        let mutations = vec![
            make_mutation(0, "true_to_false", 10, 15, "false"),
            make_mutation(1, "zero_to_one", 12, 18, "1"), // overlaps with above
            make_mutation(2, "true_to_false", 30, 34, "false"), // no overlap
        ];
        let (switchable, fallback) = partition_mutations(mutations);
        assert_eq!(switchable.len(), 1);
        assert_eq!(switchable[0].id, 2);
        assert_eq!(fallback.len(), 2);
    }

    #[test]
    fn adjacent_ranges_are_not_overlapping() {
        let mutations = vec![
            make_mutation(0, "true_to_false", 10, 14, "false"),
            make_mutation(1, "zero_to_one", 14, 15, "1"), // adjacent, not overlapping
        ];
        let (switchable, fallback) = partition_mutations(mutations);
        assert_eq!(switchable.len(), 2);
        assert_eq!(fallback.len(), 0);
    }

    #[test]
    fn conditional_expr_python() {
        let expr = conditional_expr("python", 42, "False", "True");
        assert!(expr.contains("__import__('os').environ.get('TOGI_MUTANT')=='42'"));
        assert!(expr.contains("(False)"));
        assert!(expr.contains("(True)"));
    }

    #[test]
    fn conditional_expr_typescript() {
        let expr = conditional_expr("typescript", 7, "!==", "===");
        assert!(expr.contains("process.env.TOGI_MUTANT==='7'"));
        assert!(expr.contains("(!==)"));
        assert!(expr.contains("(===)"));
    }

    #[test]
    fn conditional_expr_ruby() {
        let expr = conditional_expr("ruby", 3, "false", "true");
        assert!(expr.contains("ENV['TOGI_MUTANT']=='3'"));
        assert!(expr.contains("(false)"));
        assert!(expr.contains("(true)"));
    }

    #[test]
    fn rewrite_source_single_mutation() {
        let source = b"x < y";
        let mutations = vec![Mutation {
            id: 1,
            file: PathBuf::from("test.py"),
            language: "python".into(),
            line: 1,
            column: 3,
            operator: "lt_to_lte".into(),
            description: "test".into(),
            original: "<".into(),
            replacement: "<=".into(),
            byte_range: 2..3,
        }];
        let result = rewrite_source(source, &mutations, "python");
        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.starts_with("x "));
        assert!(result_str.contains("<="));
        assert!(result_str.contains("TOGI_MUTANT"));
        assert!(result_str.ends_with(" y"));
    }

    #[test]
    fn rewrite_source_same_range_multiple() {
        // Two mutations at the same byte range (e.g., 0 → 1 and 0 → -1)
        let source = b"0";
        let mutations = vec![
            Mutation {
                id: 1,
                file: PathBuf::from("test.py"),
                language: "python".into(),
                line: 1,
                column: 1,
                operator: "zero_to_one".into(),
                description: "test".into(),
                original: "0".into(),
                replacement: "1".into(),
                byte_range: 0..1,
            },
            Mutation {
                id: 2,
                file: PathBuf::from("test.py"),
                language: "python".into(),
                line: 1,
                column: 1,
                operator: "decrement_numeric".into(),
                description: "test".into(),
                original: "0".into(),
                replacement: "-1".into(),
                byte_range: 0..1,
            },
        ];
        let result = rewrite_source(source, &mutations, "python");
        let result_str = String::from_utf8(result).unwrap();
        // Should contain both mutation IDs
        assert!(result_str.contains("'1'"), "should reference mutation 1");
        assert!(result_str.contains("'2'"), "should reference mutation 2");
        // Should contain both replacements
        assert!(result_str.contains("(1)"), "should contain replacement 1");
        assert!(result_str.contains("(-1)"), "should contain replacement -1");
        // Should contain original as innermost fallback
        assert!(result_str.contains("(0)"), "should contain original 0");
    }

    #[test]
    fn rewrite_preserves_surrounding_code() {
        // Use an expression-level mutation (true_to_false)
        let source = b"if True:\n    print('yes')";
        let mutations = vec![Mutation {
            id: 5,
            file: PathBuf::from("test.py"),
            language: "python".into(),
            line: 1,
            column: 4,
            operator: "true_to_false".into(),
            description: "test".into(),
            original: "True".into(),
            replacement: "False".into(),
            byte_range: 3..7,
        }];
        let result = rewrite_source(source, &mutations, "python");
        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.starts_with("if "));
        assert!(result_str.contains("TOGI_MUTANT"));
        assert!(result_str.contains("(False)"));
        assert!(result_str.contains("(True)"));
        assert!(result_str.ends_with(":\n    print('yes')"));
    }

    #[test]
    fn rewrite_with_conditional_like_replacement() {
        let source = b"True";
        let mutations = vec![Mutation {
            id: 1,
            file: PathBuf::from("test.py"),
            language: "python".into(),
            line: 1,
            column: 1,
            operator: "true_to_false".into(),
            description: "test".into(),
            original: "True".into(),
            replacement: "foo if bar else baz".into(),
            byte_range: 0..4,
        }];
        let result = rewrite_source(source, &mutations, "python");
        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.contains("(foo if bar else baz)"));
    }

    #[test]
    fn rewrite_produces_parseable_python() {
        // Regression test: verify the rewritten source is syntactically valid
        // by parsing it with tree-sitter. Token-level mutations (lt_to_lte etc.)
        // must NOT go through switching — they'd produce invalid code.
        let source = b"x = True\ny = 0\n";
        let mutations = vec![
            Mutation {
                id: 1,
                file: PathBuf::from("test.py"),
                language: "python".into(),
                line: 1,
                column: 5,
                operator: "true_to_false".into(),
                description: "test".into(),
                original: "True".into(),
                replacement: "False".into(),
                byte_range: 4..8,
            },
            Mutation {
                id: 2,
                file: PathBuf::from("test.py"),
                language: "python".into(),
                line: 2,
                column: 5,
                operator: "zero_to_one".into(),
                description: "test".into(),
                original: "0".into(),
                replacement: "1".into(),
                byte_range: 13..14,
            },
        ];
        let result = rewrite_source(source, &mutations, "python");
        let result_str = String::from_utf8(result).unwrap();

        // Parse the rewritten source with tree-sitter Python
        let tree = crate::test_helpers::parse_python(&result_str);
        let root = tree.root_node();
        // A valid parse should have no ERROR nodes
        assert!(
            !root.has_error(),
            "rewritten Python source has parse errors:\n{result_str}"
        );
    }

    #[test]
    fn rewrite_produces_parseable_typescript() {
        let source = b"const x = true;\nconst y = 0;\n";
        let mutations = vec![
            Mutation {
                id: 1,
                file: PathBuf::from("test.ts"),
                language: "typescript".into(),
                line: 1,
                column: 11,
                operator: "true_to_false".into(),
                description: "test".into(),
                original: "true".into(),
                replacement: "false".into(),
                byte_range: 10..14,
            },
            Mutation {
                id: 2,
                file: PathBuf::from("test.ts"),
                language: "typescript".into(),
                line: 2,
                column: 11,
                operator: "zero_to_one".into(),
                description: "test".into(),
                original: "0".into(),
                replacement: "1".into(),
                byte_range: 26..27,
            },
        ];
        let result = rewrite_source(source, &mutations, "typescript");
        let result_str = String::from_utf8(result).unwrap();

        let tree = crate::test_helpers::parse_typescript(&result_str);
        let root = tree.root_node();
        assert!(
            !root.has_error(),
            "rewritten TypeScript source has parse errors:\n{result_str}"
        );
    }

    #[test]
    fn switchable_language_detection() {
        assert!(is_switchable_language("python"));
        assert!(is_switchable_language("ruby"));
        assert!(is_switchable_language("typescript"));
        assert!(!is_switchable_language("go"));
        assert!(!is_switchable_language("rust"));
        assert!(!is_switchable_language("java"));
    }
}
