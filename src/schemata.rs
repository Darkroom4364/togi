//! Language-neutral mutation schemata planning.
//!
//! The planner identifies mutations that can be embedded behind a runtime
//! switch such as `TOGI_MUTANT=42`, while leaving unsupported or risky mutants
//! for the existing one-mutant-at-a-time runner. Execution is intentionally not
//! wired here; this module is the shared contract for future language adapters.

use crate::Mutation;
use crate::operators;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// A schema rewrite shape understood by the generic schemata engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaKind {
    /// Replace an expression with an active-mutant conditional expression.
    Expression,
    /// Replace a statement or block with an active-mutant conditional statement.
    Statement,
}

/// Mutation selected for schema execution.
#[derive(Debug, Clone)]
pub struct SchemaMutation {
    pub mutation: Mutation,
    pub kind: SchemaKind,
}

/// Mutation that must continue through the normal runner.
#[derive(Debug, Clone)]
pub struct SchemaFallback {
    pub mutation: Mutation,
    pub reason: SchemaSkipReason,
}

/// Result of schema planning.
#[derive(Debug, Clone, Default)]
pub struct SchemaPlan {
    pub selected: Vec<SchemaMutation>,
    pub fallback: Vec<SchemaFallback>,
}

/// Why a mutation is not safe for schema execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaSkipReason {
    UnsupportedLanguage,
    UnsupportedOperator,
    MissingSource,
    InvalidRange,
    OriginalMismatch,
    CompileTimeContext,
    OverlappingRange,
}

/// Language-specific syntax hooks for a generic schema execution engine.
pub trait SchemaAdapter: Send + Sync {
    /// Stable language name matching [`Mutation::language`].
    fn language(&self) -> &str;

    /// Source text to inject once per rewritten file.
    fn runtime_helper(&self) -> &'static str;

    /// Language-level dependencies needed by [`SchemaAdapter::runtime_helper`].
    fn required_imports(&self) -> &'static [&'static str] {
        &[]
    }

    /// Wrap an expression so only the active mutant observes `replacement`.
    fn wrap_expression(&self, mutant_id: u32, original: &str, replacement: &str) -> String;

    /// Wrap a statement or block so only the active mutant observes `replacement`.
    fn wrap_statement(&self, mutant_id: u32, original: &str, replacement: &str) -> String;

    /// Return whether a mutation can be represented with this adapter.
    fn classify(&self, mutation: &Mutation, source: &[u8]) -> Result<SchemaKind, SchemaSkipReason> {
        validate_source_range(mutation, source)?;
        let Some(kind) = schema_kind_for_operator(&mutation.operator) else {
            return Err(SchemaSkipReason::UnsupportedOperator);
        };
        if self.is_compile_time_context(mutation, source) {
            return Err(SchemaSkipReason::CompileTimeContext);
        }
        Ok(kind)
    }

    /// Return true when runtime switching would be too late for this mutation.
    fn is_compile_time_context(&self, _mutation: &Mutation, _source: &[u8]) -> bool {
        false
    }
}

struct GoSchema;
struct RustSchema;
struct PythonSchema;
struct TypeScriptSchema;

impl SchemaAdapter for GoSchema {
    fn language(&self) -> &str {
        "go"
    }

    fn runtime_helper(&self) -> &'static str {
        r#"func __togi_active(id string) bool {
    return os.Getenv("TOGI_MUTANT") == id
}
"#
    }

    fn required_imports(&self) -> &'static [&'static str] {
        &["os"]
    }

    fn wrap_expression(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!(
            "func() bool {{ if __togi_active(\"{mutant_id}\") {{ return {replacement} }} return {original} }}()"
        )
    }

    fn wrap_statement(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("if __togi_active(\"{mutant_id}\") {{ {replacement} }} else {{ {original} }}")
    }

    fn classify(&self, mutation: &Mutation, source: &[u8]) -> Result<SchemaKind, SchemaSkipReason> {
        validate_source_range(mutation, source)?;
        if go_line_looks_compile_time(mutation, source) {
            return Err(SchemaSkipReason::CompileTimeContext);
        }
        match mutation.operator.as_str() {
            "eq_to_neq" | "lt_to_lte" | "gt_to_gte" | "and_to_or" | "or_to_and"
            | "true_to_false" | "false_to_true" | "remove_unary_not" | "negate_condition" => {
                Ok(SchemaKind::Expression)
            }
            _ => Err(SchemaSkipReason::UnsupportedOperator),
        }
    }
}

impl SchemaAdapter for RustSchema {
    fn language(&self) -> &str {
        "rust"
    }

    fn runtime_helper(&self) -> &'static str {
        r#"#[allow(dead_code)]
fn __togi_active(id: u32) -> bool {
    std::env::var("TOGI_MUTANT").ok().as_deref() == Some(&id.to_string())
}

#[allow(dead_code)]
fn __togi_select<T, O, M>(id: u32, original: O, mutated: M) -> T
where
    O: FnOnce() -> T,
    M: FnOnce() -> T,
{
    if __togi_active(id) { mutated() } else { original() }
}
"#
    }

    fn wrap_expression(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("__togi_select({mutant_id}, || {{ {original} }}, || {{ {replacement} }})")
    }

    fn wrap_statement(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("if __togi_active({mutant_id}) {{ {replacement} }} else {{ {original} }}")
    }

    fn is_compile_time_context(&self, mutation: &Mutation, source: &[u8]) -> bool {
        rust_line_looks_compile_time(mutation, source)
    }
}

impl SchemaAdapter for PythonSchema {
    fn language(&self) -> &str {
        "python"
    }

    fn runtime_helper(&self) -> &'static str {
        r#"def __togi_active(id):
    import os
    return os.environ.get("TOGI_MUTANT") == str(id)

def __togi_select(id, original, mutated):
    return mutated() if __togi_active(id) else original()
"#
    }

    fn wrap_expression(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("__togi_select({mutant_id}, lambda: ({original}), lambda: ({replacement}))")
    }

    fn wrap_statement(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("if __togi_active({mutant_id}):\n    {replacement}\nelse:\n    {original}")
    }
}

impl SchemaAdapter for TypeScriptSchema {
    fn language(&self) -> &str {
        "typescript"
    }

    fn runtime_helper(&self) -> &'static str {
        r#"function __togi_active(id: number): boolean {
  return ((globalThis as any).process?.env?.TOGI_MUTANT) === String(id);
}

function __togi_select<T>(id: number, original: () => T, mutated: () => T): T {
  return __togi_active(id) ? mutated() : original();
}
"#
    }

    fn wrap_expression(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("__togi_select({mutant_id}, () => ({original}), () => ({replacement}))")
    }

    fn wrap_statement(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("if (__togi_active({mutant_id})) {{ {replacement} }} else {{ {original} }}")
    }
}

static GO_SCHEMA: GoSchema = GoSchema;
static RUST_SCHEMA: RustSchema = RustSchema;
static PYTHON_SCHEMA: PythonSchema = PythonSchema;
static TYPESCRIPT_SCHEMA: TypeScriptSchema = TypeScriptSchema;

/// Return the schema adapter for a language, if implemented.
pub fn adapter_for_language(language: &str) -> Option<&'static dyn SchemaAdapter> {
    match language {
        "go" => Some(&GO_SCHEMA),
        "rust" => Some(&RUST_SCHEMA),
        "python" => Some(&PYTHON_SCHEMA),
        "typescript" => Some(&TYPESCRIPT_SCHEMA),
        _ => None,
    }
}

/// Partition mutations into generic schema-compatible and fallback sets.
///
/// This validates source ranges, rejects unsupported languages/operators, and
/// ensures at most one schema mutation touches any byte in a file.
pub fn plan(project_root: &Path, mutations: Vec<Mutation>) -> SchemaPlan {
    let mut source_cache: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    let mut selected = Vec::new();
    let mut fallback = Vec::new();

    for mutation in mutations {
        let Some(adapter) = adapter_for_language(&mutation.language) else {
            fallback.push(SchemaFallback {
                mutation,
                reason: SchemaSkipReason::UnsupportedLanguage,
            });
            continue;
        };

        let path = source_path(project_root, &mutation.file);
        let source = source_cache
            .entry(path.clone())
            .or_insert_with(|| std::fs::read(&path).ok());
        let Some(source) = source.as_deref() else {
            fallback.push(SchemaFallback {
                mutation,
                reason: SchemaSkipReason::MissingSource,
            });
            continue;
        };

        match adapter.classify(&mutation, source) {
            Ok(kind) => selected.push(SchemaMutation { mutation, kind }),
            Err(reason) => fallback.push(SchemaFallback { mutation, reason }),
        }
    }

    let overlapping = overlapping_schema_indices(project_root, &selected);
    let mut final_selected = Vec::with_capacity(selected.len());
    for (idx, schema_mutation) in selected.into_iter().enumerate() {
        if overlapping.contains_key(&idx) {
            fallback.push(SchemaFallback {
                mutation: schema_mutation.mutation,
                reason: SchemaSkipReason::OverlappingRange,
            });
        } else {
            final_selected.push(schema_mutation);
        }
    }

    SchemaPlan {
        selected: final_selected,
        fallback,
    }
}

fn source_path(project_root: &Path, mutation_file: &Path) -> PathBuf {
    if mutation_file.is_absolute() {
        mutation_file.to_path_buf()
    } else {
        project_root.join(mutation_file)
    }
}

fn schema_kind_for_operator(operator: &str) -> Option<SchemaKind> {
    match operators::operator_category(operator) {
        "binary" | "literal" | "boundary" | "unary" | "negate" | "return" => {
            Some(SchemaKind::Expression)
        }
        _ => None,
    }
}

fn validate_source_range(mutation: &Mutation, source: &[u8]) -> Result<(), SchemaSkipReason> {
    if mutation.byte_range.start > mutation.byte_range.end || mutation.byte_range.end > source.len()
    {
        return Err(SchemaSkipReason::InvalidRange);
    }
    if &source[mutation.byte_range.clone()] != mutation.original.as_bytes() {
        return Err(SchemaSkipReason::OriginalMismatch);
    }
    Ok(())
}

fn overlapping_schema_indices(
    project_root: &Path,
    selected: &[SchemaMutation],
) -> BTreeMap<usize, ()> {
    let mut by_file: BTreeMap<String, Vec<(usize, std::ops::Range<usize>)>> = BTreeMap::new();
    for (idx, schema_mutation) in selected.iter().enumerate() {
        let path = source_path(project_root, &schema_mutation.mutation.file);
        by_file
            .entry(normalized_path_key(&path))
            .or_default()
            .push((idx, schema_mutation.mutation.byte_range.clone()));
    }

    let mut overlapping = BTreeMap::new();
    for ranges in by_file.values_mut() {
        ranges.sort_by_key(|(_, range)| (range.start, range.end));
        let mut previous_end = 0usize;
        for (position, (idx, range)) in ranges.iter().enumerate() {
            if position > 0 && range.start < previous_end {
                overlapping.insert(*idx, ());
            } else {
                previous_end = range.end;
            }
        }
    }
    overlapping
}

fn normalized_path_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn rust_line_looks_compile_time(mutation: &Mutation, source: &[u8]) -> bool {
    let line_start = source[..mutation.byte_range.start]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let line_end = source[mutation.byte_range.end..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|idx| mutation.byte_range.end + idx)
        .unwrap_or(source.len());
    let Ok(line) = std::str::from_utf8(&source[line_start..line_end]) else {
        return false;
    };
    let trimmed = strip_rust_visibility(line.trim_start());
    trimmed.starts_with("const ") || trimmed.starts_with("static ")
}

fn strip_rust_visibility(value: &str) -> &str {
    let Some(rest) = value.strip_prefix("pub") else {
        return value;
    };
    if rest.chars().next().is_some_and(char::is_whitespace) {
        return rest.trim_start();
    }
    if let Some(rest) = rest.strip_prefix('(') {
        if let Some((_, after_visibility)) = rest.split_once(')') {
            return after_visibility.trim_start();
        }
    }
    value
}

fn go_line_looks_compile_time(mutation: &Mutation, source: &[u8]) -> bool {
    let line_start = source[..mutation.byte_range.start]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let line_end = source[mutation.byte_range.end..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|idx| mutation.byte_range.end + idx)
        .unwrap_or(source.len());
    let Ok(line) = std::str::from_utf8(&source[line_start..line_end]) else {
        return false;
    };
    let trimmed = line.trim_start();
    trimmed.starts_with("const ")
        || trimmed.starts_with("const(")
        || source[..mutation.byte_range.start]
            .split(|byte| *byte == b'\n')
            .filter_map(|line| std::str::from_utf8(line).ok())
            .fold(false, |in_const_block, line| {
                let trimmed = line.trim_start();
                if in_const_block && trimmed.starts_with(')') {
                    false
                } else {
                    in_const_block
                        || trimmed.starts_with("const (")
                        || trimmed.starts_with("const(")
                }
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mutation(
        language: &str,
        file: &str,
        original: &str,
        replacement: &str,
        byte_range: std::ops::Range<usize>,
        operator: &str,
    ) -> Mutation {
        Mutation {
            id: 7,
            file: PathBuf::from(file),
            language: language.to_string(),
            line: 1,
            column: 1,
            operator: operator.to_string(),
            description: "test mutation".into(),
            original: original.into(),
            replacement: replacement.into(),
            byte_range,
        }
    }

    fn write_source(dir: &TempDir, file: &str, content: &str) {
        let path = dir.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn assert_parsed_node_kind(dir: &TempDir, file: &str, adapter: &dyn SchemaAdapter, kind: &str) {
        let source = std::fs::read(dir.path().join(file)).unwrap();
        let (tree, lang) = crate::parser::parse_file(Path::new(file), &source).unwrap();
        assert_eq!(lang.name(), adapter.language());
        assert!(
            crate::test_helpers::find_node_by_kind(tree.root_node(), kind).is_some(),
            "expected {file} to parse a {kind} node; tree: {}",
            tree.root_node().to_sexp()
        );
    }

    fn assert_parsed_node_text(
        dir: &TempDir,
        file: &str,
        adapter: &dyn SchemaAdapter,
        kind: &str,
        text: &str,
    ) {
        let source = std::fs::read(dir.path().join(file)).unwrap();
        let (tree, lang) = crate::parser::parse_file(Path::new(file), &source).unwrap();
        assert_eq!(lang.name(), adapter.language());
        assert!(
            find_node_by_kind_and_text(tree.root_node(), &source, kind, text).is_some(),
            "expected {file} to parse a {kind} node covering {text:?}; tree: {}",
            tree.root_node().to_sexp()
        );
    }

    fn find_node_by_kind_and_text<'tree>(
        node: tree_sitter::Node<'tree>,
        source: &[u8],
        kind: &str,
        text: &str,
    ) -> Option<tree_sitter::Node<'tree>> {
        if node.kind() == kind && &source[node.byte_range()] == text.as_bytes() {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(found) = find_node_by_kind_and_text(child, source, kind, text) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn adapters_wrap_expressions_with_language_syntax() {
        let dir = TempDir::new().unwrap();

        let go = adapter_for_language("go").unwrap();
        write_source(
            &dir,
            "calc.go",
            "package calc\nfunc f(a, b int) bool { return a == b }\n",
        );
        assert_parsed_node_text(&dir, "calc.go", go, "binary_expression", "a == b");
        assert_eq!(
            go.wrap_expression(42, "a == b", "a != b"),
            "func() bool { if __togi_active(\"42\") { return a != b } return a == b }()"
        );
        assert_eq!(go.required_imports(), &["os"]);

        let rust = adapter_for_language("rust").unwrap();
        write_source(
            &dir,
            "src/lib.rs",
            "fn f(a: i32, b: i32) -> bool { a == b }\n",
        );
        assert_parsed_node_text(&dir, "src/lib.rs", rust, "binary_expression", "a == b");
        assert_eq!(
            rust.wrap_expression(42, "a == b", "a != b"),
            "__togi_select(42, || { a == b }, || { a != b })"
        );

        let python = adapter_for_language("python").unwrap();
        write_source(&dir, "app.py", "def f(a, b):\n    return a == b\n");
        assert_parsed_node_text(&dir, "app.py", python, "comparison_operator", "a == b");
        assert_eq!(
            python.wrap_expression(42, "a == b", "a != b"),
            "__togi_select(42, lambda: (a == b), lambda: (a != b))"
        );

        let typescript = adapter_for_language("typescript").unwrap();
        write_source(
            &dir,
            "app.ts",
            "function f(a: number, b: number): boolean { return a === b; }\n",
        );
        assert_parsed_node_text(&dir, "app.ts", typescript, "binary_expression", "a === b");
        assert_eq!(
            typescript.wrap_expression(42, "a === b", "a !== b"),
            "__togi_select(42, () => (a === b), () => (a !== b))"
        );
    }

    #[test]
    fn plan_selects_expression_mutations_for_supported_languages() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("go").unwrap();
        write_source(
            &dir,
            "calc.go",
            "package calc\nfunc f(a, b int) bool { return a == b }\n",
        );
        assert_parsed_node_text(&dir, "calc.go", adapter, "binary_expression", "a == b");
        let start = "package calc\nfunc f(a, b int) bool { return ".len();
        let mutation = mutation(
            "go",
            "calc.go",
            "a == b",
            "a != b",
            start..start + 6,
            "eq_to_neq",
        );

        let plan = plan(dir.path(), vec![mutation]);

        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.selected[0].kind, SchemaKind::Expression);
        assert!(plan.fallback.is_empty());
    }

    #[test]
    fn plan_falls_back_for_go_non_boolean_expression_mutations() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("go").unwrap();
        write_source(
            &dir,
            "calc.go",
            "package calc\nfunc f(a, b int) int { return a + b }\n",
        );
        assert_parsed_node_text(&dir, "calc.go", adapter, "binary_expression", "a + b");
        let start = "package calc\nfunc f(a, b int) int { return ".len();
        let mutation = mutation(
            "go",
            "calc.go",
            "a + b",
            "a - b",
            start..start + 5,
            "plus_to_minus",
        );

        let plan = plan(dir.path(), vec![mutation]);

        assert!(plan.selected.is_empty());
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(
            plan.fallback[0].reason,
            SchemaSkipReason::UnsupportedOperator
        );
    }

    #[test]
    fn plan_falls_back_for_go_const_block_context() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("go").unwrap();
        write_source(
            &dir,
            "calc.go",
            "package calc\nconst (\n    Enabled = true\n)\n",
        );
        assert_parsed_node_kind(&dir, "calc.go", adapter, "const_declaration");
        assert_parsed_node_text(&dir, "calc.go", adapter, "true", "true");
        let start = "package calc\nconst (\n    Enabled = ".len();
        let mutation = mutation(
            "go",
            "calc.go",
            "true",
            "false",
            start..start + 4,
            "true_to_false",
        );

        let plan = plan(dir.path(), vec![mutation]);

        assert!(plan.selected.is_empty());
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(
            plan.fallback[0].reason,
            SchemaSkipReason::CompileTimeContext
        );
    }

    #[test]
    fn plan_falls_back_for_unsupported_language_and_operator() {
        let dir = TempDir::new().unwrap();
        write_source(&dir, "src/app.rb", "value = true\n");
        write_source(&dir, "src/lib.rs", "fn f() { call(); }\n");
        let rust = adapter_for_language("rust").unwrap();
        assert_parsed_node_kind(&dir, "src/lib.rs", rust, "expression_statement");
        assert_parsed_node_text(&dir, "src/lib.rs", rust, "call_expression", "call()");
        let unsupported_language = mutation(
            "ruby",
            "src/app.rb",
            "true",
            "false",
            8..12,
            "true_to_false",
        );
        let unsupported_operator = mutation(
            "rust",
            "src/lib.rs",
            "call();",
            "",
            "fn f() { ".len().."fn f() { call();".len(),
            "remove_call_statement",
        );

        let plan = plan(dir.path(), vec![unsupported_language, unsupported_operator]);

        assert!(plan.selected.is_empty());
        assert_eq!(plan.fallback.len(), 2);
        assert_eq!(
            plan.fallback[0].reason,
            SchemaSkipReason::UnsupportedLanguage
        );
        assert_eq!(
            plan.fallback[1].reason,
            SchemaSkipReason::UnsupportedOperator
        );
    }

    #[test]
    fn plan_falls_back_for_rust_compile_time_context() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("rust").unwrap();
        write_source(&dir, "src/lib.rs", "pub const VERSION: u16 = 2;\n");
        assert_parsed_node_kind(&dir, "src/lib.rs", adapter, "const_item");
        assert_parsed_node_text(&dir, "src/lib.rs", adapter, "integer_literal", "2");
        let start = "pub const VERSION: u16 = ".len();
        let mutation = mutation(
            "rust",
            "src/lib.rs",
            "2",
            "3",
            start..start + 1,
            "increment_numeric",
        );

        let plan = plan(dir.path(), vec![mutation]);

        assert!(plan.selected.is_empty());
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(
            plan.fallback[0].reason,
            SchemaSkipReason::CompileTimeContext
        );
    }

    #[test]
    fn plan_falls_back_for_overlapping_ranges_after_first_mutation() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("rust").unwrap();
        write_source(
            &dir,
            "src/lib.rs",
            "fn f(a: i32, b: i32) -> bool { a == b }\n",
        );
        assert_parsed_node_text(&dir, "src/lib.rs", adapter, "binary_expression", "a == b");
        let start = "fn f(a: i32, b: i32) -> bool { ".len();
        let first = mutation(
            "rust",
            "src/lib.rs",
            "a == b",
            "a != b",
            start..start + 6,
            "eq_to_neq",
        );
        let second = mutation(
            "rust",
            "src/lib.rs",
            "a == b",
            "a <= b",
            start..start + 6,
            "lt_to_lte",
        );

        let plan = plan(dir.path(), vec![first, second]);

        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(plan.fallback[0].reason, SchemaSkipReason::OverlappingRange);
    }

    #[test]
    fn plan_detects_overlapping_ranges_with_absolute_and_relative_paths() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("rust").unwrap();
        write_source(
            &dir,
            "src/lib.rs",
            "fn f(a: i32, b: i32) -> bool { a == b }\n",
        );
        assert_parsed_node_text(&dir, "src/lib.rs", adapter, "binary_expression", "a == b");
        let start = "fn f(a: i32, b: i32) -> bool { ".len();
        let relative = mutation(
            "rust",
            "src/lib.rs",
            "a == b",
            "a != b",
            start..start + 6,
            "eq_to_neq",
        );
        let absolute = mutation(
            "rust",
            dir.path().join("src/lib.rs").to_str().unwrap(),
            "a == b",
            "a <= b",
            start..start + 6,
            "lt_to_lte",
        );

        let plan = plan(dir.path(), vec![relative, absolute]);

        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(plan.fallback[0].reason, SchemaSkipReason::OverlappingRange);
    }
}
