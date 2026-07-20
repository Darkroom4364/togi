//! Language-neutral mutation schemata planning.
//!
//! The planner identifies mutations that can be embedded behind a runtime
//! switch such as `TOGI_MUTANT=42`, while leaving unsupported or risky mutants
//! for the existing one-mutant-at-a-time runner. Execution is intentionally not
//! wired here; this module is the shared contract for future language adapters.

use crate::Mutation;
use crate::operators;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
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

/// Rewritten source for one schema-enabled file.
#[derive(Debug, Clone)]
pub struct SchemaFileRewrite {
    pub file: PathBuf,
    pub content: Vec<u8>,
}

/// Why schema source rewriting failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaRewriteError {
    message: String,
}

impl SchemaRewriteError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SchemaRewriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SchemaRewriteError {}

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
    UnsupportedSyntaxContext,
}

impl SchemaSkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SchemaSkipReason::UnsupportedLanguage => "unsupported_language",
            SchemaSkipReason::UnsupportedOperator => "unsupported_operator",
            SchemaSkipReason::MissingSource => "missing_source",
            SchemaSkipReason::InvalidRange => "invalid_range",
            SchemaSkipReason::OriginalMismatch => "original_mismatch",
            SchemaSkipReason::CompileTimeContext => "compile_time_context",
            SchemaSkipReason::OverlappingRange => "overlapping_range",
            SchemaSkipReason::UnsupportedSyntaxContext => "unsupported_syntax_context",
        }
    }
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

struct CSchema;
struct CppSchema;
struct GoSchema;
struct JavaSchema;
struct RustSchema;
struct PythonSchema;
struct TypeScriptSchema;

const COMMON_EXPRESSION_OPERATOR_ALLOWLIST: &[&str] = &[
    "eq_to_neq",
    "lt_to_lte",
    "gt_to_gte",
    "and_to_or",
    "or_to_and",
    "mul_to_div",
    "div_to_mul",
    "mod_to_mul",
    "plus_to_minus",
    "minus_to_plus",
    "true_to_false",
    "false_to_true",
    "zero_to_one",
    "string_to_empty",
    "increment_numeric",
    "decrement_numeric",
    "remove_unary_not",
    "remove_unary_neg",
    "negate_condition",
];

impl SchemaAdapter for CSchema {
    fn language(&self) -> &str {
        "c"
    }

    fn runtime_helper(&self) -> &'static str {
        r#"static int __togi_active(unsigned int id) {
    extern char *getenv(const char *);
    const char *value = getenv("TOGI_MUTANT");
    unsigned int parsed = 0u;
    if (value == 0 || *value == '\0') {
        return 0;
    }
    while (*value >= '0' && *value <= '9') {
        parsed = parsed * 10u + (unsigned int)(*value - '0');
        value++;
    }
    return *value == '\0' && parsed == id;
}
"#
    }

    fn wrap_expression(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("(__togi_active({mutant_id}u) ? ({replacement}) : ({original}))")
    }

    fn wrap_statement(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("if (__togi_active({mutant_id}u)) {{ {replacement} }} else {{ {original} }}")
    }

    fn classify(&self, mutation: &Mutation, source: &[u8]) -> Result<SchemaKind, SchemaSkipReason> {
        validate_source_range(mutation, source)?;
        if !COMMON_EXPRESSION_OPERATOR_ALLOWLIST.contains(&mutation.operator.as_str()) {
            return Err(SchemaSkipReason::UnsupportedOperator);
        }
        if !c_range_is_runtime_context(source, mutation.byte_range.clone()) {
            return Err(SchemaSkipReason::CompileTimeContext);
        }
        Ok(SchemaKind::Expression)
    }

    fn is_compile_time_context(&self, mutation: &Mutation, source: &[u8]) -> bool {
        !c_range_is_runtime_context(source, mutation.byte_range.clone())
    }
}

impl SchemaAdapter for CppSchema {
    fn language(&self) -> &str {
        "cpp"
    }

    fn runtime_helper(&self) -> &'static str {
        r#"static bool __togi_active(unsigned int id) {
    const char *value = std::getenv("TOGI_MUTANT");
    unsigned int parsed = 0u;
    if (value == nullptr || *value == '\0') {
        return false;
    }
    while (*value >= '0' && *value <= '9') {
        parsed = parsed * 10u + static_cast<unsigned int>(*value - '0');
        value++;
    }
    return *value == '\0' && parsed == id;
}
"#
    }

    fn required_imports(&self) -> &'static [&'static str] {
        &["cstdlib"]
    }

    fn wrap_expression(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("(::__togi_active({mutant_id}u) ? ({replacement}) : ({original}))")
    }

    fn wrap_statement(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("if (::__togi_active({mutant_id}u)) {{ {replacement} }} else {{ {original} }}")
    }

    fn classify(&self, mutation: &Mutation, source: &[u8]) -> Result<SchemaKind, SchemaSkipReason> {
        validate_source_range(mutation, source)?;
        if !COMMON_EXPRESSION_OPERATOR_ALLOWLIST.contains(&mutation.operator.as_str()) {
            return Err(SchemaSkipReason::UnsupportedOperator);
        }
        if !cpp_range_is_runtime_context(source, mutation.byte_range.clone()) {
            return Err(SchemaSkipReason::CompileTimeContext);
        }
        Ok(SchemaKind::Expression)
    }

    fn is_compile_time_context(&self, mutation: &Mutation, source: &[u8]) -> bool {
        !cpp_range_is_runtime_context(source, mutation.byte_range.clone())
    }
}

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
            "func() bool {{ if __togi_active(\"{mutant_id}\") {{ return {replacement} }}; return {original} }}()"
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

impl SchemaAdapter for JavaSchema {
    fn language(&self) -> &str {
        "java"
    }

    fn runtime_helper(&self) -> &'static str {
        r#"    private static boolean __togi_active(int id) {
        return Integer.toString(id).equals(System.getenv("TOGI_MUTANT"));
    }
"#
    }

    fn wrap_expression(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("(__togi_active({mutant_id}) ? ({replacement}) : ({original}))")
    }

    fn wrap_statement(&self, mutant_id: u32, original: &str, replacement: &str) -> String {
        format!("if (__togi_active({mutant_id})) {{ {replacement} }} else {{ {original} }}")
    }

    fn classify(&self, mutation: &Mutation, source: &[u8]) -> Result<SchemaKind, SchemaSkipReason> {
        validate_source_range(mutation, source)?;
        if java_line_looks_compile_time(mutation, source) {
            return Err(SchemaSkipReason::CompileTimeContext);
        }
        if COMMON_EXPRESSION_OPERATOR_ALLOWLIST.contains(&mutation.operator.as_str()) {
            Ok(SchemaKind::Expression)
        } else {
            Err(SchemaSkipReason::UnsupportedOperator)
        }
    }

    fn is_compile_time_context(&self, mutation: &Mutation, source: &[u8]) -> bool {
        java_line_looks_compile_time(mutation, source)
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

    fn classify(&self, mutation: &Mutation, source: &[u8]) -> Result<SchemaKind, SchemaSkipReason> {
        validate_source_range(mutation, source)?;
        if rust_line_looks_compile_time(mutation, source) {
            return Err(SchemaSkipReason::CompileTimeContext);
        }
        if COMMON_EXPRESSION_OPERATOR_ALLOWLIST.contains(&mutation.operator.as_str()) {
            Ok(SchemaKind::Expression)
        } else {
            Err(SchemaSkipReason::UnsupportedOperator)
        }
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

static C_SCHEMA: CSchema = CSchema;
static CPP_SCHEMA: CppSchema = CppSchema;
static GO_SCHEMA: GoSchema = GoSchema;
static JAVA_SCHEMA: JavaSchema = JavaSchema;
static RUST_SCHEMA: RustSchema = RustSchema;
static PYTHON_SCHEMA: PythonSchema = PythonSchema;
static TYPESCRIPT_SCHEMA: TypeScriptSchema = TypeScriptSchema;

/// Return the schema adapter for a language, if implemented.
pub fn adapter_for_language(language: &str) -> Option<&'static dyn SchemaAdapter> {
    match language {
        "c" => Some(&C_SCHEMA),
        "cpp" => Some(&CPP_SCHEMA),
        "go" => Some(&GO_SCHEMA),
        "java" => Some(&JAVA_SCHEMA),
        "rust" => Some(&RUST_SCHEMA),
        "python" => Some(&PYTHON_SCHEMA),
        "typescript" => Some(&TYPESCRIPT_SCHEMA),
        _ => None,
    }
}

/// Partition mutations into generic schema-compatible and fallback sets.
///
/// This validates source ranges, rejects unsupported languages/operators, and
/// ensures at most one schema mutation touches any conflicting range in a file.
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

    let (overlapping, conflict_failures) =
        schema_conflict_analysis_with_sources(project_root, &selected, &source_cache);
    let mut final_selected = Vec::with_capacity(selected.len());
    for (idx, schema_mutation) in selected.into_iter().enumerate() {
        if let Some(reason) = conflict_failures.get(&idx) {
            fallback.push(SchemaFallback {
                mutation: schema_mutation.mutation,
                reason: *reason,
            });
        } else if overlapping.contains_key(&idx) {
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

type ExpressionRangeResolver = for<'tree> fn(
    tree_sitter::Node<'tree>,
    &str,
    &Mutation,
) -> Result<std::ops::Range<usize>, SchemaRewriteError>;

fn require_adapter(language: &str) -> Result<&'static dyn SchemaAdapter, SchemaRewriteError> {
    adapter_for_language(language).ok_or_else(|| {
        SchemaRewriteError::new(format!("{language} schema adapter is not available"))
    })
}

fn group_expression_mutations_by_file<'a>(
    project_root: &Path,
    selected: &'a [SchemaMutation],
    language: &str,
) -> Result<BTreeMap<PathBuf, Vec<&'a SchemaMutation>>, SchemaRewriteError> {
    let mut by_file: BTreeMap<PathBuf, Vec<&SchemaMutation>> = BTreeMap::new();
    for mutation in selected {
        if mutation.mutation.language != language {
            return Err(SchemaRewriteError::new(format!(
                "schema rewrite only supports {language}, got {}",
                mutation.mutation.language
            )));
        }
        if mutation.kind != SchemaKind::Expression {
            return Err(SchemaRewriteError::new(format!(
                "{language} schema rewrite currently supports expression mutations only"
            )));
        }
        by_file
            .entry(source_path(project_root, &mutation.mutation.file))
            .or_default()
            .push(mutation);
    }
    Ok(by_file)
}

fn rewrite_expression_files_for_language(
    project_root: &Path,
    selected: &[SchemaMutation],
    language: &str,
    adapter: &dyn SchemaAdapter,
    rewrite_file: impl Fn(
        &str,
        &[&SchemaMutation],
        &dyn SchemaAdapter,
    ) -> Result<String, SchemaRewriteError>,
) -> Result<Vec<SchemaFileRewrite>, SchemaRewriteError> {
    let by_file = group_expression_mutations_by_file(project_root, selected, language)?;
    let mut rewritten = Vec::new();
    for (file, mutations) in by_file {
        let source = std::fs::read_to_string(&file).map_err(|e| {
            SchemaRewriteError::new(format!("could not read {}: {e}", file.display()))
        })?;
        let rewritten_source = rewrite_file(&source, &mutations, adapter)?;
        rewritten.push(SchemaFileRewrite {
            file,
            content: rewritten_source.into_bytes(),
        });
    }
    Ok(rewritten)
}

fn build_expression_edits(
    source: &str,
    selected: &[&SchemaMutation],
    adapter: &dyn SchemaAdapter,
    root: tree_sitter::Node<'_>,
    range_for_mutation: ExpressionRangeResolver,
) -> Result<Vec<(std::ops::Range<usize>, String)>, SchemaRewriteError> {
    let source_bytes = source.as_bytes();
    let mut edits = Vec::with_capacity(selected.len());

    for schema_mutation in selected {
        let mutation = &schema_mutation.mutation;
        validate_source_range(mutation, source_bytes).map_err(|reason| {
            SchemaRewriteError::new(format!(
                "mutation {} is not rewriteable: {reason:?}",
                mutation.id
            ))
        })?;
        let expression_range = range_for_mutation(root, source, mutation)?;
        let original = source_slice(source, expression_range.clone())?;
        let replacement = mutated_expression(source, expression_range.clone(), mutation)?;
        let wrapped = adapter.wrap_expression(mutation.id, original, &replacement);
        edits.push((expression_range, wrapped));
    }

    edits.sort_by_key(|(range, _)| (range.start, range.end));
    ensure_non_overlapping_expression_edits(&edits)?;
    Ok(edits)
}

fn ensure_non_overlapping_expression_edits(
    edits: &[(std::ops::Range<usize>, String)],
) -> Result<(), SchemaRewriteError> {
    let mut previous_end = 0usize;
    for (position, (range, _)) in edits.iter().enumerate() {
        if position > 0 && range.start < previous_end {
            return Err(SchemaRewriteError::new(
                "schema mutations overlap after expression expansion",
            ));
        }
        previous_end = range.end;
    }
    Ok(())
}

fn apply_expression_edits(
    source: &str,
    edits: Vec<(std::ops::Range<usize>, String)>,
    language: &str,
) -> Result<String, SchemaRewriteError> {
    let mut rewritten = source.as_bytes().to_vec();
    for (range, replacement) in edits.into_iter().rev() {
        rewritten.splice(range, replacement.bytes());
    }
    String::from_utf8(rewritten).map_err(|e| {
        SchemaRewriteError::new(format!("rewritten {language} source is not utf-8: {e}"))
    })
}

fn rewrite_expression_file(
    source: &str,
    selected: &[&SchemaMutation],
    adapter: &dyn SchemaAdapter,
    parse: fn(&str) -> Result<tree_sitter::Tree, SchemaRewriteError>,
    range_for_mutation: ExpressionRangeResolver,
    language: &str,
) -> Result<String, SchemaRewriteError> {
    let tree = parse(source)?;
    let edits = build_expression_edits(
        source,
        selected,
        adapter,
        tree.root_node(),
        range_for_mutation,
    )?;
    apply_expression_edits(source, edits, language)
}

/// Rewrite C files once so selected mutations can be activated by `TOGI_MUTANT`.
pub fn rewrite_c_files(
    project_root: &Path,
    selected: &[SchemaMutation],
) -> Result<Vec<SchemaFileRewrite>, SchemaRewriteError> {
    let adapter = require_adapter("c")?;
    rewrite_expression_files_for_language(project_root, selected, "c", adapter, rewrite_c_file)
}

/// Rewrite C++ files once so selected mutations can be activated by `TOGI_MUTANT`.
pub fn rewrite_cpp_files(
    project_root: &Path,
    selected: &[SchemaMutation],
) -> Result<Vec<SchemaFileRewrite>, SchemaRewriteError> {
    let adapter = require_adapter("cpp")?;
    rewrite_expression_files_for_language(project_root, selected, "cpp", adapter, rewrite_cpp_file)
}

/// Rewrite Go files once so selected mutations can be activated by `TOGI_MUTANT`.
pub fn rewrite_go_files(
    project_root: &Path,
    selected: &[SchemaMutation],
) -> Result<Vec<SchemaFileRewrite>, SchemaRewriteError> {
    let adapter = require_adapter("go")?;
    let by_file = group_expression_mutations_by_file(project_root, selected, "go")?;

    let mut rewritten = BTreeMap::new();
    let mut helper_file_by_package = BTreeMap::<(PathBuf, String), PathBuf>::new();
    for (file, mutations) in by_file {
        let source = std::fs::read_to_string(&file).map_err(|e| {
            SchemaRewriteError::new(format!("could not read {}: {e}", file.display()))
        })?;
        let package_name = go_package_name(&source)?;
        let rewritten_source = rewrite_go_file(&source, &mutations, adapter)?;
        let dir = file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| project_root.to_path_buf());
        helper_file_by_package
            .entry((dir, package_name))
            .or_insert_with(|| file.clone());
        rewritten.insert(file, rewritten_source);
    }

    for helper_file in helper_file_by_package.values() {
        if let Some(source) = rewritten.get_mut(helper_file) {
            *source = inject_go_runtime(source, adapter)?;
        }
    }

    Ok(rewritten
        .into_iter()
        .map(|(file, content)| SchemaFileRewrite {
            file,
            content: content.into_bytes(),
        })
        .collect())
}

/// Rewrite Java files once so selected mutations can be activated by `TOGI_MUTANT`.
pub fn rewrite_java_files(
    project_root: &Path,
    selected: &[SchemaMutation],
) -> Result<Vec<SchemaFileRewrite>, SchemaRewriteError> {
    let adapter = require_adapter("java")?;
    rewrite_expression_files_for_language(
        project_root,
        selected,
        "java",
        adapter,
        rewrite_java_file,
    )
}

/// Rewrite Rust files once so selected mutations can be activated by `TOGI_MUTANT`.
pub fn rewrite_rust_files(
    project_root: &Path,
    selected: &[SchemaMutation],
) -> Result<Vec<SchemaFileRewrite>, SchemaRewriteError> {
    let adapter = require_adapter("rust")?;
    rewrite_expression_files_for_language(
        project_root,
        selected,
        "rust",
        adapter,
        |source, mutations, adapter| {
            rewrite_rust_file(source, mutations, adapter)
                .map(|rewritten| inject_rust_runtime(&rewritten, adapter))
        },
    )
}

fn source_path(project_root: &Path, mutation_file: &Path) -> PathBuf {
    if mutation_file.is_absolute() {
        mutation_file.to_path_buf()
    } else {
        project_root.join(mutation_file)
    }
}

fn rewrite_c_file(
    source: &str,
    selected: &[&SchemaMutation],
    adapter: &dyn SchemaAdapter,
) -> Result<String, SchemaRewriteError> {
    let rewritten = rewrite_expression_file(
        source,
        selected,
        adapter,
        parse_c_source,
        c_expression_range_for_mutation,
        "C",
    )?;
    inject_c_runtime(&rewritten, adapter)
}

fn rewrite_cpp_file(
    source: &str,
    selected: &[&SchemaMutation],
    adapter: &dyn SchemaAdapter,
) -> Result<String, SchemaRewriteError> {
    let rewritten = rewrite_expression_file(
        source,
        selected,
        adapter,
        parse_cpp_source,
        cpp_expression_range_for_mutation,
        "C++",
    )?;
    inject_cpp_runtime(&rewritten, adapter)
}

fn rewrite_go_file(
    source: &str,
    selected: &[&SchemaMutation],
    adapter: &dyn SchemaAdapter,
) -> Result<String, SchemaRewriteError> {
    rewrite_expression_file(
        source,
        selected,
        adapter,
        parse_go_source,
        go_expression_range_for_mutation,
        "Go",
    )
}

fn rewrite_java_file(
    source: &str,
    selected: &[&SchemaMutation],
    adapter: &dyn SchemaAdapter,
) -> Result<String, SchemaRewriteError> {
    let source_bytes = source.as_bytes();
    let tree = parse_java_source(source)?;
    let root = tree.root_node();
    let mut expression_edits = Vec::with_capacity(selected.len());
    let mut helper_bodies = BTreeMap::<usize, tree_sitter::Node<'_>>::new();

    for schema_mutation in selected {
        let mutation = &schema_mutation.mutation;
        validate_source_range(mutation, source_bytes).map_err(|reason| {
            SchemaRewriteError::new(format!(
                "mutation {} is not rewriteable: {reason:?}",
                mutation.id
            ))
        })?;
        let expression_range = java_expression_range_for_mutation(root, source, mutation)?;
        let original = source_slice(source, expression_range.clone())?;
        let replacement = mutated_expression(source, expression_range.clone(), mutation)?;
        let wrapped = adapter.wrap_expression(mutation.id, original, &replacement);
        let class_body = java_class_body_for_range(root, expression_range.clone())?;
        helper_bodies
            .entry(class_body.byte_range().start)
            .or_insert(class_body);
        expression_edits.push((expression_range, wrapped));
    }

    expression_edits.sort_by_key(|(range, _)| (range.start, range.end));
    let mut previous_end = 0usize;
    for (position, (range, _)) in expression_edits.iter().enumerate() {
        if position > 0 && range.start < previous_end {
            return Err(SchemaRewriteError::new(
                "schema mutations overlap after expression expansion",
            ));
        }
        previous_end = range.end;
    }

    let mut edits = expression_edits;
    for class_body in helper_bodies.values() {
        if java_class_body_declares_togi_active(*class_body, source_bytes) {
            continue;
        }
        let insert_at = class_body.byte_range().start + 1;
        edits.push((
            insert_at..insert_at,
            format!("\n{}\n", adapter.runtime_helper().trim_end()),
        ));
    }

    let mut rewritten = source.as_bytes().to_vec();
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    for (range, replacement) in edits.into_iter().rev() {
        rewritten.splice(range, replacement.bytes());
    }

    String::from_utf8(rewritten)
        .map_err(|e| SchemaRewriteError::new(format!("rewritten Java source is not utf-8: {e}")))
}

fn rewrite_rust_file(
    source: &str,
    selected: &[&SchemaMutation],
    adapter: &dyn SchemaAdapter,
) -> Result<String, SchemaRewriteError> {
    rewrite_expression_file(
        source,
        selected,
        adapter,
        parse_rust_source,
        rust_expression_range_for_mutation,
        "Rust",
    )
}

fn mutated_expression(
    source: &str,
    expression_range: std::ops::Range<usize>,
    mutation: &Mutation,
) -> Result<String, SchemaRewriteError> {
    if mutation.byte_range.start < expression_range.start
        || mutation.byte_range.end > expression_range.end
    {
        return Err(SchemaRewriteError::new(format!(
            "mutation {} is outside its containing expression",
            mutation.id
        )));
    }

    let mut expression = source.as_bytes()[expression_range.clone()].to_vec();
    let start = mutation.byte_range.start - expression_range.start;
    let end = mutation.byte_range.end - expression_range.start;
    expression.splice(start..end, mutation.replacement.bytes());

    String::from_utf8(expression)
        .map_err(|e| SchemaRewriteError::new(format!("mutated expression is not utf-8: {e}")))
}

fn parse_c_source(source: &str) -> Result<tree_sitter::Tree, SchemaRewriteError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .map_err(|e| SchemaRewriteError::new(format!("could not load C grammar: {e}")))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| SchemaRewriteError::new("could not parse C source"))?;
    if tree.root_node().has_error() {
        return Err(SchemaRewriteError::new("C source contains parse errors"));
    }
    Ok(tree)
}

fn parse_cpp_source(source: &str) -> Result<tree_sitter::Tree, SchemaRewriteError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .map_err(|e| SchemaRewriteError::new(format!("could not load C++ grammar: {e}")))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| SchemaRewriteError::new("could not parse C++ source"))?;
    if tree.root_node().has_error() {
        return Err(SchemaRewriteError::new("C++ source contains parse errors"));
    }
    Ok(tree)
}

fn parse_go_source(source: &str) -> Result<tree_sitter::Tree, SchemaRewriteError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .map_err(|e| SchemaRewriteError::new(format!("could not load Go grammar: {e}")))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| SchemaRewriteError::new("could not parse Go source"))?;
    if tree.root_node().has_error() {
        return Err(SchemaRewriteError::new("Go source contains parse errors"));
    }
    Ok(tree)
}

fn parse_java_source(source: &str) -> Result<tree_sitter::Tree, SchemaRewriteError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .map_err(|e| SchemaRewriteError::new(format!("could not load Java grammar: {e}")))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| SchemaRewriteError::new("could not parse Java source"))?;
    if tree.root_node().has_error() {
        return Err(SchemaRewriteError::new("Java source contains parse errors"));
    }
    Ok(tree)
}

fn parse_rust_source(source: &str) -> Result<tree_sitter::Tree, SchemaRewriteError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|e| SchemaRewriteError::new(format!("could not load Rust grammar: {e}")))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| SchemaRewriteError::new("could not parse Rust source"))?;
    if tree.root_node().has_error() {
        return Err(SchemaRewriteError::new("Rust source contains parse errors"));
    }
    Ok(tree)
}

fn c_expression_range_for_mutation(
    root: tree_sitter::Node<'_>,
    source: &str,
    mutation: &Mutation,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    if !COMMON_EXPRESSION_OPERATOR_ALLOWLIST.contains(&mutation.operator.as_str()) {
        return Err(SchemaRewriteError::new(format!(
            "unsupported C schema operator {}",
            mutation.operator
        )));
    }

    match mutation.operator.as_str() {
        "eq_to_neq" | "lt_to_lte" | "gt_to_gte" | "and_to_or" | "or_to_and" | "mul_to_div"
        | "div_to_mul" | "mod_to_mul" | "plus_to_minus" | "minus_to_plus" => {
            smallest_c_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "binary_expression"
            })
        }
        "true_to_false" | "false_to_true" | "zero_to_one" | "string_to_empty"
        | "increment_numeric" | "decrement_numeric" => {
            exact_c_node_range(root, mutation.byte_range.clone())
        }
        "remove_unary_not" | "remove_unary_neg" => {
            smallest_c_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "unary_expression"
            })
        }
        "negate_condition" => {
            source_slice(source, mutation.byte_range.clone())?;
            Ok(mutation.byte_range.clone())
        }
        _ => Err(SchemaRewriteError::new(format!(
            "accepted C schema operator has no rewrite strategy: {}",
            mutation.operator
        ))),
    }
}

fn cpp_expression_range_for_mutation(
    root: tree_sitter::Node<'_>,
    source: &str,
    mutation: &Mutation,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    if !COMMON_EXPRESSION_OPERATOR_ALLOWLIST.contains(&mutation.operator.as_str()) {
        return Err(SchemaRewriteError::new(format!(
            "unsupported C++ schema operator {}",
            mutation.operator
        )));
    }

    match mutation.operator.as_str() {
        "eq_to_neq" | "lt_to_lte" | "gt_to_gte" | "and_to_or" | "or_to_and" | "mul_to_div"
        | "div_to_mul" | "mod_to_mul" | "plus_to_minus" | "minus_to_plus" => {
            smallest_cpp_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "binary_expression"
            })
        }
        "true_to_false" | "false_to_true" | "zero_to_one" | "string_to_empty"
        | "increment_numeric" | "decrement_numeric" => {
            exact_cpp_node_range(root, mutation.byte_range.clone())
        }
        "remove_unary_not" | "remove_unary_neg" => {
            smallest_cpp_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "unary_expression"
            })
        }
        "negate_condition" => {
            source_slice(source, mutation.byte_range.clone())?;
            Ok(mutation.byte_range.clone())
        }
        _ => Err(SchemaRewriteError::new(format!(
            "accepted C++ schema operator has no rewrite strategy: {}",
            mutation.operator
        ))),
    }
}

fn go_expression_range_for_mutation(
    root: tree_sitter::Node<'_>,
    source: &str,
    mutation: &Mutation,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    match mutation.operator.as_str() {
        "eq_to_neq" | "lt_to_lte" | "gt_to_gte" | "and_to_or" | "or_to_and" => {
            smallest_go_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "binary_expression"
            })
        }
        "true_to_false" | "false_to_true" => exact_go_node_range(root, mutation.byte_range.clone()),
        "remove_unary_not" => smallest_go_node_range(root, mutation.byte_range.clone(), |node| {
            node.kind() == "unary_expression"
        }),
        "negate_condition" => {
            source_slice(source, mutation.byte_range.clone())?;
            Ok(mutation.byte_range.clone())
        }
        _ => Err(SchemaRewriteError::new(format!(
            "unsupported Go schema operator {}",
            mutation.operator
        ))),
    }
}

fn java_expression_range_for_mutation(
    root: tree_sitter::Node<'_>,
    source: &str,
    mutation: &Mutation,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    if !COMMON_EXPRESSION_OPERATOR_ALLOWLIST.contains(&mutation.operator.as_str()) {
        return Err(SchemaRewriteError::new(format!(
            "unsupported Java schema operator {}",
            mutation.operator
        )));
    }

    match mutation.operator.as_str() {
        "eq_to_neq" | "lt_to_lte" | "gt_to_gte" | "and_to_or" | "or_to_and" | "mul_to_div"
        | "div_to_mul" | "mod_to_mul" | "plus_to_minus" | "minus_to_plus" => {
            smallest_java_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "binary_expression"
            })
        }
        "true_to_false" | "false_to_true" | "zero_to_one" | "string_to_empty"
        | "increment_numeric" | "decrement_numeric" => {
            exact_java_node_range(root, mutation.byte_range.clone())
        }
        "remove_unary_not" | "remove_unary_neg" => {
            smallest_java_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "unary_expression"
            })
        }
        "negate_condition" => {
            source_slice(source, mutation.byte_range.clone())?;
            Ok(mutation.byte_range.clone())
        }
        _ => Err(SchemaRewriteError::new(format!(
            "accepted Java schema operator has no rewrite strategy: {}",
            mutation.operator
        ))),
    }
}

fn rust_expression_range_for_mutation(
    root: tree_sitter::Node<'_>,
    source: &str,
    mutation: &Mutation,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    if !COMMON_EXPRESSION_OPERATOR_ALLOWLIST.contains(&mutation.operator.as_str()) {
        return Err(SchemaRewriteError::new(format!(
            "unsupported Rust schema operator {}",
            mutation.operator
        )));
    }

    let range = match mutation.operator.as_str() {
        "eq_to_neq" | "lt_to_lte" | "gt_to_gte" | "and_to_or" | "or_to_and" | "mul_to_div"
        | "div_to_mul" | "mod_to_mul" | "plus_to_minus" | "minus_to_plus" => {
            smallest_rust_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "binary_expression"
            })?
        }
        "true_to_false" | "false_to_true" | "zero_to_one" | "string_to_empty"
        | "increment_numeric" | "decrement_numeric" => {
            exact_rust_node_range(root, mutation.byte_range.clone())?
        }
        "remove_unary_not" | "remove_unary_neg" => {
            smallest_rust_node_range(root, mutation.byte_range.clone(), |node| {
                node.kind() == "unary_expression"
            })?
        }
        "negate_condition" => {
            source_slice(source, mutation.byte_range.clone())?;
            mutation.byte_range.clone()
        }
        _ => {
            return Err(SchemaRewriteError::new(format!(
                "accepted Rust schema operator has no rewrite strategy: {}",
                mutation.operator
            )));
        }
    };

    if rust_range_conflicts_with_let_condition(root, range.clone()) {
        return Err(SchemaRewriteError::new(format!(
            "mutation {} overlaps a Rust let condition and cannot be expression-wrapped",
            mutation.id
        )));
    }
    Ok(range)
}

/// Return true when an expression range cannot be lifted into a `__togi_select`
/// closure because it overlaps a Rust `let` condition (`if let`, `while let`,
/// or a let-chain). `let` conditions are only legal directly in `if`/`while`
/// condition position, so a range that spans one — or sits inside one outside
/// its `value` expression, such as a pattern — is not valid closure body code.
fn rust_range_conflicts_with_let_condition(
    root: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
) -> bool {
    let mut conflicts = false;
    visit_nodes(root, &mut |node| {
        if conflicts || node.kind() != "let_condition" {
            return;
        }
        let node_range = node.byte_range();
        if range.end <= node_range.start || node_range.end <= range.start {
            return;
        }
        let contained_in_value = node.child_by_field_name("value").is_some_and(|value| {
            let value_range = value.byte_range();
            value_range.start <= range.start && range.end <= value_range.end
        });
        if !contained_in_value {
            conflicts = true;
        }
    });
    conflicts
}

fn smallest_c_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
    predicate: impl Fn(tree_sitter::Node<'_>) -> bool + Copy,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut best = None::<std::ops::Range<usize>>;
    visit_nodes(node, &mut |candidate| {
        let candidate_range = candidate.byte_range();
        if candidate_range.start <= range.start
            && range.end <= candidate_range.end
            && predicate(candidate)
            && best
                .as_ref()
                .is_none_or(|best| candidate_range.len() < best.len())
        {
            best = Some(candidate_range);
        }
    });
    best.ok_or_else(|| SchemaRewriteError::new("could not find containing C expression"))
}

fn smallest_cpp_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
    predicate: impl Fn(tree_sitter::Node<'_>) -> bool + Copy,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut best = None::<std::ops::Range<usize>>;
    visit_nodes(node, &mut |candidate| {
        let candidate_range = candidate.byte_range();
        if candidate_range.start <= range.start
            && range.end <= candidate_range.end
            && predicate(candidate)
            && best
                .as_ref()
                .is_none_or(|best| candidate_range.len() < best.len())
        {
            best = Some(candidate_range);
        }
    });
    best.ok_or_else(|| SchemaRewriteError::new("could not find containing C++ expression"))
}

fn smallest_go_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
    predicate: impl Fn(tree_sitter::Node<'_>) -> bool + Copy,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut best = None::<std::ops::Range<usize>>;
    visit_go_nodes(node, &mut |candidate| {
        let candidate_range = candidate.byte_range();
        if candidate_range.start <= range.start
            && range.end <= candidate_range.end
            && predicate(candidate)
            && best
                .as_ref()
                .is_none_or(|best| candidate_range.len() < best.len())
        {
            best = Some(candidate_range);
        }
    });
    best.ok_or_else(|| SchemaRewriteError::new("could not find containing Go expression"))
}

fn smallest_java_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
    predicate: impl Fn(tree_sitter::Node<'_>) -> bool + Copy,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut best = None::<std::ops::Range<usize>>;
    visit_nodes(node, &mut |candidate| {
        let candidate_range = candidate.byte_range();
        if candidate_range.start <= range.start
            && range.end <= candidate_range.end
            && predicate(candidate)
            && best
                .as_ref()
                .is_none_or(|best| candidate_range.len() < best.len())
        {
            best = Some(candidate_range);
        }
    });
    best.ok_or_else(|| SchemaRewriteError::new("could not find containing Java expression"))
}

fn smallest_rust_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
    predicate: impl Fn(tree_sitter::Node<'_>) -> bool + Copy,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut best = None::<std::ops::Range<usize>>;
    visit_nodes(node, &mut |candidate| {
        let candidate_range = candidate.byte_range();
        if candidate_range.start <= range.start
            && range.end <= candidate_range.end
            && predicate(candidate)
            && best
                .as_ref()
                .is_none_or(|best| candidate_range.len() < best.len())
        {
            best = Some(candidate_range);
        }
    });
    best.ok_or_else(|| SchemaRewriteError::new("could not find containing Rust expression"))
}

fn exact_go_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut found = None;
    visit_go_nodes(node, &mut |candidate| {
        if candidate.byte_range() == range {
            found = Some(range.clone());
        }
    });
    found.ok_or_else(|| SchemaRewriteError::new("could not find Go expression node"))
}

fn exact_c_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut found = None;
    visit_nodes(node, &mut |candidate| {
        if candidate.byte_range() == range {
            found = Some(range.clone());
        }
    });
    found.ok_or_else(|| SchemaRewriteError::new("could not find C expression node"))
}

fn exact_cpp_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut found = None;
    visit_nodes(node, &mut |candidate| {
        if candidate.byte_range() == range {
            found = Some(range.clone());
        }
    });
    found.ok_or_else(|| SchemaRewriteError::new("could not find C++ expression node"))
}

fn exact_java_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut found = None;
    visit_nodes(node, &mut |candidate| {
        if candidate.byte_range() == range {
            found = Some(range.clone());
        }
    });
    found.ok_or_else(|| SchemaRewriteError::new("could not find Java expression node"))
}

fn exact_rust_node_range(
    node: tree_sitter::Node<'_>,
    range: std::ops::Range<usize>,
) -> Result<std::ops::Range<usize>, SchemaRewriteError> {
    let mut found = None;
    visit_nodes(node, &mut |candidate| {
        if candidate.byte_range() == range {
            found = Some(range.clone());
        }
    });
    found.ok_or_else(|| SchemaRewriteError::new("could not find Rust expression node"))
}

fn java_class_body_for_range<'tree>(
    node: tree_sitter::Node<'tree>,
    range: std::ops::Range<usize>,
) -> Result<tree_sitter::Node<'tree>, SchemaRewriteError> {
    let mut best = None::<tree_sitter::Node<'tree>>;
    visit_nodes(node, &mut |candidate| {
        let candidate_range = candidate.byte_range();
        let parent_kind = candidate.parent().map(|parent| parent.kind());
        if candidate.kind() == "class_body"
            && parent_kind == Some("class_declaration")
            && candidate_range.start <= range.start
            && range.end <= candidate_range.end
            && best
                .as_ref()
                .is_none_or(|best| candidate_range.len() < best.byte_range().len())
        {
            best = Some(candidate);
        }
    });
    best.ok_or_else(|| {
        SchemaRewriteError::new("could not find containing Java class body for schema helper")
    })
}

fn java_class_body_declares_togi_active(class_body: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut cursor = class_body.walk();
    class_body.children(&mut cursor).any(|child| {
        child.kind() == "method_declaration"
            && java_method_declaration_name_is(child, source, "__togi_active")
    })
}

fn java_method_declaration_name_is(
    method: tree_sitter::Node<'_>,
    source: &[u8],
    expected: &str,
) -> bool {
    if method
        .child_by_field_name("name")
        .is_some_and(|name| node_text_eq(name, source, expected))
    {
        return true;
    }

    let mut cursor = method.walk();
    method
        .children(&mut cursor)
        .any(|child| child.kind() == "identifier" && node_text_eq(child, source, expected))
}

fn node_text_eq(node: tree_sitter::Node<'_>, source: &[u8], expected: &str) -> bool {
    source
        .get(node.byte_range())
        .is_some_and(|bytes| bytes == expected.as_bytes())
}

fn c_range_is_runtime_context(source: &[u8], range: std::ops::Range<usize>) -> bool {
    let Ok(source_text) = std::str::from_utf8(source) else {
        return false;
    };
    let Ok(tree) = parse_c_source(source_text) else {
        return false;
    };
    let Some(mut current) = smallest_containing_node(tree.root_node(), range) else {
        return false;
    };

    let mut inside_function_body = false;
    loop {
        let kind = current.kind();
        if kind.starts_with("preproc") || kind == "case_statement" || kind == "enumerator" {
            return false;
        }
        if kind == "declaration" && c_declaration_has_static_storage(current, source) {
            return false;
        }
        if kind == "compound_statement"
            && current
                .parent()
                .is_some_and(|parent| parent.kind() == "function_definition")
        {
            inside_function_body = true;
        }

        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }

    inside_function_body
}

fn cpp_range_is_runtime_context(source: &[u8], range: std::ops::Range<usize>) -> bool {
    let Ok(source_text) = std::str::from_utf8(source) else {
        return false;
    };
    let Ok(tree) = parse_cpp_source(source_text) else {
        return false;
    };
    let Some(mut current) = smallest_containing_node(tree.root_node(), range) else {
        return false;
    };

    let mut inside_function_body = false;
    loop {
        let kind = current.kind();
        if kind.starts_with("preproc") || kind == "case_statement" || kind == "enumerator" {
            return false;
        }
        if kind.contains("static_assert") {
            return false;
        }
        if kind == "declaration" && c_declaration_has_static_storage(current, source) {
            return false;
        }
        if kind == "function_definition" && cpp_function_is_constant_evaluated(current, source) {
            return false;
        }
        if kind == "compound_statement"
            && current
                .parent()
                .is_some_and(|parent| parent.kind() == "function_definition")
        {
            inside_function_body = true;
        }

        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }

    inside_function_body
}

fn smallest_containing_node<'tree>(
    node: tree_sitter::Node<'tree>,
    range: std::ops::Range<usize>,
) -> Option<tree_sitter::Node<'tree>> {
    let mut best = None::<tree_sitter::Node<'tree>>;
    visit_nodes(node, &mut |candidate| {
        let candidate_range = candidate.byte_range();
        if candidate_range.start <= range.start
            && range.end <= candidate_range.end
            && best
                .as_ref()
                .is_none_or(|best| candidate_range.len() < best.byte_range().len())
        {
            best = Some(candidate);
        }
    });
    best
}

fn c_declaration_has_static_storage(declaration: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    source
        .get(declaration.byte_range())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .is_some_and(|text| text.trim_start().starts_with("static "))
}

fn visit_go_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    visit: &mut impl FnMut(tree_sitter::Node<'tree>),
) {
    visit_nodes(node, visit);
}

fn visit_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    visit: &mut impl FnMut(tree_sitter::Node<'tree>),
) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_nodes(child, visit);
    }
}

fn source_slice(source: &str, range: std::ops::Range<usize>) -> Result<&str, SchemaRewriteError> {
    std::str::from_utf8(&source.as_bytes()[range])
        .map_err(|e| SchemaRewriteError::new(format!("source slice is not utf-8: {e}")))
}

fn inject_c_runtime(
    source: &str,
    adapter: &dyn SchemaAdapter,
) -> Result<String, SchemaRewriteError> {
    let tree = parse_c_source(source)?;
    let root = tree.root_node();
    if c_source_declares_togi_active(root, source.as_bytes()) {
        return Ok(source.to_string());
    }
    let insert_at = c_runtime_insert_offset(root)?;
    let helper = adapter.runtime_helper().trim_end();
    let mut insertion = String::new();
    if insert_at > 0 && !source[..insert_at].ends_with('\n') {
        insertion.push('\n');
    }
    insertion.push_str(helper);
    insertion.push_str("\n\n");

    let mut rewritten = source.to_string();
    rewritten.insert_str(insert_at, &insertion);
    Ok(rewritten)
}

fn inject_cpp_runtime(
    source: &str,
    adapter: &dyn SchemaAdapter,
) -> Result<String, SchemaRewriteError> {
    let tree = parse_cpp_source(source)?;
    if cpp_source_declares_togi_active(tree.root_node(), source.as_bytes()) {
        return Ok(source.to_string());
    }

    let source = ensure_cpp_imports(source.to_string(), adapter.required_imports());
    let tree = parse_cpp_source(&source)?;
    let insert_at = cpp_runtime_insert_offset(tree.root_node());
    let helper = adapter.runtime_helper().trim_end();
    let mut insertion = String::new();
    if insert_at > 0 && !source[..insert_at].ends_with('\n') {
        insertion.push('\n');
    }
    insertion.push_str(helper);
    insertion.push_str("\n\n");

    let mut rewritten = source;
    rewritten.insert_str(insert_at, &insertion);
    Ok(rewritten)
}

fn ensure_cpp_imports(mut source: String, imports: &[&str]) -> String {
    for import in imports {
        if cpp_source_includes(&source, import) {
            continue;
        }
        let offset = cpp_include_insert_offset(&source);
        let mut include = String::new();
        if offset > 0 && !source[..offset].ends_with('\n') {
            include.push('\n');
        }
        include.push_str(&format!("#include <{import}>\n"));
        source.insert_str(offset, &include);
    }
    source
}

fn cpp_source_includes(source: &str, import: &str) -> bool {
    let needle = format!("<{import}>");
    source
        .lines()
        .map(str::trim_start)
        .any(|line| line.starts_with("#include") && line.contains(&needle))
}

fn cpp_include_insert_offset(source: &str) -> usize {
    let mut offset = 0usize;
    let mut insertion = 0usize;
    let mut in_block_comment = false;

    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if in_block_comment {
            offset += line.len();
            insertion = offset;
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("//") {
            offset += line.len();
            insertion = offset;
            continue;
        }
        if trimmed.starts_with("/*") {
            offset += line.len();
            insertion = offset;
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            continue;
        }
        if trimmed.starts_with("#include") || trimmed.starts_with("#pragma once") {
            offset += line.len();
            insertion = offset;
            continue;
        }
        break;
    }

    insertion
}

fn cpp_runtime_insert_offset(root: tree_sitter::Node<'_>) -> usize {
    let mut cursor = root.walk();
    root.children(&mut cursor)
        .find(|child| {
            !matches!(child.kind(), "comment" | "preproc_include" | "preproc_def")
                && !child.kind().starts_with("preproc")
        })
        .map(|child| child.byte_range().start)
        .unwrap_or(root.byte_range().end)
}

fn cpp_source_declares_togi_active(root: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut found = false;
    visit_nodes(root, &mut |candidate| {
        if candidate.kind() == "function_definition"
            && !cpp_function_is_member_definition(candidate)
            && c_function_definition_name_is(candidate, source, "__togi_active")
        {
            found = true;
        }
    });
    found
}

fn cpp_function_is_member_definition(function: tree_sitter::Node<'_>) -> bool {
    let mut current = function;
    while let Some(parent) = current.parent() {
        if matches!(
            parent.kind(),
            "class_specifier" | "struct_specifier" | "union_specifier" | "namespace_definition"
        ) {
            return true;
        }
        current = parent;
    }
    function
        .child_by_field_name("declarator")
        .is_some_and(cpp_declarator_is_qualified)
}

fn cpp_declarator_is_qualified(declarator: tree_sitter::Node<'_>) -> bool {
    if declarator.kind() == "qualified_identifier" {
        return true;
    }
    let mut cursor = declarator.walk();
    declarator
        .children(&mut cursor)
        .any(cpp_declarator_is_qualified)
}

fn cpp_function_is_constant_evaluated(function: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let prefix_end = function
        .child_by_field_name("declarator")
        .map(|declarator| declarator.byte_range().start)
        .unwrap_or_else(|| function.byte_range().end);
    source
        .get(function.byte_range().start..prefix_end)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .is_some_and(|prefix| {
            contains_ascii_keyword(prefix, "constexpr")
                || contains_ascii_keyword(prefix, "consteval")
        })
}

fn contains_ascii_keyword(text: &str, keyword: &str) -> bool {
    text.match_indices(keyword).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + keyword.len()..].chars().next();
        before.is_none_or(|character| !is_ascii_identifier_character(character))
            && after.is_none_or(|character| !is_ascii_identifier_character(character))
    })
}

fn is_ascii_identifier_character(character: char) -> bool {
    character == '_' || character.is_ascii_alphanumeric()
}

fn c_runtime_insert_offset(root: tree_sitter::Node<'_>) -> Result<usize, SchemaRewriteError> {
    let mut cursor = root.walk();
    root.children(&mut cursor)
        .find(|child| child.kind() == "function_definition")
        .map(|child| child.byte_range().start)
        .ok_or_else(|| SchemaRewriteError::new("could not find C function for schema helper"))
}

fn c_source_declares_togi_active(root: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut found = false;
    visit_nodes(root, &mut |candidate| {
        if candidate.kind() == "function_definition"
            && c_function_definition_name_is(candidate, source, "__togi_active")
        {
            found = true;
        }
    });
    found
}

fn c_function_definition_name_is(
    function: tree_sitter::Node<'_>,
    source: &[u8],
    expected: &str,
) -> bool {
    function
        .child_by_field_name("declarator")
        .is_some_and(|declarator| c_declarator_name_is(declarator, source, expected))
}

fn c_declarator_name_is(declarator: tree_sitter::Node<'_>, source: &[u8], expected: &str) -> bool {
    if declarator.kind() == "identifier" {
        return node_text_eq(declarator, source, expected);
    }
    if let Some(child) = declarator.child_by_field_name("declarator") {
        return c_declarator_name_is(child, source, expected);
    }

    let mut cursor = declarator.walk();
    declarator.children(&mut cursor).any(|child| {
        child.kind() != "parameter_list" && c_declarator_name_is(child, source, expected)
    })
}

fn inject_go_runtime(
    source: &str,
    adapter: &dyn SchemaAdapter,
) -> Result<String, SchemaRewriteError> {
    if source.contains("func __togi_active(") {
        return Ok(source.to_string());
    }
    let source = ensure_go_imports(source.to_string(), adapter.required_imports())?;
    let insert_at = go_runtime_insert_offset(&source)?;
    let helper = adapter.runtime_helper().trim();
    let mut rewritten = source;
    rewritten.insert_str(insert_at, &format!("\n{helper}\n"));
    Ok(rewritten)
}

fn inject_rust_runtime(source: &str, adapter: &dyn SchemaAdapter) -> String {
    if source.contains("fn __togi_active(") {
        return source.to_string();
    }
    let insert_at = rust_runtime_insert_offset(source);
    let helper = adapter.runtime_helper().trim();
    let mut rewritten = source.to_string();
    rewritten.insert_str(insert_at, &format!("{helper}\n\n"));
    rewritten
}

fn rust_runtime_insert_offset(source: &str) -> usize {
    let mut offset = 0usize;
    let mut lines = source.split_inclusive('\n');

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            offset += line.len();
        } else if trimmed.starts_with("#![") {
            let mut balance = square_bracket_balance(line);
            offset += line.len();
            while balance > 0 {
                let Some(line) = lines.next() else {
                    break;
                };
                balance += square_bracket_balance(line);
                offset += line.len();
            }
        } else {
            break;
        }
    }
    offset
}

fn square_bracket_balance(line: &str) -> i32 {
    line.chars().fold(0, |balance, character| match character {
        '[' => balance + 1,
        ']' => balance - 1,
        _ => balance,
    })
}

#[derive(Debug, Clone, Copy)]
enum GoImportDeclaration {
    Block {
        import_start: usize,
        open_end: usize,
        close_start: usize,
        end: usize,
    },
    Single {
        path_start: usize,
        end: usize,
    },
}

impl GoImportDeclaration {
    fn end(self) -> usize {
        match self {
            Self::Block { end, .. } | Self::Single { end, .. } => end,
        }
    }

    fn import_text(self, source: &str) -> &str {
        match self {
            Self::Block {
                open_end,
                close_start,
                ..
            } => &source[open_end..close_start],
            Self::Single { path_start, end } => &source[path_start..end],
        }
    }
}

#[derive(Debug)]
struct GoImportRegion {
    declarations: Vec<GoImportDeclaration>,
    imports: BTreeSet<String>,
    new_import_block_offset: usize,
    runtime_insert_offset: usize,
}

fn ensure_go_imports(mut source: String, imports: &[&str]) -> Result<String, SchemaRewriteError> {
    let missing: Vec<&str> = imports
        .iter()
        .copied()
        .filter(|import| !go_source_imports(&source, import))
        .collect();
    if missing.is_empty() {
        return Ok(source);
    }

    let region = go_import_region(&source)?;
    if let Some(block) = region
        .declarations
        .iter()
        .rev()
        .copied()
        .find(|declaration| matches!(declaration, GoImportDeclaration::Block { .. }))
    {
        insert_missing_go_imports_into_block(&mut source, block, &missing);
    } else {
        let import_block =
            go_import_block_for_insert(&source, region.new_import_block_offset, &missing);
        source.insert_str(region.new_import_block_offset, &import_block);
    }
    Ok(source)
}

fn go_source_imports(source: &str, import: &str) -> bool {
    match go_import_region(source) {
        Ok(region) => region.imports.contains(import),
        Err(_) => false,
    }
}

fn go_runtime_insert_offset(source: &str) -> Result<usize, SchemaRewriteError> {
    Ok(go_import_region(source)?.runtime_insert_offset)
}

fn insert_missing_go_imports_into_block(
    source: &mut String,
    declaration: GoImportDeclaration,
    missing: &[&str],
) {
    let GoImportDeclaration::Block {
        import_start,
        open_end,
        close_start,
        end,
    } = declaration
    else {
        return;
    };

    let body = &source[open_end..close_start];
    if body.contains('\n') {
        let insertion_at = go_import_block_append_offset(source, close_start);
        let mut insertion = String::new();
        if insertion_at == open_end
            || !source
                .as_bytes()
                .get(insertion_at.saturating_sub(1))
                .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
        {
            insertion.push('\n');
        }
        for import in missing {
            insertion.push_str(&format!("    \"{import}\"\n"));
        }
        source.insert_str(insertion_at, &insertion);
    } else {
        let existing_imports = body.trim();
        let mut replacement = String::from("import (\n");
        if !existing_imports.is_empty() {
            replacement.push_str("    ");
            replacement.push_str(existing_imports);
            replacement.push('\n');
        }
        for import in missing {
            replacement.push_str(&format!("    \"{import}\"\n"));
        }
        replacement.push(')');
        source.replace_range(import_start..end, &replacement);
    }
}

fn go_import_block_append_offset(source: &str, close_start: usize) -> usize {
    let mut offset = close_start;
    while offset > 0
        && source
            .as_bytes()
            .get(offset - 1)
            .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
    {
        offset -= 1;
    }
    offset
}

fn go_import_block_for_insert(source: &str, offset: usize, imports: &[&str]) -> String {
    let mut block = String::new();
    if offset > 0
        && !source
            .as_bytes()
            .get(offset - 1)
            .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        block.push('\n');
    }
    block.push_str("import (\n");
    for import in imports {
        block.push_str(&format!("    \"{import}\"\n"));
    }
    block.push_str(")\n");
    block
}

fn go_import_region(source: &str) -> Result<GoImportRegion, SchemaRewriteError> {
    let package_end = go_package_decl_end(source)?;
    let mut declarations = Vec::new();
    let mut imports = BTreeSet::new();
    let mut offset = package_end;
    let mut first_declaration_offset = None;
    let mut runtime_insert_offset = package_end;

    loop {
        let next = skip_go_import_trivia(source, offset);
        if first_declaration_offset.is_none() {
            first_declaration_offset = Some(next);
        }
        let Some(declaration) = go_import_declaration(source, next) else {
            break;
        };

        collect_go_imports(declaration.import_text(source), &mut imports);
        runtime_insert_offset = go_import_declaration_end_with_line(source, declaration);
        offset = runtime_insert_offset;
        declarations.push(declaration);
    }

    let new_import_block_offset = if declarations.is_empty() {
        first_declaration_offset.unwrap_or(package_end)
    } else {
        runtime_insert_offset
    };

    Ok(GoImportRegion {
        declarations,
        imports,
        new_import_block_offset,
        runtime_insert_offset,
    })
}

fn go_import_declaration(source: &str, import_start: usize) -> Option<GoImportDeclaration> {
    let rest = source.get(import_start..)?;
    if !rest.starts_with("import") {
        return None;
    }

    let keyword_end = import_start + "import".len();
    match source.as_bytes().get(keyword_end) {
        Some(byte) if byte.is_ascii_whitespace() || *byte == b'(' => {}
        _ => return None,
    }

    let spec_start = skip_ascii_whitespace(source, keyword_end);
    if source.as_bytes().get(spec_start) == Some(&b'(') {
        let open_end = spec_start + 1;
        let close_start = source[open_end..].find(')')? + open_end;
        return Some(GoImportDeclaration::Block {
            import_start,
            open_end,
            close_start,
            end: close_start + 1,
        });
    }

    source.as_bytes().get(spec_start)?;
    let end = source[spec_start..]
        .find('\n')
        .map(|idx| spec_start + idx)
        .unwrap_or(source.len());
    Some(GoImportDeclaration::Single {
        path_start: spec_start,
        end,
    })
}

fn go_import_declaration_end_with_line(source: &str, declaration: GoImportDeclaration) -> usize {
    let mut offset = declaration.end();
    if source.as_bytes().get(offset) == Some(&b'\r') {
        offset += 1;
    }
    if source.as_bytes().get(offset) == Some(&b'\n') {
        offset += 1;
    }
    offset
}

fn collect_go_imports(source: &str, imports: &mut BTreeSet<String>) {
    let bytes = source.as_bytes();
    let mut offset = 0usize;
    while let Some(relative_start) = bytes[offset..]
        .iter()
        .position(|byte| *byte == b'"' || *byte == b'`')
    {
        let start = offset + relative_start;
        let quote = bytes[start];
        let Some(relative_end) = bytes[start + 1..].iter().position(|byte| *byte == quote) else {
            break;
        };
        let end = start + 1 + relative_end;
        imports.insert(source[start + 1..end].to_string());
        offset = end + 1;
    }
}

fn skip_go_import_trivia(source: &str, mut offset: usize) -> usize {
    loop {
        offset = skip_ascii_whitespace(source, offset);
        let Some(rest) = source.get(offset..) else {
            return offset;
        };
        if rest.starts_with("//") {
            offset = rest
                .find('\n')
                .map(|idx| offset + idx + 1)
                .unwrap_or(source.len());
            continue;
        }
        if rest.starts_with("/*") {
            offset = rest
                .find("*/")
                .map(|idx| offset + idx + 2)
                .unwrap_or(source.len());
            continue;
        }
        return offset;
    }
}

fn go_package_decl_end(source: &str) -> Result<usize, SchemaRewriteError> {
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        if line.trim_start().starts_with("package ") {
            return Ok(offset + line.len());
        }
        offset += line.len();
    }
    if source.trim_start().starts_with("package ") {
        return Ok(source.len());
    }
    Err(SchemaRewriteError::new(
        "could not find Go package declaration",
    ))
}

fn go_package_name(source: &str) -> Result<String, SchemaRewriteError> {
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("package") else {
            continue;
        };
        if !rest.as_bytes().first().is_some_and(u8::is_ascii_whitespace) {
            continue;
        }
        let name = rest
            .trim_start()
            .split(|character: char| character != '_' && !character.is_ascii_alphanumeric())
            .next()
            .unwrap_or_default();
        if !name.is_empty() {
            return Ok(name.to_string());
        }
    }
    Err(SchemaRewriteError::new(
        "could not find Go package declaration",
    ))
}

fn skip_ascii_whitespace(source: &str, mut offset: usize) -> usize {
    while source
        .as_bytes()
        .get(offset)
        .is_some_and(u8::is_ascii_whitespace)
    {
        offset += 1;
    }
    offset
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

fn schema_conflict_analysis_with_sources(
    project_root: &Path,
    selected: &[SchemaMutation],
    source_cache: &HashMap<PathBuf, Option<Vec<u8>>>,
) -> (BTreeMap<usize, ()>, BTreeMap<usize, SchemaSkipReason>) {
    let mut by_file: BTreeMap<String, (PathBuf, Vec<usize>)> = BTreeMap::new();
    for (idx, schema_mutation) in selected.iter().enumerate() {
        let path = source_path(project_root, &schema_mutation.mutation.file);
        by_file
            .entry(normalized_path_key(&path))
            .or_insert_with(|| (path, Vec::new()))
            .1
            .push(idx);
    }

    let mut overlapping = BTreeMap::new();
    let mut failures = BTreeMap::new();
    for (_, (path, indices)) in by_file {
        let source_bytes = source_cache
            .get(&path)
            .and_then(|cached_source| cached_source.as_deref());
        let source_problem = match source_bytes {
            Some(bytes) if std::str::from_utf8(bytes).is_err() => {
                Some(SchemaSkipReason::InvalidRange)
            }
            Some(_) => None,
            None => Some(SchemaSkipReason::MissingSource),
        };
        let source_text = source_bytes.and_then(|bytes| std::str::from_utf8(bytes).ok());
        let needs_rust_tree = indices
            .iter()
            .any(|idx| schema_conflict_range_uses_rust_expression(&selected[*idx]));
        let (rust_tree, rust_problem) = if needs_rust_tree {
            match source_text {
                Some(source) => match parse_rust_source(source) {
                    Ok(tree) => (Some(tree), None),
                    Err(_) => (None, Some(SchemaSkipReason::InvalidRange)),
                },
                None => (None, source_problem),
            }
        } else {
            (None, None)
        };

        let mut ranges = Vec::new();
        for idx in indices {
            match schema_conflict_range(
                &selected[idx],
                source_text,
                rust_tree.as_ref(),
                rust_problem,
            ) {
                Ok(range) => ranges.push((idx, range)),
                Err(reason) => {
                    failures.insert(idx, reason);
                }
            }
        }

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
    (overlapping, failures)
}

fn schema_conflict_range(
    schema_mutation: &SchemaMutation,
    source: Option<&str>,
    rust_tree: Option<&tree_sitter::Tree>,
    rust_problem: Option<SchemaSkipReason>,
) -> Result<std::ops::Range<usize>, SchemaSkipReason> {
    if schema_conflict_range_uses_rust_expression(schema_mutation) {
        if let Some(reason) = rust_problem {
            return Err(reason);
        }
        let source = source.ok_or(SchemaSkipReason::MissingSource)?;
        let tree = rust_tree.ok_or(SchemaSkipReason::InvalidRange)?;
        if rust_range_conflicts_with_let_condition(
            tree.root_node(),
            schema_mutation.mutation.byte_range.clone(),
        ) {
            return Err(SchemaSkipReason::UnsupportedSyntaxContext);
        }
        return rust_expression_range_for_mutation(
            tree.root_node(),
            source,
            &schema_mutation.mutation,
        )
        .map_err(|_| SchemaSkipReason::InvalidRange);
    }

    Ok(schema_mutation.mutation.byte_range.clone())
}

fn schema_conflict_range_uses_rust_expression(schema_mutation: &SchemaMutation) -> bool {
    schema_mutation.mutation.language == "rust" && schema_mutation.kind == SchemaKind::Expression
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

fn java_line_looks_compile_time(mutation: &Mutation, source: &[u8]) -> bool {
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
    let context_start = source[line_start..mutation.byte_range.start]
        .iter()
        .rposition(|byte| *byte == b'{')
        .map(|idx| line_start + idx + 1)
        .unwrap_or(line_start);
    let Ok(line) = std::str::from_utf8(&source[context_start..line_end]) else {
        return false;
    };

    let mut rest = line.trim_start();
    let mut saw_static = false;
    let mut saw_final = false;
    while let Some((token, after_token)) = split_java_token(rest) {
        match token {
            "public" | "protected" | "private" | "abstract" | "synchronized" | "native"
            | "strictfp" | "transient" | "volatile" => rest = after_token.trim_start(),
            "static" => {
                saw_static = true;
                rest = after_token.trim_start();
            }
            "final" => {
                saw_final = true;
                rest = after_token.trim_start();
            }
            _ => break,
        }
    }
    saw_static && saw_final
}

fn split_java_token(value: &str) -> Option<(&str, &str)> {
    let value = value.trim_start();
    if value.is_empty() {
        return None;
    }
    let end = value
        .char_indices()
        .find_map(|(index, character)| {
            (!character.is_ascii_alphanumeric() && character != '_').then_some(index)
        })
        .unwrap_or(value.len());
    (end > 0).then(|| value.split_at(end))
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

        let c = adapter_for_language("c").unwrap();
        write_source(&dir, "calc.c", "int f(int a, int b) { return a == b; }\n");
        assert_parsed_node_text(&dir, "calc.c", c, "binary_expression", "a == b");
        assert_eq!(
            c.wrap_expression(42, "a == b", "a != b"),
            "(__togi_active(42u) ? (a != b) : (a == b))"
        );

        let cpp = adapter_for_language("cpp").unwrap();
        write_source(
            &dir,
            "calc.cpp",
            "bool f(int a, int b) { return a == b; }\n",
        );
        assert_parsed_node_text(&dir, "calc.cpp", cpp, "binary_expression", "a == b");
        assert_eq!(
            cpp.wrap_expression(42, "a == b", "a != b"),
            "(::__togi_active(42u) ? (a != b) : (a == b))"
        );
        assert_eq!(cpp.required_imports(), &["cstdlib"]);

        let go = adapter_for_language("go").unwrap();
        write_source(
            &dir,
            "calc.go",
            "package calc\nfunc f(a, b int) bool { return a == b }\n",
        );
        assert_parsed_node_text(&dir, "calc.go", go, "binary_expression", "a == b");
        assert_eq!(
            go.wrap_expression(42, "a == b", "a != b"),
            "func() bool { if __togi_active(\"42\") { return a != b }; return a == b }()"
        );
        assert_eq!(go.required_imports(), &["os"]);

        let java = adapter_for_language("java").unwrap();
        write_source(
            &dir,
            "Calc.java",
            "class Calc { boolean f(int a, int b) { return a == b; } }\n",
        );
        assert_parsed_node_text(&dir, "Calc.java", java, "binary_expression", "a == b");
        assert_eq!(
            java.wrap_expression(42, "a == b", "a != b"),
            "(__togi_active(42) ? (a != b) : (a == b))"
        );

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
    fn plan_selects_c_expression_mutations_inside_function_body() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("c").unwrap();
        write_source(&dir, "calc.c", "int f(int a, int b) { return a == b; }\n");
        assert_parsed_node_text(&dir, "calc.c", adapter, "binary_expression", "a == b");
        let start = "int f(int a, int b) { return ".len();
        let mutation = mutation(
            "c",
            "calc.c",
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
    fn plan_selects_cpp_expression_mutations_inside_function_body() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("cpp").unwrap();
        write_source(
            &dir,
            "calc.cpp",
            "bool f(int a, int b) { return a == b; }\n",
        );
        assert_parsed_node_text(&dir, "calc.cpp", adapter, "binary_expression", "a == b");
        let start = "bool f(int a, int b) { return ".len();
        let mutation = mutation(
            "cpp",
            "calc.cpp",
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
    fn plan_falls_back_for_c_global_context() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("c").unwrap();
        let source = "static const int VERSION = 2;\nint f(void) { return VERSION; }\n";
        write_source(&dir, "calc.c", source);
        assert_parsed_node_kind(&dir, "calc.c", adapter, "declaration");
        assert_parsed_node_text(&dir, "calc.c", adapter, "number_literal", "2");
        let start = source.find('2').unwrap();
        let mutation = mutation(
            "c",
            "calc.c",
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
    fn plan_falls_back_for_cpp_global_context() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("cpp").unwrap();
        let source = "static constexpr int VERSION = 2;\nbool f() { return VERSION > 0; }\n";
        write_source(&dir, "calc.cpp", source);
        assert_parsed_node_kind(&dir, "calc.cpp", adapter, "declaration");
        assert_parsed_node_text(&dir, "calc.cpp", adapter, "number_literal", "2");
        let start = source.find('2').unwrap();
        let mutation = mutation(
            "cpp",
            "calc.cpp",
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
    fn plan_falls_back_for_cpp_constant_evaluated_contexts() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("cpp").unwrap();
        for (file, source, original, replacement) in [
            (
                "constexpr.cpp",
                "constexpr bool f(int a, int b) { return a == b; }\n",
                "a == b",
                "a != b",
            ),
            (
                "consteval.cpp",
                "consteval bool f(int a, int b) { return a == b; }\n",
                "a == b",
                "a != b",
            ),
            (
                "static_assert.cpp",
                "bool f() { static_assert(1 == 1, \"ok\"); return true; }\n",
                "1 == 1",
                "1 != 1",
            ),
        ] {
            write_source(&dir, file, source);
            assert_parsed_node_text(&dir, file, adapter, "binary_expression", original);
            let start = source.find(original).unwrap();
            let mutation = mutation(
                "cpp",
                file,
                original,
                replacement,
                start..start + original.len(),
                "eq_to_neq",
            );

            let plan = plan(dir.path(), vec![mutation]);

            assert!(plan.selected.is_empty(), "{file}");
            assert_eq!(plan.fallback.len(), 1, "{file}");
            assert_eq!(
                plan.fallback[0].reason,
                SchemaSkipReason::CompileTimeContext,
                "{file}"
            );
        }
    }

    #[test]
    fn plan_falls_back_for_java_static_final_context() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("java").unwrap();
        let source = "class Calc { private static final int VERSION = 2; }\n";
        write_source(&dir, "Calc.java", source);
        assert_parsed_node_kind(&dir, "Calc.java", adapter, "field_declaration");
        assert_parsed_node_text(&dir, "Calc.java", adapter, "decimal_integer_literal", "2");
        let start = source.find('2').unwrap();
        let mutation = mutation(
            "java",
            "Calc.java",
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

    #[test]
    fn plan_detects_rust_overlaps_after_expression_expansion() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("rust").unwrap();
        let source = "fn f(a: i32, b: i32, c: i32, d: i32) -> bool { a == b && c == d }\n";
        write_source(&dir, "src/lib.rs", source);
        assert_parsed_node_text(&dir, "src/lib.rs", adapter, "binary_expression", "a == b");
        assert_parsed_node_text(
            &dir,
            "src/lib.rs",
            adapter,
            "binary_expression",
            "a == b && c == d",
        );
        let equality = source.find("==").unwrap();
        let conjunction = source.find("&&").unwrap();
        let mut first = mutation(
            "rust",
            "src/lib.rs",
            "==",
            "!=",
            equality..equality + 2,
            "eq_to_neq",
        );
        first.id = 1;
        let mut second = mutation(
            "rust",
            "src/lib.rs",
            "&&",
            "||",
            conjunction..conjunction + 2,
            "and_to_or",
        );
        second.id = 2;
        assert!(first.byte_range.end <= second.byte_range.start);

        let plan = plan(dir.path(), vec![first, second]);

        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.selected[0].mutation.id, 1);
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(plan.fallback[0].mutation.id, 2);
        assert_eq!(plan.fallback[0].reason, SchemaSkipReason::OverlappingRange);
        let rewrites = rewrite_rust_files(dir.path(), &plan.selected).unwrap();
        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.contains("__togi_select(1,"));
        parse_rust_source(&rewritten).unwrap();
    }

    #[test]
    fn plan_keeps_independent_rust_mutations_when_expanded_neighbor_overlaps() {
        let dir = TempDir::new().unwrap();
        let adapter = adapter_for_language("rust").unwrap();
        let source = "fn f(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> bool { (a == b && c == d) || e == f }\n";
        write_source(&dir, "src/lib.rs", source);
        assert_parsed_node_text(
            &dir,
            "src/lib.rs",
            adapter,
            "binary_expression",
            "a == b && c == d",
        );
        assert_parsed_node_text(&dir, "src/lib.rs", adapter, "binary_expression", "e == f");
        let first_equality = source.find("==").unwrap();
        let conjunction = source.find("&&").unwrap();
        let independent_equality = source.rfind("==").unwrap();
        let mut first = mutation(
            "rust",
            "src/lib.rs",
            "==",
            "!=",
            first_equality..first_equality + 2,
            "eq_to_neq",
        );
        first.id = 1;
        let mut overlapping = mutation(
            "rust",
            "src/lib.rs",
            "&&",
            "||",
            conjunction..conjunction + 2,
            "and_to_or",
        );
        overlapping.id = 2;
        let mut independent = mutation(
            "rust",
            "src/lib.rs",
            "==",
            "!=",
            independent_equality..independent_equality + 2,
            "eq_to_neq",
        );
        independent.id = 3;

        let plan = plan(dir.path(), vec![first, overlapping, independent]);
        let selected_ids = plan
            .selected
            .iter()
            .map(|schema_mutation| schema_mutation.mutation.id)
            .collect::<Vec<_>>();

        assert_eq!(selected_ids, vec![1, 3]);
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(plan.fallback[0].mutation.id, 2);
        assert_eq!(plan.fallback[0].reason, SchemaSkipReason::OverlappingRange);
    }

    #[test]
    fn rewrite_c_files_expands_operator_mutation_to_expression_wrapper() {
        let dir = TempDir::new().unwrap();
        let source = "#include <stdio.h>\nint f(int a, int b) { return a == b; }\n";
        write_source(&dir, "calc.c", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "c",
            "calc.c",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_c_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        assert_eq!(rewrites.len(), 1);
        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.contains("static int __togi_active(unsigned int id)"));
        assert!(rewritten.contains("return (__togi_active(7u) ? (a != b) : (a == b));"));
        assert!(
            rewritten.find("static int __togi_active").unwrap() < rewritten.find("int f").unwrap(),
            "{rewritten}"
        );
        parse_c_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_c_files_ignores_togi_active_mentions_in_text() {
        let dir = TempDir::new().unwrap();
        let source = "const char *marker(void) { return \"__togi_active(\"; }\nint f(int a, int b) { return a == b; }\n";
        write_source(&dir, "calc.c", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "c",
            "calc.c",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_c_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert_eq!(
            rewritten
                .matches("static int __togi_active(unsigned int id)")
                .count(),
            1,
            "{rewritten}"
        );
        parse_c_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_c_files_reuses_existing_togi_active_function_with_spacing() {
        let dir = TempDir::new().unwrap();
        let source = "static int __togi_active (unsigned int id) { return 0; }\nint f(int a, int b) { return a == b; }\n";
        write_source(&dir, "calc.c", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "c",
            "calc.c",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_c_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert_eq!(
            rewritten.matches("static int __togi_active").count(),
            1,
            "{rewritten}"
        );
        parse_c_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_cpp_files_expands_operator_mutation_to_expression_wrapper() {
        let dir = TempDir::new().unwrap();
        let source = "#include <iostream>\nbool f(int a, int b) { return a == b; }\n";
        write_source(&dir, "calc.cpp", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "cpp",
            "calc.cpp",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_cpp_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        assert_eq!(rewrites.len(), 1);
        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.contains("#include <cstdlib>"));
        assert!(rewritten.contains("static bool __togi_active(unsigned int id)"));
        assert!(rewritten.contains("return (::__togi_active(7u) ? (a != b) : (a == b));"));
        assert!(
            rewritten.find("#include <cstdlib>").unwrap()
                < rewritten.find("static bool __togi_active").unwrap(),
            "{rewritten}"
        );
        assert!(
            rewritten.find("static bool __togi_active").unwrap()
                < rewritten.find("bool f").unwrap(),
            "{rewritten}"
        );
        parse_cpp_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_cpp_files_does_not_reuse_member_togi_active_function() {
        let dir = TempDir::new().unwrap();
        let source = "class Calc { static bool __togi_active (unsigned int id) { return false; } static bool f(int a, int b) { return a == b; } };\n";
        write_source(&dir, "calc.cpp", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "cpp",
            "calc.cpp",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_cpp_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.contains("#include <cstdlib>"), "{rewritten}");
        assert!(
            rewritten.contains("static bool __togi_active(unsigned int id)"),
            "{rewritten}"
        );
        assert!(
            rewritten.contains("return (::__togi_active(7u) ? (a != b) : (a == b));"),
            "{rewritten}"
        );
        parse_cpp_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_cpp_files_does_not_reuse_namespace_togi_active_function() {
        let dir = TempDir::new().unwrap();
        let source = "namespace hidden { static bool __togi_active (unsigned int id) { return false; } }\nbool f(int a, int b) { return a == b; }\n";
        write_source(&dir, "calc.cpp", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "cpp",
            "calc.cpp",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_cpp_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.contains("#include <cstdlib>"), "{rewritten}");
        assert!(
            rewritten.contains("static bool __togi_active(unsigned int id)"),
            "{rewritten}"
        );
        assert_eq!(
            rewritten.matches("static bool __togi_active").count(),
            2,
            "{rewritten}"
        );
        parse_cpp_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_cpp_files_ignores_togi_active_mentions_in_text() {
        let dir = TempDir::new().unwrap();
        let source = "const char *marker() { return \"__togi_active(\"; }\nbool f(int a, int b) { return a == b; }\n";
        write_source(&dir, "calc.cpp", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "cpp",
            "calc.cpp",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_cpp_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert_eq!(
            rewritten
                .matches("static bool __togi_active(unsigned int id)")
                .count(),
            1,
            "{rewritten}"
        );
        parse_cpp_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_cpp_files_reuses_existing_togi_active_function_with_spacing() {
        let dir = TempDir::new().unwrap();
        let source = "static bool __togi_active (unsigned int id) { return false; }\nbool f(int a, int b) { return a == b; }\n";
        write_source(&dir, "calc.cpp", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "cpp",
            "calc.cpp",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_cpp_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert_eq!(
            rewritten.matches("static bool __togi_active").count(),
            1,
            "{rewritten}"
        );
        assert!(!rewritten.contains("#include <cstdlib>"), "{rewritten}");
        parse_cpp_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_go_files_expands_operator_mutation_to_expression_wrapper() {
        let dir = TempDir::new().unwrap();
        let source = "package calc\nfunc packageName() string { return \"os\" }\nfunc f(a, b int) bool { return a == b }\n";
        write_source(&dir, "calc.go", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "go",
            "calc.go",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_go_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        assert_eq!(rewrites.len(), 1);
        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.contains("import (\n    \"os\"\n)"));
        assert!(rewritten.contains("func __togi_active(id string) bool"));
        assert!(rewritten.contains(
            "func() bool { if __togi_active(\"7\") { return a != b }; return a == b }()"
        ));
        parse_go_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_java_files_expands_operator_mutation_to_expression_wrapper() {
        let dir = TempDir::new().unwrap();
        let source = "class Calc { static boolean f(int a, int b) { return a == b; } }\n";
        write_source(&dir, "Calc.java", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "java",
            "Calc.java",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_java_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        assert_eq!(rewrites.len(), 1);
        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.contains("private static boolean __togi_active(int id)"));
        assert!(rewritten.contains("return (__togi_active(7) ? (a != b) : (a == b));"));
        parse_java_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_java_files_ignores_togi_active_mentions_in_text() {
        let dir = TempDir::new().unwrap();
        let source = "class Calc { static String marker() { return \"__togi_active(\"; } static boolean f(int a, int b) { return a == b; } }\n";
        write_source(&dir, "Calc.java", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "java",
            "Calc.java",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_java_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert_eq!(
            rewritten
                .matches("private static boolean __togi_active")
                .count(),
            1,
            "{rewritten}"
        );
        parse_java_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_java_files_reuses_existing_togi_active_method_with_spacing() {
        let dir = TempDir::new().unwrap();
        let source = "class Calc { private static boolean __togi_active (int id) { return false; } static boolean f(int a, int b) { return a == b; } }\n";
        write_source(&dir, "Calc.java", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "java",
            "Calc.java",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_java_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert_eq!(
            rewritten
                .matches("private static boolean __togi_active")
                .count(),
            1,
            "{rewritten}"
        );
        parse_java_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_rust_files_expands_operator_mutation_to_expression_wrapper() {
        let dir = TempDir::new().unwrap();
        let source = "#![allow(dead_code)]\npub fn f(a: i32, b: i32) -> bool { a == b }\n";
        write_source(&dir, "src/lib.rs", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "rust",
            "src/lib.rs",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_rust_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        assert_eq!(rewrites.len(), 1);
        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.starts_with("#![allow(dead_code)]\n#[allow(dead_code)]\n"));
        assert!(rewritten.contains("fn __togi_active(id: u32) -> bool"));
        assert!(
            rewritten.contains("__togi_select(7, || { a == b }, || { a != b })"),
            "{rewritten}"
        );
        parse_rust_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_rust_files_inserts_helper_after_multiline_inner_attribute() {
        let dir = TempDir::new().unwrap();
        let source = "#![doc(\n    html_logo_url = \"https://example.com/logo.png\"\n)]\npub fn f(a: i32, b: i32) -> bool { a == b }\n";
        write_source(&dir, "src/lib.rs", source);
        let operator = source.find("==").unwrap();
        let mutation = mutation(
            "rust",
            "src/lib.rs",
            "==",
            "!=",
            operator..operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_rust_files(
            dir.path(),
            &[SchemaMutation {
                mutation,
                kind: SchemaKind::Expression,
            }],
        )
        .unwrap();

        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(
            rewritten.starts_with(
                "#![doc(\n    html_logo_url = \"https://example.com/logo.png\"\n)]\n#[allow(dead_code)]\n"
            ),
            "{rewritten}"
        );
        parse_rust_source(&rewritten).unwrap();
    }

    #[test]
    fn go_import_handling_accepts_spacing_variants() {
        let adapter = adapter_for_language("go").unwrap();
        for (import_decl, expected_imports) in [
            ("import(\"fmt\")", "import (\n    \"fmt\"\n    \"os\"\n)"),
            (
                "import\t\"fmt\"",
                "import\t\"fmt\"\nimport (\n    \"os\"\n)",
            ),
        ] {
            let source = format!(
                "package calc\n{import_decl}\nfunc packageName() string {{ return fmt.Sprintf(\"%s\", \"os\") }}\nfunc f(a, b int) bool {{ return a == b }}\n",
            );
            parse_go_source(&source).unwrap();
            assert!(go_source_imports(&source, "fmt"));
            assert!(!go_source_imports(&source, "os"));

            let rewritten = inject_go_runtime(&source, adapter).unwrap();

            assert!(
                rewritten.contains(expected_imports),
                "rewritten source did not contain {expected_imports:?}:\n{rewritten}"
            );
            parse_go_source(&rewritten).unwrap_or_else(|error| {
                panic!("rewritten source failed to parse: {error:?}\n{rewritten}")
            });
            assert!(
                rewritten.find("func __togi_active(").unwrap()
                    < rewritten.find("func packageName()").unwrap()
            );
        }
    }

    #[test]
    fn go_import_handling_scans_comments_and_multiple_declarations() {
        let adapter = adapter_for_language("go").unwrap();
        let source = "package calc\n\n// generated imports\nimport \"fmt\"\n\n// grouped imports\nimport (\n    \"strings\"\n)\n\nfunc packageName() string { return fmt.Sprintf(\"%s\", strings.TrimSpace(\"os\")) }\n";
        parse_go_source(source).unwrap();
        assert!(go_source_imports(source, "fmt"));
        assert!(go_source_imports(source, "strings"));
        assert!(!go_source_imports(source, "os"));

        let rewritten = inject_go_runtime(source, adapter).unwrap();

        assert!(rewritten.contains("import \"fmt\""));
        assert!(rewritten.contains("import (\n    \"strings\"\n    \"os\"\n)"));
        let helper = rewritten.find("func __togi_active(").unwrap();
        assert!(helper > rewritten.find("\"fmt\"").unwrap());
        assert!(helper > rewritten.find("\"os\"").unwrap());
        assert!(helper < rewritten.find("func packageName()").unwrap());
        parse_go_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_go_files_injects_one_helper_per_package_directory() {
        let dir = TempDir::new().unwrap();
        let first_source = "package calc\nfunc f(a, b int) bool { return a == b }\n";
        let second_source = "package calc\nfunc g(c, d int) bool { return c == d }\n";
        write_source(&dir, "first.go", first_source);
        write_source(&dir, "second.go", second_source);
        let first_operator = first_source.find("==").unwrap();
        let second_operator = second_source.find("==").unwrap();
        let first = mutation(
            "go",
            "first.go",
            "==",
            "!=",
            first_operator..first_operator + 2,
            "eq_to_neq",
        );
        let second = mutation(
            "go",
            "second.go",
            "==",
            "!=",
            second_operator..second_operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_go_files(
            dir.path(),
            &[
                SchemaMutation {
                    mutation: first,
                    kind: SchemaKind::Expression,
                },
                SchemaMutation {
                    mutation: second,
                    kind: SchemaKind::Expression,
                },
            ],
        )
        .unwrap();

        assert_eq!(rewrites.len(), 2);
        let helper_count = rewrites
            .iter()
            .filter(|rewrite| {
                String::from_utf8_lossy(&rewrite.content).contains("func __togi_active(")
            })
            .count();
        assert_eq!(helper_count, 1);
    }

    #[test]
    fn rewrite_go_files_injects_helpers_per_directory_and_package() {
        let dir = TempDir::new().unwrap();
        let first_source = "package calc\nfunc f(a, b int) bool { return a == b }\n";
        let second_source = "package calc_test\nfunc g(c, d int) bool { return c == d }\n";
        write_source(&dir, "calc.go", first_source);
        write_source(&dir, "calc_test.go", second_source);
        let first_operator = first_source.find("==").unwrap();
        let second_operator = second_source.find("==").unwrap();
        let first = mutation(
            "go",
            "calc.go",
            "==",
            "!=",
            first_operator..first_operator + 2,
            "eq_to_neq",
        );
        let second = mutation(
            "go",
            "calc_test.go",
            "==",
            "!=",
            second_operator..second_operator + 2,
            "eq_to_neq",
        );

        let rewrites = rewrite_go_files(
            dir.path(),
            &[
                SchemaMutation {
                    mutation: first,
                    kind: SchemaKind::Expression,
                },
                SchemaMutation {
                    mutation: second,
                    kind: SchemaKind::Expression,
                },
            ],
        )
        .unwrap();

        assert_eq!(rewrites.len(), 2);
        for rewrite in rewrites {
            let rewritten = String::from_utf8(rewrite.content).unwrap();
            assert!(rewritten.contains("func __togi_active("));
            parse_go_source(&rewritten).unwrap();
        }
    }

    #[test]
    fn plan_falls_back_for_rust_if_let_condition_mutation() {
        let dir = TempDir::new().unwrap();
        let source = "fn f(peer_ip: Option<std::net::IpAddr>) {\n    if let Some(peer_ip) = peer_ip {\n        log_ip(peer_ip);\n    }\n}\n";
        write_source(&dir, "src/lib.rs", source);

        let condition = "let Some(peer_ip) = peer_ip";
        let start = source.find(condition).unwrap();
        let plan = plan(
            dir.path(),
            vec![mutation(
                "rust",
                "src/lib.rs",
                condition,
                "!(let Some(peer_ip) = peer_ip)",
                start..start + condition.len(),
                "negate_condition",
            )],
        );

        assert!(plan.selected.is_empty());
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(
            plan.fallback[0].reason,
            SchemaSkipReason::UnsupportedSyntaxContext
        );
    }

    #[test]
    fn plan_falls_back_for_rust_while_let_pattern_mutation() {
        let dir = TempDir::new().unwrap();
        let source = "fn f(mut it: std::vec::IntoIter<i32>) {\n    while let Some(0) = it.next() {\n        tick();\n    }\n}\n";
        write_source(&dir, "src/lib.rs", source);

        let start = source.find("Some(0)").unwrap() + "Some(".len();
        let plan = plan(
            dir.path(),
            vec![mutation(
                "rust",
                "src/lib.rs",
                "0",
                "1",
                start..start + 1,
                "zero_to_one",
            )],
        );

        assert!(plan.selected.is_empty());
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(
            plan.fallback[0].reason,
            SchemaSkipReason::UnsupportedSyntaxContext
        );
    }

    #[test]
    fn plan_falls_back_for_rust_let_chain_condition_mutation() {
        let dir = TempDir::new().unwrap();
        let source = "fn f(mut it: std::vec::IntoIter<i32>, x: i32) {\n    if x > 0 && let Some(y) = it.next() {\n        use_pair(x, y);\n    }\n}\n";
        write_source(&dir, "src/lib.rs", source);

        let condition = "x > 0 && let Some(y) = it.next()";
        let start = source.find(condition).unwrap();
        let plan = plan(
            dir.path(),
            vec![mutation(
                "rust",
                "src/lib.rs",
                condition,
                "!(x > 0 && let Some(y) = it.next())",
                start..start + condition.len(),
                "negate_condition",
            )],
        );

        assert!(plan.selected.is_empty());
        assert_eq!(plan.fallback.len(), 1);
        assert_eq!(
            plan.fallback[0].reason,
            SchemaSkipReason::UnsupportedSyntaxContext
        );
    }

    #[test]
    fn plan_keeps_rust_mutations_inside_let_condition_value() {
        let dir = TempDir::new().unwrap();
        let source = "fn f(a: i32, b: i32) -> i32 {\n    if let Some(sum) = Some(a + b) {\n        sum\n    } else {\n        0\n    }\n}\n";
        write_source(&dir, "src/lib.rs", source);

        let start = source.find("a + b").unwrap();
        let plan = plan(
            dir.path(),
            vec![mutation(
                "rust",
                "src/lib.rs",
                "+",
                "-",
                start + 2..start + 3,
                "plus_to_minus",
            )],
        );

        assert!(plan.fallback.is_empty());
        assert_eq!(plan.selected.len(), 1);

        let rewrites = rewrite_rust_files(dir.path(), &plan.selected).unwrap();
        assert_eq!(rewrites.len(), 1);
        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(rewritten.contains("__togi_select(7, || { a + b }, || { a - b })"));
        parse_rust_source(&rewritten).unwrap();
    }

    #[test]
    fn plan_keeps_rust_binary_mutation_beside_let_chain() {
        let dir = TempDir::new().unwrap();
        let source = "fn f(mut it: std::vec::IntoIter<i32>, x: i32) {\n    if x > 0 && let Some(y) = it.next() {\n        use_pair(x, y);\n    }\n}\n";
        write_source(&dir, "src/lib.rs", source);

        let start = source.find("x > 0").unwrap() + "x ".len();
        let plan = plan(
            dir.path(),
            vec![mutation(
                "rust",
                "src/lib.rs",
                ">",
                ">=",
                start..start + 1,
                "gt_to_gte",
            )],
        );

        assert!(plan.fallback.is_empty());
        assert_eq!(plan.selected.len(), 1);

        let rewrites = rewrite_rust_files(dir.path(), &plan.selected).unwrap();
        let rewritten = String::from_utf8(rewrites[0].content.clone()).unwrap();
        assert!(
            rewritten.contains(
                "__togi_select(7, || { x > 0 }, || { x >= 0 }) && let Some(y) = it.next()"
            )
        );
        parse_rust_source(&rewritten).unwrap();
    }

    #[test]
    fn rewrite_rust_files_rejects_let_condition_wrap() {
        let dir = TempDir::new().unwrap();
        let source = "fn f(peer_ip: Option<std::net::IpAddr>) {\n    if let Some(peer_ip) = peer_ip {\n        log_ip(peer_ip);\n    }\n}\n";
        write_source(&dir, "src/lib.rs", source);

        let condition = "let Some(peer_ip) = peer_ip";
        let start = source.find(condition).unwrap();
        let selected = vec![SchemaMutation {
            mutation: mutation(
                "rust",
                "src/lib.rs",
                condition,
                "!(let Some(peer_ip) = peer_ip)",
                start..start + condition.len(),
                "negate_condition",
            ),
            kind: SchemaKind::Expression,
        }];

        assert!(rewrite_rust_files(dir.path(), &selected).is_err());
    }
}
