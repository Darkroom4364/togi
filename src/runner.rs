// Parallel test execution with timeouts

use crate::cache::{self, CacheKey};
use crate::{
    BuildErrorDiagnostic, Mutation, MutationReport, MutationResult, SchemataFallbackReasonCount,
    SchemataReport,
};
use anyhow::{Context, bail};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::hash::Hasher;
use std::io::{IsTerminal, Read, Write};
use std::panic::{AssertUnwindSafe as PanicBoundary, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURED_OUTPUT_LIMIT: usize = 1024 * 1024;
const CAPTURE_CLEANUP_TIMEOUT: Duration = Duration::from_millis(100);

/// Write mutation workspace content. Workspaces are disposable temp copies, so
/// durable temp-file + fsync writes would only add hot-loop I/O overhead.
fn write_workspace_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    fs::write(path, data)
}

/// Commands and timeouts used while evaluating mutations.
///
/// `command` is the default test command. Project and language commands can
/// override it for matching mutations unless `force_default_command` is set
/// by a CLI command override. `build_command`, when explicitly enabled by the
/// CLI/config, runs before tests to classify uncompilable mutations as build
/// errors.
pub struct CommandConfig {
    /// Default test command, stored as argv.
    pub command: Vec<String>,
    /// Optional wrapper prefixed to every build and test command.
    pub sandbox_command: Vec<String>,
    /// True when the default command came from a CLI override and must win.
    pub force_default_command: bool,
    /// True when the default timeout came from a CLI override and must win.
    pub force_default_timeout: bool,
    /// Per-project test command and timeout overrides, longest path wins.
    pub project_commands: Vec<ProjectCommandConfig>,
    /// Per-language test command overrides keyed by `LanguageSupport::name()`.
    pub language_commands: HashMap<String, Vec<String>>,
    /// Optional build-check command, stored as argv.
    pub build_command: Vec<String>,
    /// Whether the build command came from user config/CLI rather than detection.
    pub build_command_explicit: bool,
    /// Default per-mutation timeout.
    pub timeout: Duration,
    /// Per-language timeout overrides keyed by `LanguageSupport::name()`.
    pub language_timeouts: HashMap<String, Duration>,
    /// Optional source-line to test-name map used to narrow test commands.
    pub test_selection: Option<TestSelectionConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCommandConfig {
    pub path: PathBuf,
    pub command: Option<Vec<String>>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Default)]
pub struct TestSelectionConfig {
    tests_by_line: HashMap<(String, usize), Vec<SelectedTest>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedTest {
    pub name: String,
    pub duration_ms: Option<u64>,
}

impl SelectedTest {
    pub fn new(name: impl Into<String>, duration_ms: Option<u64>) -> Self {
        Self {
            name: name.into(),
            duration_ms,
        }
    }
}

impl TestSelectionConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        project_root: &Path,
        file: impl AsRef<Path>,
        line: usize,
        tests: Vec<String>,
    ) {
        self.insert_tests(
            project_root,
            file,
            line,
            tests
                .into_iter()
                .map(|name| SelectedTest::new(name, None))
                .collect(),
        );
    }

    pub fn insert_tests(
        &mut self,
        project_root: &Path,
        file: impl AsRef<Path>,
        line: usize,
        tests: Vec<SelectedTest>,
    ) {
        self.tests_by_line.insert(
            (normalized_cache_path(project_root, file.as_ref()), line),
            tests,
        );
    }

    fn tests_for(&self, project_root: &Path, mutation: &Mutation) -> Option<Vec<String>> {
        let mut tests = self
            .tests_by_line
            .get(&(
                normalized_cache_path(project_root, &mutation.file),
                mutation.line,
            ))?
            .iter()
            .collect::<Vec<_>>();
        tests.sort_by_key(|test| test.duration_ms.unwrap_or(u64::MAX));
        Some(tests.into_iter().map(|test| test.name.clone()).collect())
            .filter(|tests: &Vec<String>| !tests.is_empty())
    }
}

/// Optional conditions that stop scheduling new mutations once enough signal
/// has been observed for a PR gate.
#[derive(Debug, Clone, Default)]
pub struct EarlyStopConfig {
    /// Stop after at least this many survived mutants have completed.
    pub max_survivors: Option<usize>,
    /// Stop when even killing every remaining mutation could not satisfy this score.
    pub fail_under: Option<f64>,
}

impl EarlyStopConfig {
    fn is_enabled(&self) -> bool {
        self.max_survivors.is_some() || self.fail_under.is_some()
    }
}

/// Applies mutations, runs checks, restores files, and aggregates results.
///
/// The runner evaluates mutations in isolated workspace copies so whole-project
/// test commands do not observe another active mutation. It still honors
/// cancellation, cache lookups, output capture, and per-language command/timeout
/// selection.
pub struct TestRunner {
    /// Test/build commands and timeout configuration.
    pub commands: CommandConfig,
    /// Maximum number of scheduled mutation tasks.
    pub parallelism: usize,
    /// Repository root where commands run and mutation paths are resolved.
    pub project_root: PathBuf,
    /// Print every mutation result as it runs.
    pub verbose: bool,
    /// Capture and print output for survived mutations.
    pub show_output: bool,
    /// Optional cap on how many non-build-error mutations are tested.
    pub max_tested: Option<usize>,
    /// Optional stop conditions for gate-style partial runs.
    pub early_stop: EarlyStopConfig,
    /// Whether workspace copies should honor `.ignore`/`.gitignore` rules.
    pub respect_workspace_ignores: bool,
    /// Extra environment variables passed to every spawned command.
    pub env: HashMap<String, String>,
    /// Use structured incremental history in addition to exact cache entries.
    pub incremental_history: bool,
    /// Re-run mutations instead of trusting cache or incremental history hits.
    pub force_rerun: bool,
    /// Set to true externally (e.g. Ctrl+C handler) to stop spawning new mutations.
    pub cancelled: Arc<AtomicBool>,
}

/// Result of a runner invocation.
///
/// `report` contains the completed mutation results. `cancelled` is true when
/// the runner observed cancellation before or during execution.
#[derive(Debug)]
pub struct RunOutcome {
    pub report: MutationReport,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineMeasurement {
    pub build_duration: Option<Duration>,
    pub test_duration: Duration,
}

pub struct BaselineTimingConfig<'a> {
    pub test_command: &'a [String],
    pub build_command: &'a [String],
    pub sandbox_command: &'a [String],
    pub build_command_explicit: bool,
    pub timeout: Duration,
    pub env: &'a HashMap<String, String>,
    pub cancelled: &'a AtomicBool,
    pub respect_workspace_ignores: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedTestCommand {
    argv: Vec<String>,
    timeout: Duration,
    selected_tests: Vec<String>,
}

impl SelectedTestCommand {
    fn cache_context(
        &self,
        build_command: &[String],
        build_command_explicit: bool,
        sandbox_command: &[String],
        env: &HashMap<String, String>,
    ) -> String {
        let build_str = if build_command_explicit {
            format!("{build_command:?}")
        } else {
            String::new()
        };
        let sandbox_str = if sandbox_command.is_empty() {
            String::new()
        } else {
            format!("{sandbox_command:?}")
        };
        let mut env_parts: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        env_parts.sort();
        format!(
            "test={:?};build={};sandbox={};timeout={};env={}",
            self.argv,
            build_str,
            sandbox_str,
            self.timeout.as_millis(),
            env_parts.join(",")
        )
    }
}

#[cfg(test)]
fn select_test_command(
    project_root: &Path,
    commands: &CommandConfig,
    mutation: &Mutation,
) -> SelectedTestCommand {
    select_test_command_with_history(project_root, commands, mutation, None)
}

fn select_test_command_with_history(
    project_root: &Path,
    commands: &CommandConfig,
    mutation: &Mutation,
    history: Option<&cache::IncrementalHistoryStore>,
) -> SelectedTestCommand {
    let project_info = matching_project_command(project_root, commands, mutation);

    let mut argv = project_info
        .filter(|_| !commands.force_default_command)
        .and_then(|project| project.command.as_ref())
        .or_else(|| {
            (!commands.force_default_command)
                .then(|| commands.language_commands.get(mutation.language.as_str()))
                .flatten()
        })
        .unwrap_or(&commands.command)
        .clone();
    let mut selected_tests = Vec::new();

    if let Some(test_selection) = &commands.test_selection {
        if let Some(mut tests) = test_selection.tests_for(project_root, mutation) {
            if let Some(preferred) = history.and_then(|history| {
                history.preferred_killer_test(
                    &cache_identity(project_root, mutation),
                    &mutation.description,
                    &tests,
                )
            }) {
                tests.sort_by_key(|test| if *test == preferred { 0 } else { 1 });
            }
            argv = narrow_test_command(argv, &tests);
            selected_tests = tests;
        }
    }

    SelectedTestCommand {
        argv,
        timeout: if commands.force_default_timeout {
            commands.timeout
        } else {
            project_info
                .and_then(|project| project.timeout)
                .or_else(|| {
                    commands
                        .language_timeouts
                        .get(mutation.language.as_str())
                        .copied()
                })
                .unwrap_or(commands.timeout)
        },
        selected_tests,
    }
}

fn matching_project_command<'a>(
    project_root: &Path,
    commands: &'a CommandConfig,
    mutation: &Mutation,
) -> Option<&'a ProjectCommandConfig> {
    let mutation_path = normalized_cache_path(project_root, &mutation.file);
    let mutation_parts: Vec<&str> = mutation_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();

    commands
        .project_commands
        .iter()
        .filter_map(|project| {
            let project_path = normalized_cache_path(project_root, &project.path);
            let project_parts: Vec<&str> = project_path
                .split('/')
                .filter(|part| !part.is_empty())
                .collect();
            (!project_parts.is_empty()
                && project_parts.len() <= mutation_parts.len()
                && mutation_parts
                    .iter()
                    .zip(&project_parts)
                    .all(|(mutation, project)| mutation == project))
            .then_some((project_parts.len(), project))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, project)| project)
}

fn narrow_test_command(argv: Vec<String>, tests: &[String]) -> Vec<String> {
    let narrowed = narrow_go_test_command(argv.clone(), tests);
    if narrowed != argv {
        return narrowed;
    }
    let narrowed = narrow_pytest_command(argv.clone(), tests);
    if narrowed != argv {
        return narrowed;
    }
    let narrowed = narrow_jest_or_vitest_command(argv.clone(), tests);
    if narrowed != argv {
        return narrowed;
    }
    let narrowed = narrow_cargo_test_command(argv.clone(), tests);
    if narrowed != argv {
        return narrowed;
    }
    let narrowed = narrow_maven_test_command(argv.clone(), tests);
    if narrowed != argv {
        return narrowed;
    }
    narrow_gradle_test_command(argv, tests)
}

fn narrow_go_test_command(mut argv: Vec<String>, tests: &[String]) -> Vec<String> {
    if argv.len() < 2 || argv[0] != "go" || argv[1] != "test" {
        return argv;
    }

    let pattern = format!(
        "^({})$",
        tests
            .iter()
            .map(|test| escape_go_test_regex(test))
            .collect::<Vec<_>>()
            .join("|")
    );

    for i in 2..argv.len() {
        if argv[i] == "-run" {
            if i + 1 < argv.len() {
                argv[i + 1] = pattern;
            } else {
                argv.push(pattern);
            }
            return argv;
        }
        if argv[i].starts_with("-run=") {
            argv[i] = format!("-run={pattern}");
            return argv;
        }
    }

    argv.splice(2..2, ["-run".to_string(), pattern]);
    argv
}

fn narrow_pytest_command(mut argv: Vec<String>, tests: &[String]) -> Vec<String> {
    if !is_pytest_command(&argv) {
        return argv;
    }
    if tests.iter().all(|test| is_pytest_node_id(test)) {
        argv.extend(tests.iter().cloned());
        return argv;
    }

    if !tests.iter().all(|test| is_simple_pytest_keyword(test)) {
        return argv;
    }
    let expression = tests.join(" or ");
    argv.extend(["-k".to_string(), expression]);
    argv
}

fn is_pytest_command(argv: &[String]) -> bool {
    matches!(argv.first().map(String::as_str), Some("pytest" | "py.test"))
        || matches!(
            (
                argv.first().map(String::as_str),
                argv.get(1).map(String::as_str),
                argv.get(2).map(String::as_str)
            ),
            (Some("python" | "python3"), Some("-m"), Some("pytest"))
        )
}

fn is_pytest_node_id(test: &str) -> bool {
    test.contains("::") || test.ends_with(".py")
}

fn is_simple_pytest_keyword(test: &str) -> bool {
    !test.is_empty()
        && test
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn narrow_jest_or_vitest_command(mut argv: Vec<String>, tests: &[String]) -> Vec<String> {
    if !is_jest_or_vitest_command(&argv) {
        return argv;
    }
    let pattern = exact_regex_pattern(tests);
    argv.extend(["-t".to_string(), pattern]);
    argv
}

fn is_jest_or_vitest_command(argv: &[String]) -> bool {
    matches!(argv.first().map(String::as_str), Some("jest" | "vitest"))
        || matches!(
            (
                argv.first().map(String::as_str),
                argv.get(1).map(String::as_str)
            ),
            (Some("npx"), Some("jest" | "vitest"))
        )
}

fn narrow_cargo_test_command(mut argv: Vec<String>, tests: &[String]) -> Vec<String> {
    if argv.len() < 2 || argv[0] != "cargo" || argv[1] != "test" || tests.len() != 1 {
        return argv;
    }
    insert_before_double_dash(&mut argv, tests[0].clone());
    argv
}

fn narrow_maven_test_command(mut argv: Vec<String>, tests: &[String]) -> Vec<String> {
    if argv.is_empty() || argv[0] != "mvn" || !argv.iter().any(|arg| arg == "test") {
        return argv;
    }
    let value = format!("-Dtest={}", tests.join(","));
    if let Some(existing) = argv.iter_mut().find(|arg| arg.starts_with("-Dtest=")) {
        *existing = value;
    } else {
        argv.push(value);
    }
    argv
}

fn narrow_gradle_test_command(mut argv: Vec<String>, tests: &[String]) -> Vec<String> {
    if argv.is_empty()
        || !matches!(argv[0].as_str(), "gradle" | "./gradlew" | "gradlew")
        || !argv.iter().any(|arg| arg == "test")
    {
        return argv;
    }
    for test in tests {
        argv.extend(["--tests".to_string(), test.clone()]);
    }
    argv
}

fn insert_before_double_dash(argv: &mut Vec<String>, value: String) {
    let insert_at = argv
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(argv.len());
    argv.insert(insert_at, value);
}

fn exact_regex_pattern(tests: &[String]) -> String {
    format!(
        "^({})$",
        tests
            .iter()
            .map(|test| escape_test_regex(test))
            .collect::<Vec<_>>()
            .join("|")
    )
}

fn escape_go_test_regex(test: &str) -> String {
    escape_test_regex(test)
}

fn escape_test_regex(test: &str) -> String {
    let mut escaped = String::new();
    for ch in test.chars() {
        if matches!(
            ch,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

struct FileGuard {
    path: PathBuf,
    original: Vec<u8>,
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        if let Err(e) = write_workspace_file(&self.path, &self.original) {
            eprintln!(
                "error: failed to restore {}: {} — file may be corrupted, check git status",
                self.path.display(),
                e
            );
        }
    }
}

pub(crate) struct WorkspaceCopy {
    _tempdir: tempfile::TempDir,
    root: PathBuf,
    reset_strategy: WorkspaceResetStrategy,
}

enum WorkspaceResetStrategy {
    Copy,
    GitWorktree {
        project_root: PathBuf,
        overlay: GitWorktreeOverlay,
    },
}

#[derive(Clone)]
struct GitWorktreeOverlay {
    copy_paths: Vec<PathBuf>,
    remove_paths: Vec<PathBuf>,
}

impl GitWorktreeOverlay {
    fn apply(&self, project_root: &Path, workspace_root: &Path) -> std::io::Result<()> {
        for relative in &self.remove_paths {
            remove_workspace_path(&workspace_root.join(relative))?;
        }
        for relative in &self.copy_paths {
            copy_overlay_file(project_root, workspace_root, relative)?;
        }
        Ok(())
    }
}

impl WorkspaceCopy {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    fn reset(&self, project_root: &Path, respect_ignores: bool) -> std::io::Result<()> {
        match &self.reset_strategy {
            WorkspaceResetStrategy::Copy => {
                reset_copied_workspace(project_root, &self.root, respect_ignores)
            }
            WorkspaceResetStrategy::GitWorktree {
                project_root,
                overlay,
            } => reset_git_worktree(project_root, &self.root, overlay),
        }
    }
}

impl Drop for WorkspaceCopy {
    fn drop(&mut self) {
        let WorkspaceResetStrategy::GitWorktree { project_root, .. } = &self.reset_strategy else {
            return;
        };
        if !self.root.exists() {
            return;
        }
        if let Err(e) = remove_git_worktree(project_root, &self.root) {
            eprintln!("warning: {e}");
        }
    }
}

pub(crate) fn should_skip_workspace_entry(relative: &Path) -> bool {
    relative.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|name| {
            matches!(
                name,
                ".git"
                    | ".togi"
                    | ".togi-cache"
                    | ".togi.lock"
                    | ".codex"
                    | ".claude"
                    | "target"
                    | "node_modules"
                    | ".venv"
                    | "dist"
                    | "build"
            )
        })
    })
}

fn should_copy_workspace_entry(project_root: &Path, path: &Path) -> bool {
    path == project_root
        || path
            .strip_prefix(project_root)
            .is_ok_and(|relative| !should_skip_workspace_entry(relative))
}

/// Workspace directories kept across resets.
///
/// Stashes are created in `workspace_root.parent()` as `.togi-preserved-{name}`
/// so `fs::rename` stays on the same filesystem and remains metadata-only.
/// Any stale stash from an interrupted prior reset is reclaimed before rename.
const PRESERVED_WORKSPACE_DIRS: &[&str] = &["target"];

fn cache_context_fingerprint(project_root: &Path) -> u64 {
    git_cache_context_fingerprint(project_root)
        .unwrap_or_else(|| filesystem_cache_context_fingerprint(project_root))
}

fn filesystem_cache_context_fingerprint(project_root: &Path) -> u64 {
    let mut files = Vec::new();
    collect_cache_context_files(project_root, project_root, &mut files);
    files.sort_by_key(|path| normalized_cache_path(project_root, path));

    let mut hasher = StableCacheHasher::default();
    update_cache_hash(&mut hasher, b"filesystem");
    for relative in files {
        let path_key = normalized_cache_path(project_root, &relative);
        update_cache_hash(&mut hasher, path_key.as_bytes());
        if let Ok(content) = fs::read(project_root.join(&relative)) {
            update_cache_hash(&mut hasher, &content);
        }
    }
    hasher.finish()
}

fn git_cache_context_fingerprint(project_root: &Path) -> Option<u64> {
    if git_cache_context_is_dirty(project_root)? {
        return None;
    }

    let output = std::process::Command::new("git")
        .args(["ls-files", "-z", "-s", "--"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let mut files = Vec::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let tab = entry.iter().position(|byte| *byte == b'\t')?;
        let metadata = String::from_utf8_lossy(&entry[..tab]);
        let mut fields = metadata.split_whitespace();
        let _mode = fields.next()?;
        let object_id = fields.next()?.to_string();
        let relative = PathBuf::from(String::from_utf8_lossy(&entry[tab + 1..]).into_owned());
        if is_cache_context_file(&relative) {
            files.push((relative, object_id));
        }
    }

    files.sort_by_key(|(path, _)| normalized_cache_path(project_root, path));

    let mut hasher = StableCacheHasher::default();
    update_cache_hash(&mut hasher, b"git-index");
    for (relative, object_id) in files {
        let path_key = normalized_cache_path(project_root, &relative);
        update_cache_hash(&mut hasher, path_key.as_bytes());
        update_cache_hash(&mut hasher, object_id.as_bytes());
    }
    Some(hasher.finish())
}

fn git_cache_context_is_dirty(project_root: &Path) -> Option<bool> {
    // No -M/--find-renames here: porcelain v1 -z then stays in the
    // single-token "XY path\0" form this parser expects.
    let output = std::process::Command::new("git")
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
        ])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.len() <= 3 || entry[2] != b' ' {
            continue;
        }
        let relative = PathBuf::from(String::from_utf8_lossy(&entry[3..]).into_owned());
        if is_cache_context_file(&relative) {
            return Some(true);
        }
    }
    Some(false)
}

fn collect_cache_context_files(project_root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let relative = path.strip_prefix(project_root).unwrap_or(&path);
        if should_skip_workspace_entry(relative) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            collect_cache_context_files(project_root, &path, out);
        } else if file_type.is_file() && is_cache_context_file(relative) {
            out.push(relative.to_path_buf());
        }
    }
}

/// Files whose content can change mutation verdicts beyond the mutated file
/// itself: build manifests, CI workflows, tests — and any source file, since
/// sources both shape which tests compile and run (e.g. Rust `mod` wiring) and
/// can carry colocated tests (`#[cfg(test)]` modules, specs). A verdict cached
/// before such a change must not be reused (#410).
fn is_cache_context_file(relative: &Path) -> bool {
    let path_key = normalized_cache_path(Path::new(""), relative).to_ascii_lowercase();
    let file_name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if has_source_context_extension(&file_name) {
        return true;
    }

    if matches!(
        file_name.as_str(),
        "togi.toml"
            | "cargo.toml"
            | "cargo.lock"
            | "go.mod"
            | "go.sum"
            | "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lock"
            | "bun.lockb"
            | "pyproject.toml"
            | "setup.py"
            | "setup.cfg"
            | "pytest.ini"
            | "tox.ini"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "gemfile"
            | "gemfile.lock"
            | "cmakelists.txt"
    ) {
        return true;
    }

    if path_key.starts_with(".github/workflows/")
        && (file_name.ends_with(".yml") || file_name.ends_with(".yaml"))
    {
        return true;
    }

    let in_test_dir = relative.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|part| {
            matches!(
                part.to_ascii_lowercase().as_str(),
                "test" | "tests" | "spec" | "specs" | "__tests__" | "__specs__"
            )
        })
    });
    if in_test_dir {
        return has_test_context_extension(&file_name);
    }

    file_name.starts_with("test_")
        || file_name.ends_with("_test.go")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with("_test.py")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name.ends_with("test.java")
        || file_name.ends_with("tests.java")
}

/// Extensions of source files togi can mutate. Any of them can influence test
/// compilation or execution, so all of them are verdict-cache context.
fn has_source_context_extension(file_name: &str) -> bool {
    matches!(
        Path::new(file_name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or(""),
        "go" | "rs"
            | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "cxx"
            | "h"
            | "hpp"
            | "cs"
            | "rb"
    )
}

fn has_test_context_extension(file_name: &str) -> bool {
    matches!(
        Path::new(file_name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or(""),
        "go" | "rs"
            | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "cxx"
            | "h"
            | "hpp"
            | "cs"
            | "rb"
            | "toml"
            | "json"
            | "yaml"
            | "yml"
            | "xml"
    )
}

const STABLE_HASH_OFFSET: u64 = 0xcbf29ce484222325;
const STABLE_HASH_PRIME: u64 = 0x100000001b3;

struct StableCacheHasher {
    hash: u64,
}

impl Default for StableCacheHasher {
    fn default() -> Self {
        Self {
            hash: STABLE_HASH_OFFSET,
        }
    }
}

impl Hasher for StableCacheHasher {
    fn finish(&self) -> u64 {
        self.hash
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.hash ^= u64::from(*byte);
            self.hash = self.hash.wrapping_mul(STABLE_HASH_PRIME);
        }
    }
}

fn update_cache_hash(hasher: &mut impl Hasher, bytes: &[u8]) {
    hasher.write(&(bytes.len() as u64).to_le_bytes());
    hasher.write(bytes);
}

#[derive(Default)]
struct SourceContentCache {
    contents: Mutex<HashMap<String, Option<Vec<u8>>>>,
}

impl SourceContentCache {
    fn content_for(&self, project_root: &Path, mutation_file: &Path) -> Option<Vec<u8>> {
        let key = normalized_cache_path(project_root, mutation_file);
        let Ok(mut contents) = self.contents.lock() else {
            eprintln!("warning: source content cache mutex poisoned");
            return read_source_content(project_root, mutation_file);
        };
        if let Some(content) = contents.get(&key) {
            return content.clone();
        }

        let content = read_source_content(project_root, mutation_file);
        contents.insert(key, content.clone());
        content
    }

    #[cfg(test)]
    fn cached_entry_count(&self) -> usize {
        self.contents
            .lock()
            .expect("source content cache mutex poisoned")
            .len()
    }
}

#[derive(Default)]
struct TestContextIndex {
    files: Vec<TestContextFile>,
}

struct TestContextFile {
    key: String,
    content: Vec<u8>,
    text: Option<String>,
}

impl TestContextIndex {
    fn build(project_root: &Path) -> Self {
        let mut files = Vec::new();
        collect_cache_context_files(project_root, project_root, &mut files);
        files.sort_by_key(|path| normalized_cache_path(project_root, path));

        let files = files
            .into_iter()
            .filter_map(|relative| {
                let content = fs::read(project_root.join(&relative)).ok()?;
                let key = normalized_cache_path(project_root, &relative);
                let text = String::from_utf8(content.clone()).ok();
                Some(TestContextFile { key, content, text })
            })
            .collect();

        Self { files }
    }

    fn fingerprint_for_tests(&self, tests: &[String], fallback: u64) -> u64 {
        if tests.is_empty() || self.files.is_empty() {
            return fallback;
        }

        let mut matched = BTreeSet::new();
        for test in tests {
            let matches = self.files_for_test(test);
            if matches.is_empty() {
                return fallback;
            }
            matched.extend(matches);
        }

        let mut hasher = StableCacheHasher::default();
        update_cache_hash(&mut hasher, b"selected-test-context-v1");
        for test in tests {
            update_cache_hash(&mut hasher, test.as_bytes());
        }
        for index in matched {
            if let Some(file) = self.files.get(index) {
                update_cache_hash(&mut hasher, file.key.as_bytes());
                update_cache_hash(&mut hasher, &file.content);
            }
        }
        hasher.finish()
    }

    fn files_for_test(&self, test: &str) -> Vec<usize> {
        if let Some(path_key) = direct_test_path_key(test) {
            return self
                .files
                .iter()
                .enumerate()
                .filter_map(|(index, file)| (file.key == path_key).then_some(index))
                .collect();
        }

        let tokens = test_name_tokens(test);
        if tokens.is_empty() {
            return Vec::new();
        }

        self.files
            .iter()
            .enumerate()
            .filter_map(|(index, file)| {
                let text = file.text.as_ref()?;
                tokens
                    .iter()
                    .all(|token| text.contains(token))
                    .then_some(index)
            })
            .collect()
    }
}

fn direct_test_path_key(test: &str) -> Option<String> {
    let path_part = test.split("::").next().unwrap_or(test);
    if !looks_like_test_path(path_part) {
        return None;
    }
    Some(normalized_cache_path(Path::new(""), Path::new(path_part)))
}

fn looks_like_test_path(value: &str) -> bool {
    (value.contains('/') || value.contains('\\'))
        && matches!(
            Path::new(value)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or(""),
            "go" | "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "java" | "cs" | "rb"
        )
}

fn test_name_tokens(test: &str) -> Vec<String> {
    if test.chars().any(char::is_whitespace) {
        return (test.len() >= 3)
            .then(|| test.to_string())
            .into_iter()
            .collect();
    }

    let mut tokens = BTreeSet::new();
    for token in test.split([':', '#', '.']) {
        let token = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_');
        if token.len() >= 3 {
            tokens.insert(token.to_string());
        }
    }
    tokens.into_iter().collect()
}

fn read_source_content(project_root: &Path, mutation_file: &Path) -> Option<Vec<u8>> {
    let Ok(file_path) = validate_and_resolve_mutation_path(project_root, mutation_file) else {
        return None;
    };
    #[cfg(test)]
    notify_source_content_read(&file_path);
    fs::read(file_path).ok()
}

#[cfg(test)]
type SourceContentReadHook = Arc<dyn Fn(&Path) + Send + Sync + 'static>;

#[cfg(test)]
static SOURCE_CONTENT_READ_HOOK: std::sync::OnceLock<Mutex<Option<SourceContentReadHook>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
fn set_source_content_read_hook(hook: Option<SourceContentReadHook>) {
    let lock = SOURCE_CONTENT_READ_HOOK.get_or_init(|| Mutex::new(None));
    *lock.lock().expect("source content read hook poisoned") = hook;
}

#[cfg(test)]
fn notify_source_content_read(path: &Path) {
    let Some(lock) = SOURCE_CONTENT_READ_HOOK.get() else {
        return;
    };
    let Ok(hook) = lock.lock() else {
        return;
    };
    if let Some(hook) = hook.as_ref() {
        hook(path);
    }
}

pub(crate) fn copy_workspace(project_root: &Path) -> std::io::Result<WorkspaceCopy> {
    copy_workspace_with_options(project_root, true)
}

fn copy_workspace_with_options(
    project_root: &Path,
    respect_ignores: bool,
) -> std::io::Result<WorkspaceCopy> {
    let tempdir = tempfile::tempdir()?;
    let root = tempdir.path().join("workspace");

    if respect_ignores {
        match create_git_worktree_workspace(project_root, &root) {
            Ok(Some(overlay)) => {
                return Ok(WorkspaceCopy {
                    _tempdir: tempdir,
                    root,
                    reset_strategy: WorkspaceResetStrategy::GitWorktree {
                        project_root: project_root.to_path_buf(),
                        overlay,
                    },
                });
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "warning: could not create git worktree workspace: {e}; falling back to copy"
                );
            }
        }
    }

    fs::create_dir(&root)?;
    populate_workspace(project_root, &root, respect_ignores)?;

    Ok(WorkspaceCopy {
        _tempdir: tempdir,
        root,
        reset_strategy: WorkspaceResetStrategy::Copy,
    })
}

pub fn measure_baseline_timing(
    project_root: &Path,
    config: BaselineTimingConfig<'_>,
) -> anyhow::Result<BaselineMeasurement> {
    if config.test_command.is_empty() {
        bail!("baseline test command is empty");
    }

    let workspace = copy_workspace_with_options(project_root, config.respect_workspace_ignores)
        .with_context(|| "could not create baseline timing workspace")?;
    let root = workspace.root();
    let build_duration = if config.build_command_explicit && !config.build_command.is_empty() {
        Some(measure_baseline_command(
            "baseline build command",
            config.build_command,
            config.sandbox_command,
            root,
            config.timeout,
            config.env,
            config.cancelled,
        )?)
    } else {
        None
    };
    let test_duration = measure_baseline_command(
        "baseline test command",
        config.test_command,
        config.sandbox_command,
        root,
        config.timeout,
        config.env,
        config.cancelled,
    )?;

    Ok(BaselineMeasurement {
        build_duration,
        test_duration,
    })
}

fn measure_baseline_command(
    label: &str,
    command: &[String],
    sandbox_command: &[String],
    cwd: &Path,
    timeout: Duration,
    env: &HashMap<String, String>,
    cancelled: &AtomicBool,
) -> anyhow::Result<Duration> {
    let started = Instant::now();
    let outcome = run_command(command, sandbox_command, cwd, timeout, true, env, cancelled);
    let duration = started.elapsed();
    if outcome.cancelled {
        bail!("baseline timing cancelled");
    }

    match outcome.result {
        MutationResult::Survived => Ok(duration),
        MutationResult::Killed => {
            let output = baseline_failure_output(outcome.test_output.as_deref());
            bail!(
                "{label} failed (`{}`){output}",
                command_for_message(command)
            )
        }
        MutationResult::Timeout => bail!(
            "{label} timed out after {:.2}s (`{}`)",
            timeout.as_secs_f64(),
            command_for_message(command)
        ),
        MutationResult::BuildError => {
            let detail = outcome
                .build_error_detail
                .as_ref()
                .map(|detail| detail.message.as_str())
                .unwrap_or("command could not run");
            bail!(
                "{label} could not run (`{}`): {detail}",
                command_for_message(command)
            )
        }
    }
}

fn command_for_message(command: &[String]) -> String {
    if command.is_empty() {
        "<empty>".to_string()
    } else {
        command.join(" ")
    }
}

fn sandboxed_command(sandbox_command: &[String], command: &[String]) -> Vec<String> {
    if sandbox_command.is_empty() {
        return command.to_vec();
    }
    let mut argv = sandbox_command.to_vec();
    argv.extend_from_slice(command);
    argv
}

fn baseline_failure_output(output: Option<&str>) -> String {
    let Some(output) = output.map(str::trim).filter(|output| !output.is_empty()) else {
        return String::new();
    };
    let excerpt = output.lines().take(6).collect::<Vec<_>>().join("\n");
    format!(":\n{excerpt}")
}

fn create_git_worktree_workspace(
    project_root: &Path,
    root: &Path,
) -> std::io::Result<Option<GitWorktreeOverlay>> {
    if !git_worktree_workspace_is_available(project_root) {
        return Ok(None);
    }
    let overlay = collect_git_worktree_overlay(project_root)?;

    let output = std::process::Command::new("git")
        .args(["worktree", "add", "--detach", "--quiet"])
        .arg(root)
        .arg("HEAD")
        .current_dir(project_root)
        .output()?;
    if output.status.success() {
        if let Err(e) = overlay.apply(project_root, root) {
            let _ = remove_git_worktree(project_root, root);
            return Err(e);
        }
        return Ok(Some(overlay));
    }

    if root.exists() {
        let _ = fs::remove_dir_all(root);
    }
    Ok(None)
}

fn git_worktree_workspace_is_available(project_root: &Path) -> bool {
    let Some(top_level) = git_top_level(project_root) else {
        return false;
    };
    let Ok(project_root) = project_root.canonicalize() else {
        return false;
    };
    if top_level != project_root {
        return false;
    }

    std::process::Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(&project_root)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_top_level(project_root: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let top_level = String::from_utf8(output.stdout).ok()?;
    PathBuf::from(top_level.trim()).canonicalize().ok()
}

fn collect_git_worktree_overlay(project_root: &Path) -> std::io::Result<GitWorktreeOverlay> {
    let changed_paths = git_z_output_paths(
        project_root,
        &["diff", "--name-only", "-z", "--no-renames", "HEAD", "--"],
    )?;
    let untracked_paths = git_z_output_paths(
        project_root,
        &["ls-files", "-z", "--others", "--exclude-standard", "--"],
    )?;

    let mut copy_paths = BTreeSet::new();
    let mut remove_paths = BTreeSet::new();

    for relative in changed_paths {
        if !should_overlay_workspace_entry(&relative) {
            continue;
        }
        let source = project_root.join(&relative);
        if source.is_file() {
            copy_paths.insert(relative);
        } else {
            remove_paths.insert(relative);
        }
    }

    for relative in untracked_paths {
        if !should_overlay_workspace_entry(&relative) {
            continue;
        }
        if project_root.join(&relative).is_file() {
            copy_paths.insert(relative);
        }
    }

    for copied in &copy_paths {
        remove_paths.remove(copied);
    }

    Ok(GitWorktreeOverlay {
        copy_paths: copy_paths.into_iter().collect(),
        remove_paths: remove_paths.into_iter().collect(),
    })
}

fn git_z_output_paths(project_root: &Path, args: &[&str]) -> std::io::Result<Vec<PathBuf>> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(project_root)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git {} failed in {}\nstderr:\n{}",
            args.join(" "),
            project_root.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(safe_git_relative_path)
        .collect())
}

fn should_overlay_workspace_entry(relative: &Path) -> bool {
    is_safe_relative_path(relative) && !should_skip_workspace_entry(relative)
}

fn safe_git_relative_path(raw: &[u8]) -> Option<PathBuf> {
    let raw = std::str::from_utf8(raw).ok()?;
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || raw.contains('\\')
        || raw.contains('\0')
    {
        return None;
    }

    let mut path = PathBuf::new();
    for part in raw.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn remove_git_worktree(project_root: &Path, root: &Path) -> std::io::Result<()> {
    let output = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(root)
        .current_dir(project_root)
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "could not remove git worktree {}\nstderr:\n{}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    )))
}

fn reset_copied_workspace(
    project_root: &Path,
    workspace_root: &Path,
    respect_ignores: bool,
) -> std::io::Result<()> {
    let preserved_dirs = preserve_workspace_dirs(workspace_root)?;
    if workspace_root.exists() {
        fs::remove_dir_all(workspace_root)?;
    }
    fs::create_dir(workspace_root)?;
    restore_workspace_dirs(workspace_root, preserved_dirs)?;
    populate_workspace(project_root, workspace_root, respect_ignores)
}

fn reset_git_worktree(
    project_root: &Path,
    workspace_root: &Path,
    overlay: &GitWorktreeOverlay,
) -> std::io::Result<()> {
    run_git_workspace_command(workspace_root, &["reset", "--hard", "--quiet", "HEAD"])?;

    let mut clean_args = vec!["clean", "-ffdx", "--quiet"];
    for preserved in PRESERVED_WORKSPACE_DIRS {
        clean_args.push("-e");
        clean_args.push(preserved);
    }
    run_git_workspace_command(workspace_root, &clean_args)?;
    overlay.apply(project_root, workspace_root)
}

fn run_git_workspace_command(workspace_root: &Path, args: &[&str]) -> std::io::Result<()> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(std::io::Error::other(format!(
        "git {} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        workspace_root.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )))
}

fn remove_workspace_path(path: &Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn copy_overlay_file(
    project_root: &Path,
    workspace_root: &Path,
    relative: &Path,
) -> std::io::Result<()> {
    let source = project_root.join(relative);
    let destination = workspace_root.join(relative);
    if !source.is_file() {
        remove_workspace_path(&destination)?;
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

/// Move preserved dirs to sibling stashes before deleting the workspace.
///
/// The sibling stash location keeps rename on the same filesystem; removing an
/// existing stash is intentional recovery from an interrupted previous reset.
fn preserve_workspace_dirs(workspace_root: &Path) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    let Some(parent) = workspace_root.parent() else {
        return Ok(Vec::new());
    };

    let mut preserved = Vec::new();
    for name in PRESERVED_WORKSPACE_DIRS {
        let source = workspace_root.join(name);
        if !source.exists() {
            continue;
        }

        let stash = parent.join(format!(".togi-preserved-{name}"));
        if stash.exists() {
            fs::remove_dir_all(&stash)?;
        }
        fs::rename(&source, &stash)?;
        preserved.push((PathBuf::from(name), stash));
    }
    Ok(preserved)
}

fn restore_workspace_dirs(
    workspace_root: &Path,
    preserved_dirs: Vec<(PathBuf, PathBuf)>,
) -> std::io::Result<()> {
    for (relative, stash) in preserved_dirs {
        let destination = workspace_root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(stash, destination)?;
    }
    Ok(())
}

fn populate_workspace(
    project_root: &Path,
    root: &Path,
    respect_ignores: bool,
) -> std::io::Result<()> {
    let project_root_for_filter = project_root.to_path_buf();

    let mut builder = ignore::WalkBuilder::new(project_root);
    builder
        .hidden(false)
        .ignore(respect_ignores)
        .git_ignore(respect_ignores)
        .git_exclude(respect_ignores)
        .git_global(respect_ignores)
        .parents(respect_ignores)
        .filter_entry(move |entry| {
            should_copy_workspace_entry(&project_root_for_filter, entry.path())
        });

    for entry in builder.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                return Err(std::io::Error::other(err));
            }
        };
        let path = entry.path();
        if path == project_root {
            continue;
        }
        let relative = match path.strip_prefix(project_root) {
            Ok(relative) => relative,
            Err(_) => continue,
        };

        let dest = root.join(relative);
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            fs::create_dir_all(&dest)?;
        } else if entry.file_type().is_some_and(|ft| ft.is_file()) {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(path, dest)?;
        }
    }

    Ok(())
}

pub(crate) struct WorkspacePool {
    slots: Arc<Vec<WorkspaceCopy>>,
    free_slots: Arc<(Mutex<VecDeque<usize>>, Condvar)>,
    dirty_slots: Arc<Mutex<Vec<bool>>>,
}

impl WorkspacePool {
    pub(crate) fn new(project_root: &Path, slots: usize) -> std::io::Result<Self> {
        Self::new_with_options(project_root, slots, true)
    }

    fn new_with_options(
        project_root: &Path,
        slots: usize,
        respect_ignores: bool,
    ) -> std::io::Result<Self> {
        let slots = slots.max(1);
        let mut copies = Vec::with_capacity(slots);
        for _ in 0..slots {
            let copy = if respect_ignores {
                copy_workspace(project_root)?
            } else {
                copy_workspace_with_options(project_root, false)?
            };
            copies.push(copy);
        }

        let free_slots = (0..slots).collect();
        let dirty_slots = vec![false; slots];

        Ok(Self {
            slots: Arc::new(copies),
            free_slots: Arc::new((Mutex::new(free_slots), Condvar::new())),
            dirty_slots: Arc::new(Mutex::new(dirty_slots)),
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn acquire(&self) -> WorkspaceSlot {
        let (lock, cvar) = &*self.free_slots;
        let mut free_slots = lock.lock().expect("workspace free-list mutex poisoned");
        let index = loop {
            if let Some(index) = free_slots.pop_front() {
                break index;
            }
            free_slots = cvar
                .wait(free_slots)
                .expect("workspace free-list mutex poisoned");
        };
        let needs_reset = {
            let mut dirty_slots = self
                .dirty_slots
                .lock()
                .expect("workspace dirty-list mutex poisoned");
            let needs_reset = dirty_slots[index];
            dirty_slots[index] = true;
            needs_reset
        };

        WorkspaceSlot {
            slots: self.slots.clone(),
            free_slots: self.free_slots.clone(),
            index,
            needs_reset,
        }
    }
}

pub(crate) struct WorkspaceSlot {
    slots: Arc<Vec<WorkspaceCopy>>,
    free_slots: Arc<(Mutex<VecDeque<usize>>, Condvar)>,
    index: usize,
    needs_reset: bool,
}

impl WorkspaceSlot {
    pub(crate) fn root(&self) -> &Path {
        self.slots[self.index].root()
    }

    fn needs_reset(&self) -> bool {
        self.needs_reset
    }

    fn reset(&self, project_root: &Path, respect_ignores: bool) -> std::io::Result<()> {
        self.slots[self.index].reset(project_root, respect_ignores)
    }
}

impl Drop for WorkspaceSlot {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.free_slots;
        lock.lock()
            .expect("workspace free-list mutex poisoned")
            .push_back(self.index);
        cvar.notify_one();
    }
}

struct QueuedMutation {
    index: usize,
    mutation: Mutation,
}

#[derive(Debug)]
struct MutationRunRecord {
    mutation: Mutation,
    result: MutationResult,
    build_error_diagnostic: Option<BuildErrorDiagnostic>,
}

impl MutationRunRecord {
    fn new(
        mutation: Mutation,
        result: MutationResult,
        build_error_diagnostic: Option<BuildErrorDiagnostic>,
    ) -> Self {
        Self {
            mutation,
            result,
            build_error_diagnostic,
        }
    }
}

#[derive(Default)]
struct SchemataRunSummary {
    fast_path: usize,
    fallback: usize,
    fallback_reasons: BTreeMap<String, usize>,
}

impl SchemataRunSummary {
    fn record_fallback(&mut self, reason: &str) {
        self.record_fallbacks(reason, 1);
    }

    fn record_fallbacks(&mut self, reason: &str, count: usize) {
        self.fallback += count;
        *self.fallback_reasons.entry(reason.to_string()).or_default() += count;
    }

    fn into_report(self) -> SchemataReport {
        SchemataReport {
            fast_path: self.fast_path,
            fallback: self.fallback,
            fallback_reasons: self
                .fallback_reasons
                .into_iter()
                .map(|(reason, count)| SchemataFallbackReasonCount { reason, count })
                .collect(),
        }
    }
}

#[derive(Default)]
struct EarlyStopCounts {
    completed: usize,
    killed: usize,
    survived: usize,
    build_errors: usize,
}

struct EarlyStopState {
    config: EarlyStopConfig,
    planned_total: usize,
    stopped: AtomicBool,
    reason: Mutex<Option<String>>,
    counts: Mutex<EarlyStopCounts>,
}

impl EarlyStopState {
    fn new(config: EarlyStopConfig, planned_total: usize) -> Self {
        Self {
            config,
            planned_total,
            stopped: AtomicBool::new(false),
            reason: Mutex::new(None),
            counts: Mutex::new(EarlyStopCounts::default()),
        }
    }

    fn for_config(config: EarlyStopConfig, planned_total: usize) -> Option<Arc<Self>> {
        config
            .is_enabled()
            .then(|| Arc::new(Self::new(config, planned_total)))
    }

    fn should_stop(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn record(&self, result: MutationResult) {
        if self.should_stop() {
            return;
        }

        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        counts.completed += 1;
        match result {
            MutationResult::Killed => counts.killed += 1,
            MutationResult::Survived => counts.survived += 1,
            MutationResult::Timeout => {}
            MutationResult::BuildError => counts.build_errors += 1,
        }

        let remaining = self.planned_total.saturating_sub(counts.completed);
        if remaining == 0 {
            return;
        }

        if let Some(max_survivors) = self.config.max_survivors {
            if counts.survived >= max_survivors {
                self.stop(format!(
                    "--max-survivors {max_survivors} reached after {} completed mutation{}",
                    counts.completed,
                    if counts.completed == 1 { "" } else { "s" }
                ));
                return;
            }
        }

        if let Some(threshold) = self.config.fail_under {
            let tested = counts.completed.saturating_sub(counts.build_errors);
            let best_tested = tested + remaining;
            let best_killed = counts.killed + remaining;
            let best_score = if best_tested > 0 {
                (best_killed as f64 / best_tested as f64) * 100.0
            } else if self.planned_total == 0 {
                100.0
            } else {
                0.0
            };
            if best_score < threshold {
                self.stop(format!(
                    "--fail-under {threshold:.1} cannot be reached; best possible score is {best_score:.1}%"
                ));
            }
        }
    }

    fn stop(&self, reason: String) {
        let mut current = self
            .reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.is_none() {
            *current = Some(reason);
            self.stopped.store(true, Ordering::Release);
        }
    }
}

struct RunShared<'a> {
    workspace_pool: &'a WorkspacePool,
    project_root: &'a Path,
    commands: &'a CommandConfig,
    build_command: &'a [String],
    build_command_explicit: bool,
    env: &'a HashMap<String, String>,
    total: usize,
    verbose: bool,
    is_tty: bool,
    show_output: bool,
    counter: &'a AtomicUsize,
    cancelled: &'a AtomicBool,
    early_stop: Option<&'a Arc<EarlyStopState>>,
    respect_workspace_ignores: bool,
    cache_context_fingerprint: u64,
    test_context_index: &'a TestContextIndex,
    source_contents: &'a SourceContentCache,
    history: Option<&'a cache::IncrementalHistoryStore>,
    force_rerun: bool,
}

fn should_stop_early(early_stop: &Option<Arc<EarlyStopState>>) -> bool {
    early_stop.as_ref().is_some_and(|state| state.should_stop())
}

fn record_early_stop(shared: &RunShared<'_>, result: MutationResult) {
    if let Some(early_stop) = shared.early_stop {
        early_stop.record(result);
    }
}

fn incremental_history_query(
    project_root: &Path,
    mutation: &Mutation,
    source_content: &[u8],
    command_context: &str,
    relevant_test_hash: u64,
) -> cache::IncrementalHistoryQuery {
    cache::IncrementalHistoryQuery {
        mutation_identity: cache_identity(project_root, mutation),
        mutation_description: mutation.description.clone(),
        source_hash: cache::hash_bytes(source_content),
        command_hash: cache::hash_str(command_context),
        relevant_test_hash,
    }
}

fn record_incremental_history(
    history: Option<&cache::IncrementalHistoryStore>,
    query: Option<&cache::IncrementalHistoryQuery>,
    selected_tests: &[String],
    result: MutationResult,
    previous_killer: Option<String>,
) {
    let (Some(history), Some(query)) = (history, query) else {
        return;
    };
    let killer_test = if result == MutationResult::Killed {
        previous_killer
            .filter(|killer| selected_tests.iter().any(|test| test == killer))
            .or_else(|| (selected_tests.len() == 1).then(|| selected_tests[0].clone()))
    } else {
        None
    };
    history.record(cache::IncrementalHistoryEntry {
        mutation_identity: query.mutation_identity.clone(),
        mutation_description: query.mutation_description.clone(),
        result,
        source_hash: query.source_hash,
        command_hash: query.command_hash,
        relevant_test_hash: query.relevant_test_hash,
        covering_tests: selected_tests.to_vec(),
        killer_test,
    });
}

struct PreparedMutationRun {
    selected_test: SelectedTestCommand,
    previous_killer: Option<String>,
    history_query: Option<cache::IncrementalHistoryQuery>,
    cache_key: Option<CacheKey>,
}

struct PreparedMutationContext<'a> {
    commands: &'a CommandConfig,
    history: Option<&'a cache::IncrementalHistoryStore>,
    source_contents: &'a SourceContentCache,
    cache_context_fingerprint: u64,
    test_context_index: &'a TestContextIndex,
    env: &'a HashMap<String, String>,
}

impl PreparedMutationRun {
    fn new(project_root: &Path, mutation: &Mutation, context: PreparedMutationContext<'_>) -> Self {
        let selected_test = select_test_command_with_history(
            project_root,
            context.commands,
            mutation,
            context.history,
        );
        let mutation_identity = cache_identity(project_root, mutation);
        let command_ctx = selected_test.cache_context(
            &context.commands.build_command,
            context.commands.build_command_explicit,
            context.commands.sandbox_command.as_slice(),
            context.env,
        );
        let relevant_test_hash = context.test_context_index.fingerprint_for_tests(
            &selected_test.selected_tests,
            context.cache_context_fingerprint,
        );
        let previous_killer = context.history.and_then(|history| {
            history.preferred_killer_test(
                &mutation_identity,
                &mutation.description,
                &selected_test.selected_tests,
            )
        });
        let cache_ctx = format!(
            "{command_ctx};context={:016x}",
            context.cache_context_fingerprint
        );
        let source_content = context
            .source_contents
            .content_for(project_root, &mutation.file);
        let history_query = source_content.as_deref().map(|content| {
            incremental_history_query(
                project_root,
                mutation,
                content,
                &command_ctx,
                relevant_test_hash,
            )
        });
        let cache_key = source_content.as_ref().map(|content| {
            CacheKey::new(
                content,
                &mutation_identity,
                &mutation.description,
                &cache_ctx,
            )
        });

        Self {
            selected_test,
            previous_killer,
            history_query,
            cache_key,
        }
    }

    fn restore_result(
        &self,
        project_root: &Path,
        history: Option<&cache::IncrementalHistoryStore>,
        force_rerun: bool,
    ) -> Option<MutationResult> {
        if force_rerun {
            return None;
        }
        if let Some(ref key) = self.cache_key {
            if let Some(result) = cache::lookup(project_root, key) {
                self.record_history(history, result);
                return Some(result);
            }
        }
        if let (Some(history), Some(query)) = (history, self.history_query.as_ref()) {
            if let Some(result) = history.lookup(query) {
                if let Some(ref key) = self.cache_key {
                    cache::store(project_root, key, result);
                }
                self.record_history(Some(history), result);
                return Some(result);
            }
        }
        None
    }

    fn store_cache(&self, project_root: &Path, result: MutationResult) {
        if let Some(ref key) = self.cache_key {
            cache::store(project_root, key, result);
        }
    }

    fn record_history(
        &self,
        history: Option<&cache::IncrementalHistoryStore>,
        result: MutationResult,
    ) {
        record_incremental_history(
            history,
            self.history_query.as_ref(),
            &self.selected_test.selected_tests,
            result,
            self.previous_killer.clone(),
        );
    }
}

fn run_queued_mutation(
    queued: QueuedMutation,
    reservation: TestSlotReservation,
    shared: RunShared<'_>,
) -> Option<(usize, MutationRunRecord)> {
    let QueuedMutation { index, mutation } = queued;

    // Stop if cancelled (Ctrl+C).
    if shared.cancelled.load(Ordering::Relaxed) {
        return None;
    }

    let prepared = PreparedMutationRun::new(
        shared.project_root,
        &mutation,
        PreparedMutationContext {
            commands: shared.commands,
            history: shared.history,
            source_contents: shared.source_contents,
            cache_context_fingerprint: shared.cache_context_fingerprint,
            test_context_index: shared.test_context_index,
            env: shared.env,
        },
    );

    // Check exact cache and then structured history before acquiring a workspace slot.
    if let Some(result) =
        prepared.restore_result(shared.project_root, shared.history, shared.force_rerun)
    {
        reservation.release();
        record_progress(&shared, &mutation, result, None, true);
        record_early_stop(&shared, result);
        let diagnostic = cached_build_error_diagnostic(&mutation, "regular", result);
        return Some((index, MutationRunRecord::new(mutation, result, diagnostic)));
    }

    let outcome = {
        let workspace_slot = shared.workspace_pool.acquire();
        let workspace_root = workspace_slot.root().to_path_buf();
        if workspace_slot.needs_reset() {
            if let Err(e) =
                workspace_slot.reset(shared.project_root, shared.respect_workspace_ignores)
            {
                eprintln!(
                    "warning: could not reset isolated mutation workspace {}: {e}",
                    workspace_root.display()
                );
                reservation.release();
                record_progress(&shared, &mutation, MutationResult::BuildError, None, false);
                record_early_stop(&shared, MutationResult::BuildError);
                let diagnostic = BuildErrorDiagnostic::new(
                    mutation.id,
                    "regular",
                    "workspace_reset",
                    vec![],
                    format!(
                        "could not reset isolated mutation workspace {}: {e}",
                        workspace_root.display()
                    ),
                );
                return Some((
                    index,
                    MutationRunRecord::new(mutation, MutationResult::BuildError, Some(diagnostic)),
                ));
            }
        }
        let workspace_target =
            ResolvedMutation::new_for_execution(shared.project_root, &workspace_root, &mutation);
        run_single_mutation(
            &prepared.selected_test.argv,
            shared.commands.sandbox_command.as_slice(),
            BuildCommand {
                argv: shared.build_command,
                explicit: shared.build_command_explicit,
            },
            prepared.selected_test.timeout,
            &workspace_root,
            workspace_target,
            shared.show_output,
            shared.env,
            shared.cancelled,
        )
    };

    if outcome.cancelled {
        return None;
    }
    if outcome.result == MutationResult::BuildError {
        reservation.release();
    } else {
        reservation.commit();
    }

    prepared.store_cache(shared.project_root, outcome.result);
    prepared.record_history(shared.history, outcome.result);

    let result = outcome.result;
    record_progress(
        &shared,
        &mutation,
        result,
        outcome.test_output.as_deref(),
        false,
    );
    record_early_stop(&shared, result);
    let diagnostic = build_error_diagnostic_from_outcome(&mutation, "regular", &outcome);
    Some((index, MutationRunRecord::new(mutation, result, diagnostic)))
}

struct TestSlotReservation {
    counter: Option<Arc<AtomicUsize>>,
}

impl TestSlotReservation {
    fn try_reserve(max_tested: Option<usize>, counter: &Arc<AtomicUsize>) -> Option<Self> {
        let Some(max) = max_tested else {
            return Some(Self { counter: None });
        };

        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < max).then_some(current + 1)
            })
            .ok()
            .map(|_| Self {
                counter: Some(counter.clone()),
            })
    }

    fn commit(mut self) {
        self.counter = None;
    }

    fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if let Some(counter) = self.counter.take() {
            counter.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for TestSlotReservation {
    fn drop(&mut self) {
        self.release_inner();
    }
}

#[allow(clippy::manual_is_multiple_of)]
fn record_progress(
    shared: &RunShared<'_>,
    mutation: &Mutation,
    result: MutationResult,
    test_output: Option<&str>,
    cached: bool,
) {
    let n = shared.counter.fetch_add(1, Ordering::Relaxed) + 1;
    if shared.verbose {
        if cached {
            eprintln!(
                "  [{}/{}] \u{21bb} cached  {}:{} \u{2014} {}",
                n,
                shared.total,
                mutation.file.display(),
                mutation.line,
                mutation.operator
            );
        } else {
            let symbol = match result {
                MutationResult::Killed => "\u{2713} killed",
                MutationResult::Survived => "\u{2717} survived",
                MutationResult::Timeout => "⧖ timeout",
                MutationResult::BuildError => "⚠ build error",
            };
            eprintln!(
                "  [{}/{}] {}  {}:{} \u{2014} {}",
                n,
                shared.total,
                symbol,
                mutation.file.display(),
                mutation.line,
                mutation.operator
            );
        }
    } else if shared.is_tty {
        eprint!("\r  [{}/{}] testing mutations...", n, shared.total);
        let _ = std::io::stderr().flush();
    } else if n == shared.total || (shared.total >= 4 && n % (shared.total / 4) == 0) {
        eprintln!("  [{}/{}] testing mutations...", n, shared.total);
    }

    if shared.show_output && result == MutationResult::Survived {
        if let Some(output) = test_output {
            eprintln!(
                "  ┌─ test output for {}:{} ({})",
                mutation.file.display(),
                mutation.line,
                mutation.operator
            );
            for line in output.lines() {
                eprintln!("  │ {}", line);
            }
            eprintln!("  └─");
        }
    }
}

impl TestRunner {
    #[allow(clippy::manual_is_multiple_of)]
    pub fn run(&self, mutations: Vec<Mutation>) -> RunOutcome {
        let planned_total = mutations.len();
        let early_stop = EarlyStopState::for_config(self.early_stop.clone(), planned_total);
        self.run_regular_with_state(mutations, early_stop, planned_total)
    }

    pub fn run_with_schemata(&self, mutations: Vec<Mutation>) -> RunOutcome {
        let start = Instant::now();
        if mutations.is_empty() {
            return self.outcome_from_records(Vec::new(), start.elapsed());
        }
        let planned_total = mutations.len();
        let early_stop = EarlyStopState::for_config(self.early_stop.clone(), planned_total);

        let index_by_id: HashMap<u32, usize> = mutations
            .iter()
            .enumerate()
            .map(|(index, mutation)| (mutation.id, index))
            .collect();
        let plan = crate::schemata::plan(&self.project_root, mutations);
        let mut schema_by_language = HashMap::<String, Vec<crate::schemata::SchemaMutation>>::new();
        let mut fallback_mutations = Vec::new();
        let mut schemata_summary = SchemataRunSummary::default();

        for schema_mutation in plan.selected {
            match schema_mutation.mutation.language.as_str() {
                "c" | "cpp" | "go" | "java" | "rust" => {
                    schema_by_language
                        .entry(schema_mutation.mutation.language.clone())
                        .or_default()
                        .push(schema_mutation);
                }
                _ => {
                    schemata_summary.record_fallback("unsupported_runner");
                    fallback_mutations.push(schema_mutation.mutation);
                }
            }
        }
        for fallback in plan.fallback {
            schemata_summary.record_fallback(fallback.reason.as_str());
            fallback_mutations.push(fallback.mutation);
        }

        if schema_by_language.is_empty() {
            let mut outcome =
                self.run_regular_with_state(fallback_mutations, early_stop, planned_total);
            outcome.report.schemata = Some(schemata_summary.into_report());
            return outcome;
        }

        let mut all_records = Vec::new();
        for (language, schema_mutations) in schema_by_language {
            if should_stop_early(&early_stop) {
                break;
            }
            let mutation_count = schema_mutations.len();
            match self.run_schema_mutations(&language, &schema_mutations, early_stop.clone()) {
                Ok(records) => {
                    schemata_summary.fast_path += records.len();
                    all_records.extend(records);
                }
                Err(err) => {
                    eprintln!("warning: could not run {language} schemata: {err} — falling back");
                    schemata_summary.record_fallbacks("rewrite_error", mutation_count);
                    fallback_mutations.extend(
                        schema_mutations
                            .into_iter()
                            .map(|schema_mutation| schema_mutation.mutation),
                    );
                }
            }
        }

        if !self.cancelled.load(Ordering::Acquire)
            && !fallback_mutations.is_empty()
            && !should_stop_early(&early_stop)
        {
            let fallback =
                self.run_regular_with_state(fallback_mutations, early_stop.clone(), planned_total);
            all_records.extend(records_from_report(fallback.report));
        }

        all_records.sort_by_key(|record| {
            index_by_id
                .get(&record.mutation.id)
                .copied()
                .unwrap_or(usize::MAX)
        });
        let mut outcome = self.outcome_from_records_with_status(
            all_records,
            start.elapsed(),
            planned_total,
            early_stop.as_ref().and_then(|state| state.reason()),
        );
        outcome.report.schemata = Some(schemata_summary.into_report());
        outcome
    }

    #[allow(clippy::manual_is_multiple_of)]
    fn run_regular_with_state(
        &self,
        mutations: Vec<Mutation>,
        early_stop: Option<Arc<EarlyStopState>>,
        planned_total: usize,
    ) -> RunOutcome {
        let start = Instant::now();
        let total = mutations.len();
        if total == 0 {
            return self.outcome_from_records_with_status(
                Vec::new(),
                start.elapsed(),
                planned_total,
                early_stop.as_ref().and_then(|state| state.reason()),
            );
        }
        if self.cancelled.load(Ordering::Acquire) {
            return self.outcome_from_records_with_status(
                Vec::new(),
                start.elapsed(),
                planned_total,
                early_stop.as_ref().and_then(|state| state.reason()),
            );
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let tested_counter = Arc::new(AtomicUsize::new(0));
        let verbose = self.verbose;
        let is_tty = std::io::stderr().is_terminal();
        let workspace_slots = workspace_pool_slot_count(self.parallelism, total);

        let workspace_pool_result = if self.respect_workspace_ignores {
            WorkspacePool::new(&self.project_root, workspace_slots)
        } else {
            WorkspacePool::new_with_options(&self.project_root, workspace_slots, false)
        };
        let workspace_pool = match workspace_pool_result {
            Ok(pool) => Arc::new(pool),
            Err(e) => {
                eprintln!("warning: could not create isolated mutation workspaces: {e}");
                let results = mutations
                    .into_iter()
                    .map(|mutation| {
                        let diagnostic = BuildErrorDiagnostic::new(
                            mutation.id,
                            "regular",
                            "workspace_pool",
                            vec![],
                            format!("could not create isolated mutation workspaces: {e}"),
                        );
                        MutationRunRecord::new(
                            mutation,
                            MutationResult::BuildError,
                            Some(diagnostic),
                        )
                    })
                    .collect();
                return self.outcome_from_records(results, start.elapsed());
            }
        };

        let source_contents = SourceContentCache::default();
        let project_root = Arc::new(self.project_root.clone());
        let build_command = Arc::new(self.commands.build_command.clone());
        let build_command_explicit = self.commands.build_command_explicit;
        let queue = Arc::new(Mutex::new(
            mutations.into_iter().enumerate().collect::<VecDeque<_>>(),
        ));
        let results = Arc::new(Mutex::new(Vec::new()));
        let worker_count = workspace_pool.len().min(total).max(1);
        let cache_context_hash = cache_context_fingerprint(&self.project_root);
        let test_context_index = TestContextIndex::build(&self.project_root);
        let history = self
            .incremental_history
            .then(|| cache::IncrementalHistoryStore::load(&self.project_root));

        thread::scope(|scope| {
            for _ in 0..worker_count {
                let queue = queue.clone();
                let results = results.clone();
                let workspace_pool = workspace_pool.clone();
                let project_root = project_root.clone();
                let build_command = build_command.clone();
                let counter = counter.clone();
                let tested_counter = tested_counter.clone();
                let cancelled = self.cancelled.clone();
                let commands = &self.commands;
                let env = &self.env;
                let max_tested = self.max_tested;
                let show_output = self.show_output;
                let source_contents = &source_contents;
                let test_context_index = &test_context_index;
                let history = history.as_ref();
                let force_rerun = self.force_rerun;
                let early_stop = early_stop.clone();

                scope.spawn(move || {
                    loop {
                        if cancelled.load(Ordering::Relaxed) || should_stop_early(&early_stop) {
                            break;
                        }

                        let Some(reservation) =
                            TestSlotReservation::try_reserve(max_tested, &tested_counter)
                        else {
                            break;
                        };
                        if should_stop_early(&early_stop) {
                            reservation.release();
                            break;
                        }
                        let next = match queue.lock() {
                            Ok(mut queue) => queue.pop_front(),
                            Err(_) => {
                                eprintln!("warning: mutation queue mutex poisoned");
                                break;
                            }
                        };
                        let Some((index, mutation)) = next else {
                            break;
                        };

                        let outcome = catch_unwind(PanicBoundary(|| {
                            run_queued_mutation(
                                QueuedMutation { index, mutation },
                                reservation,
                                RunShared {
                                    workspace_pool: workspace_pool.as_ref(),
                                    project_root: project_root.as_ref().as_path(),
                                    commands,
                                    build_command: build_command.as_ref().as_slice(),
                                    build_command_explicit,
                                    env,
                                    total,
                                    verbose,
                                    is_tty,
                                    show_output,
                                    counter: &counter,
                                    cancelled: &cancelled,
                                    early_stop: early_stop.as_ref(),
                                    respect_workspace_ignores: self.respect_workspace_ignores,
                                    cache_context_fingerprint: cache_context_hash,
                                    test_context_index,
                                    source_contents,
                                    history,
                                    force_rerun,
                                },
                            )
                        }));

                        match outcome {
                            Ok(Some(result)) => match results.lock() {
                                Ok(mut results) => results.push(result),
                                Err(_) => {
                                    eprintln!("warning: mutation results mutex poisoned");
                                    break;
                                }
                            },
                            Ok(None) => {}
                            Err(_) => eprintln!("warning: mutation task panicked"),
                        }
                    }
                });
            }
        });

        let mut indexed_results = Arc::try_unwrap(results)
            .expect("all result handles should be dropped")
            .into_inner()
            .expect("mutation results mutex poisoned");
        indexed_results.sort_by_key(|(index, _)| *index);
        let all_records = indexed_results
            .into_iter()
            .map(|(_, record)| record)
            .collect();

        // Clear progress line on TTY
        if !verbose && is_tty {
            eprint!("\r                                        \r");
            let _ = std::io::stderr().flush();
        }

        self.outcome_from_records_with_status(
            all_records,
            start.elapsed(),
            planned_total,
            early_stop.as_ref().and_then(|state| state.reason()),
        )
    }

    fn run_schema_mutations(
        &self,
        language: &str,
        schema_mutations: &[crate::schemata::SchemaMutation],
        early_stop: Option<Arc<EarlyStopState>>,
    ) -> Result<Vec<MutationRunRecord>, crate::schemata::SchemaRewriteError> {
        let rewrites = match language {
            "c" => crate::schemata::rewrite_c_files(&self.project_root, schema_mutations)?,
            "cpp" => crate::schemata::rewrite_cpp_files(&self.project_root, schema_mutations)?,
            "go" => crate::schemata::rewrite_go_files(&self.project_root, schema_mutations)?,
            "java" => crate::schemata::rewrite_java_files(&self.project_root, schema_mutations)?,
            "rust" => crate::schemata::rewrite_rust_files(&self.project_root, schema_mutations)?,
            _ => {
                return Err(crate::schemata::SchemaRewriteError::new(format!(
                    "{language} schemata execution is not available"
                )));
            }
        };
        let workspace =
            copy_workspace_with_options(&self.project_root, self.respect_workspace_ignores)
                .map_err(|e| {
                    crate::schemata::SchemaRewriteError::new(format!(
                        "could not create schema workspace: {e}"
                    ))
                })?;

        let mut results = Vec::with_capacity(schema_mutations.len());
        let tested_counter = Arc::new(AtomicUsize::new(0));
        let source_contents = SourceContentCache::default();
        let cache_context_hash = cache_context_fingerprint(&self.project_root);
        let test_context_index = TestContextIndex::build(&self.project_root);
        let history = self
            .incremental_history
            .then(|| cache::IncrementalHistoryStore::load(&self.project_root));
        let mut workspace_needs_reset = false;
        for schema_mutation in schema_mutations {
            if self.cancelled.load(Ordering::Acquire) || should_stop_early(&early_stop) {
                break;
            }
            let Some(reservation) =
                TestSlotReservation::try_reserve(self.max_tested, &tested_counter)
            else {
                break;
            };
            if should_stop_early(&early_stop) {
                reservation.release();
                break;
            }
            let mutation = &schema_mutation.mutation;
            let mut env = self.env.clone();
            env.insert("TOGI_MUTANT".to_string(), mutation.id.to_string());
            // Cache keys must not embed the per-mutant TOGI_MUTANT id: ids are
            // reassigned whenever the mutation set changes, which would orphan
            // every cached verdict from earlier runs.
            let prepared = PreparedMutationRun::new(
                &self.project_root,
                mutation,
                PreparedMutationContext {
                    commands: &self.commands,
                    history: history.as_ref(),
                    source_contents: &source_contents,
                    cache_context_fingerprint: cache_context_hash,
                    test_context_index: &test_context_index,
                    env: &self.env,
                },
            );
            let argv = if language == "go" {
                force_go_no_test_cache(prepared.selected_test.argv.clone())
            } else {
                prepared.selected_test.argv.clone()
            };
            if let Some(result) =
                prepared.restore_result(&self.project_root, history.as_ref(), self.force_rerun)
            {
                reservation.release();
                if let Some(early_stop) = &early_stop {
                    early_stop.record(result);
                }
                if self.verbose {
                    eprintln!(
                        "  [schema] ↻ cached  {}:{} — {}",
                        mutation.file.display(),
                        mutation.line,
                        mutation.operator
                    );
                }
                let diagnostic = cached_build_error_diagnostic(mutation, "schemata", result);
                results.push(MutationRunRecord::new(mutation.clone(), result, diagnostic));
                continue;
            }

            let mut cacheable = true;
            let outcome = if workspace_needs_reset {
                if let Err(e) = workspace.reset(&self.project_root, self.respect_workspace_ignores)
                {
                    cacheable = false;
                    MutationOutcome::build_error_with(
                        "schema_workspace_reset",
                        vec![],
                        format!(
                            "could not reset schema workspace {}: {e}",
                            workspace.root().display()
                        ),
                    )
                } else {
                    workspace_needs_reset = true;
                    run_schema_workspace_mutation(
                        self,
                        workspace.root(),
                        &rewrites,
                        &argv,
                        prepared.selected_test.timeout,
                        &env,
                        &mut cacheable,
                    )
                }
            } else {
                workspace_needs_reset = true;
                run_schema_workspace_mutation(
                    self,
                    workspace.root(),
                    &rewrites,
                    &argv,
                    prepared.selected_test.timeout,
                    &env,
                    &mut cacheable,
                )
            };
            if outcome.cancelled {
                break;
            }
            if outcome.result == MutationResult::BuildError {
                reservation.release();
            } else {
                reservation.commit();
            }
            if let Some(early_stop) = &early_stop {
                early_stop.record(outcome.result);
            }
            if cacheable {
                prepared.store_cache(&self.project_root, outcome.result);
            }
            prepared.record_history(history.as_ref(), outcome.result);
            if self.verbose {
                let symbol = match outcome.result {
                    MutationResult::Killed => "✓ killed",
                    MutationResult::Survived => "✗ survived",
                    MutationResult::Timeout => "⧖ timeout",
                    MutationResult::BuildError => "⚠ build error",
                };
                eprintln!(
                    "  [schema] {}  {}:{} — {}",
                    symbol,
                    mutation.file.display(),
                    mutation.line,
                    mutation.operator
                );
            }
            if self.show_output && outcome.result == MutationResult::Survived {
                if let Some(output) = outcome.test_output.as_deref() {
                    eprintln!(
                        "  ┌─ test output for {}:{} ({})",
                        mutation.file.display(),
                        mutation.line,
                        mutation.operator
                    );
                    for line in output.lines() {
                        eprintln!("  │ {}", line);
                    }
                    eprintln!("  └─");
                }
            }
            let diagnostic = build_error_diagnostic_from_outcome(mutation, "schemata", &outcome);
            results.push(MutationRunRecord::new(
                mutation.clone(),
                outcome.result,
                diagnostic,
            ));
        }

        Ok(results)
    }

    fn outcome_from_records(
        &self,
        all_records: Vec<MutationRunRecord>,
        duration: Duration,
    ) -> RunOutcome {
        let planned_total = all_records.len();
        self.outcome_from_records_with_status(all_records, duration, planned_total, None)
    }

    fn outcome_from_records_with_status(
        &self,
        all_records: Vec<MutationRunRecord>,
        duration: Duration,
        planned_total: usize,
        early_stop_reason: Option<String>,
    ) -> RunOutcome {
        RunOutcome {
            report: self.report_from_records(
                all_records,
                duration,
                planned_total,
                early_stop_reason,
            ),
            cancelled: self.cancelled.load(Ordering::Acquire),
        }
    }

    fn report_from_records(
        &self,
        all_records: Vec<MutationRunRecord>,
        duration: Duration,
        planned_total: usize,
        early_stop_reason: Option<String>,
    ) -> MutationReport {
        let mut results = Vec::with_capacity(all_records.len());
        let mut build_error_diagnostics = Vec::new();
        let mut total = 0;
        let mut killed = 0;
        let mut survived = 0;
        let mut timeout_count = 0;
        let mut build_errors = 0;

        for record in all_records {
            total += 1;
            match record.result {
                MutationResult::Killed => killed += 1,
                MutationResult::Survived => survived += 1,
                MutationResult::Timeout => timeout_count += 1,
                MutationResult::BuildError => build_errors += 1,
            }
            if let Some(diagnostic) = record.build_error_diagnostic {
                build_error_diagnostics.push(diagnostic);
            }
            results.push((record.mutation, record.result));
        }

        MutationReport {
            results,
            build_error_diagnostics,
            schemata: None,
            baseline_timing: None,
            duration,
            test_command: if self.commands.language_commands.is_empty()
                && self.commands.project_commands.is_empty()
            {
                Some(sandboxed_command(
                    &self.commands.sandbox_command,
                    &self.commands.command,
                ))
            } else {
                None
            },
            build_command: if self.commands.build_command_explicit {
                sandboxed_command(&self.commands.sandbox_command, &self.commands.build_command)
            } else {
                vec![]
            },
            planned_total,
            early_stop_reason,
            total,
            killed,
            survived,
            timeout: timeout_count,
            build_errors,
        }
    }
}

fn apply_schema_rewrites_to_workspace(
    project_root: &Path,
    workspace_root: &Path,
    rewrites: &[crate::schemata::SchemaFileRewrite],
) -> Result<(), crate::schemata::SchemaRewriteError> {
    for rewrite in rewrites {
        let relative = project_relative_path(project_root, &rewrite.file).map_err(|_| {
            crate::schemata::SchemaRewriteError::new(format!(
                "could not resolve rewritten file {}",
                rewrite.file.display()
            ))
        })?;
        let target =
            validate_and_resolve_mutation_path(workspace_root, &relative).map_err(|_| {
                crate::schemata::SchemaRewriteError::new(format!(
                    "rewritten file {} is not in schema workspace",
                    rewrite.file.display()
                ))
            })?;
        write_workspace_file(&target, &rewrite.content).map_err(|e| {
            crate::schemata::SchemaRewriteError::new(format!(
                "could not write schema file {}: {e}",
                target.display()
            ))
        })?;
    }

    Ok(())
}

fn run_schema_workspace_mutation(
    runner: &TestRunner,
    workspace_root: &Path,
    rewrites: &[crate::schemata::SchemaFileRewrite],
    argv: &[String],
    timeout: Duration,
    env: &HashMap<String, String>,
    cacheable: &mut bool,
) -> MutationOutcome {
    if let Err(e) =
        apply_schema_rewrites_to_workspace(&runner.project_root, workspace_root, rewrites)
    {
        *cacheable = false;
        return MutationOutcome::build_error_with(
            "schema_rewrite",
            vec![],
            format!("could not apply schema rewrites: {e}"),
        );
    }

    if runner.commands.build_command_explicit && !runner.commands.build_command.is_empty() {
        let build = run_command(
            &runner.commands.build_command,
            &runner.commands.sandbox_command,
            workspace_root,
            runner.commands.timeout,
            true,
            &runner.env,
            &runner.cancelled,
        );
        if build.cancelled {
            return build;
        }
        if build.result != MutationResult::Survived {
            return MutationOutcome::build_error_with(
                "schema_build",
                runner.commands.build_command.clone(),
                build_error_message_from_outcome(
                    "schema build command",
                    &runner.commands.build_command,
                    workspace_root,
                    &build,
                ),
            );
        }
    }

    run_command(
        argv,
        &runner.commands.sandbox_command,
        workspace_root,
        timeout,
        runner.show_output,
        env,
        &runner.cancelled,
    )
}

fn records_from_report(report: MutationReport) -> Vec<MutationRunRecord> {
    let mut diagnostics: HashMap<u32, BuildErrorDiagnostic> = report
        .build_error_diagnostics
        .into_iter()
        .map(|diagnostic| (diagnostic.mutation_id, diagnostic))
        .collect();
    report
        .results
        .into_iter()
        .map(|(mutation, result)| {
            let diagnostic = diagnostics.remove(&mutation.id);
            MutationRunRecord::new(mutation, result, diagnostic)
        })
        .collect()
}

fn workspace_pool_slot_count(parallelism: usize, total: usize) -> usize {
    debug_assert!(total > 0);
    parallelism.max(1).min(total)
}

struct MutationOutcome {
    result: MutationResult,
    test_output: Option<String>,
    build_error_detail: Option<BuildErrorDetail>,
    cancelled: bool,
}

struct BuildErrorDetail {
    phase: String,
    command: Vec<String>,
    message: String,
}

impl BuildErrorDetail {
    fn new(phase: impl Into<String>, command: Vec<String>, message: impl Into<String>) -> Self {
        Self {
            phase: phase.into(),
            command,
            message: message.into(),
        }
    }
}

impl MutationOutcome {
    fn new(result: MutationResult, test_output: Option<String>) -> Self {
        Self {
            result,
            test_output,
            build_error_detail: None,
            cancelled: false,
        }
    }

    fn build_error_with(
        phase: impl Into<String>,
        command: Vec<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            result: MutationResult::BuildError,
            test_output: None,
            build_error_detail: Some(BuildErrorDetail::new(phase, command, message)),
            cancelled: false,
        }
    }

    fn cancelled() -> Self {
        Self {
            result: MutationResult::BuildError,
            test_output: None,
            build_error_detail: None,
            cancelled: true,
        }
    }
}

struct BuildCommand<'a> {
    argv: &'a [String],
    explicit: bool,
}

/// Cache entries currently persist only [`MutationResult`], so
/// `cached_build_error_diagnostic` intentionally groups cached build errors
/// under one synthetic [`BuildErrorDiagnostic::new`] bucket. Persist the
/// original phase/fingerprint with cached results to restore full fidelity.
fn cached_build_error_diagnostic(
    mutation: &Mutation,
    runner: &str,
    result: MutationResult,
) -> Option<BuildErrorDiagnostic> {
    (result == MutationResult::BuildError).then(|| {
        BuildErrorDiagnostic::new(
            mutation.id,
            runner,
            "cache",
            vec![],
            "build error result restored from cache; diagnostic output unavailable",
        )
    })
}

fn build_error_diagnostic_from_outcome(
    mutation: &Mutation,
    runner: &str,
    outcome: &MutationOutcome,
) -> Option<BuildErrorDiagnostic> {
    if outcome.result != MutationResult::BuildError {
        return None;
    }
    let Some(detail) = outcome.build_error_detail.as_ref() else {
        return Some(BuildErrorDiagnostic::new(
            mutation.id,
            runner,
            "unknown",
            vec![],
            "build error diagnostic unavailable",
        ));
    };
    Some(BuildErrorDiagnostic::new(
        mutation.id,
        runner,
        detail.phase.clone(),
        detail.command.clone(),
        detail.message.clone(),
    ))
}

fn build_error_message_from_outcome(
    context: &str,
    command: &[String],
    cwd: &Path,
    outcome: &MutationOutcome,
) -> String {
    if let Some(output) = outcome.test_output.as_deref() {
        if !output.trim().is_empty() {
            return output.to_string();
        }
    }
    if let Some(detail) = outcome.build_error_detail.as_ref() {
        if !detail.message.trim().is_empty() {
            return detail.message.clone();
        }
    }
    let command = if command.is_empty() {
        "<empty>".to_string()
    } else {
        command.join(" ")
    };
    format!(
        "{context} returned {}; command: {command}; cwd: {}",
        outcome.result,
        cwd.display()
    )
}

fn mutation_file_path(project_root: &Path, mutation_file: &Path) -> PathBuf {
    if mutation_file.is_absolute() {
        mutation_file.to_path_buf()
    } else {
        project_root.join(mutation_file)
    }
}

fn project_relative_path(project_root: &Path, file: &Path) -> Result<PathBuf, ()> {
    if file.is_absolute() {
        let canonical = file.canonicalize().map_err(|_| ())?;
        let root = project_root.canonicalize().map_err(|_| ())?;
        canonical
            .strip_prefix(root)
            .map(PathBuf::from)
            .map_err(|_| ())
    } else {
        Ok(file.to_path_buf())
    }
}

fn force_go_no_test_cache(mut argv: Vec<String>) -> Vec<String> {
    if argv.len() < 2 || argv[0] != "go" || argv[1] != "test" {
        return argv;
    }
    if argv
        .iter()
        .skip(2)
        .any(|arg| arg == "-count" || arg.starts_with("-count="))
    {
        return argv;
    }
    argv.insert(2, "-count=1".to_string());
    argv
}

fn cache_identity(project_root: &Path, mutation: &Mutation) -> String {
    format!(
        "{}:{}..{}:{}:{}=>{}",
        normalized_cache_path(project_root, &mutation.file),
        mutation.byte_range.start,
        mutation.byte_range.end,
        mutation.operator,
        mutation.original,
        mutation.replacement
    )
}

fn normalized_cache_path(project_root: &Path, mutation_file: &Path) -> String {
    let relative = if mutation_file.is_absolute() {
        mutation_file
            .canonicalize()
            .ok()
            .and_then(|path| {
                project_root
                    .canonicalize()
                    .ok()
                    .and_then(|root| path.strip_prefix(root).ok().map(PathBuf::from))
            })
            .unwrap_or_else(|| mutation_file.to_path_buf())
    } else {
        mutation_file.to_path_buf()
    };

    relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_and_resolve_mutation_path(
    project_root: &Path,
    mutation_file: &Path,
) -> Result<PathBuf, ()> {
    let file_path = mutation_file_path(project_root, mutation_file);

    let canonical = match file_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "warning: path traversal blocked: cannot resolve {}: {e}",
                mutation_file.display()
            );
            return Err(());
        }
    };
    let root = match project_root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("warning: path traversal blocked: cannot resolve project root: {e}");
            return Err(());
        }
    };
    if !canonical.starts_with(&root) {
        eprintln!(
            "warning: path traversal blocked: {} escapes project root",
            mutation_file.display()
        );
        return Err(());
    }

    Ok(file_path)
}

struct ResolvedMutation<'a> {
    mutation: &'a Mutation,
    file_path: Result<PathBuf, ()>,
}

impl<'a> ResolvedMutation<'a> {
    #[cfg(test)]
    fn new(project_root: &Path, mutation: &'a Mutation) -> Self {
        Self {
            mutation,
            file_path: validate_and_resolve_mutation_path(project_root, &mutation.file),
        }
    }

    fn new_for_execution(
        original_root: &Path,
        execution_root: &Path,
        mutation: &'a Mutation,
    ) -> Self {
        let file_path = if mutation.file.is_absolute() {
            mutation
                .file
                .canonicalize()
                .ok()
                .and_then(|canonical| {
                    original_root
                        .canonicalize()
                        .ok()
                        .and_then(|root| canonical.strip_prefix(root).ok().map(PathBuf::from))
                })
                .and_then(|relative| {
                    validate_and_resolve_mutation_path(execution_root, &relative).ok()
                })
                .ok_or(())
        } else {
            validate_and_resolve_mutation_path(execution_root, &mutation.file)
        };

        Self {
            mutation,
            file_path,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_single_mutation(
    command: &[String],
    sandbox_command: &[String],
    build_command: BuildCommand<'_>,
    timeout: Duration,
    project_root: &Path,
    target: ResolvedMutation<'_>,
    capture_output: bool,
    env: &HashMap<String, String>,
    cancelled: &AtomicBool,
) -> MutationOutcome {
    if cancelled.load(Ordering::Acquire) {
        return MutationOutcome::cancelled();
    }

    let mutation = target.mutation;
    let file_path = match target.file_path {
        Ok(path) => path,
        Err(()) => {
            return MutationOutcome::build_error_with(
                "resolve_mutation_path",
                vec![],
                format!(
                    "could not resolve mutation path {} in workspace {}",
                    mutation.file.display(),
                    project_root.display()
                ),
            );
        }
    };

    // Read original content
    let original = match std::fs::read(&file_path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("warning: could not read {}: {e}", file_path.display());
            return MutationOutcome::build_error_with(
                "read_source",
                vec![],
                format!("could not read {}: {e}", file_path.display()),
            );
        }
    };

    // Set up file guard for guaranteed restoration
    let _guard = FileGuard {
        path: file_path.clone(),
        original: original.clone(),
    };

    // Apply mutation
    let mut mutated = original.clone();
    let range = mutation.byte_range.clone();
    if range.start > range.end || range.end > mutated.len() {
        return MutationOutcome::build_error_with(
            "apply_mutation",
            vec![],
            format!(
                "mutation byte range {}..{} is outside {} bytes in {}",
                range.start,
                range.end,
                mutated.len(),
                file_path.display()
            ),
        );
    }
    mutated.splice(range, mutation.replacement.as_bytes().iter().copied());

    if let Err(e) = write_workspace_file(&file_path, &mutated) {
        eprintln!("warning: could not write {}: {e}", file_path.display());
        return MutationOutcome::build_error_with(
            "write_source",
            vec![],
            format!("could not write {}: {e}", file_path.display()),
        );
    }

    // Explicit build check: skip expensive test if mutation doesn't compile.
    if build_command.explicit && !build_command.argv.is_empty() {
        let build_outcome = run_command(
            build_command.argv,
            sandbox_command,
            project_root,
            timeout,
            true,
            env,
            cancelled,
        );
        if build_outcome.cancelled {
            return build_outcome;
        }
        if build_outcome.result != MutationResult::Survived {
            return MutationOutcome::build_error_with(
                "build_command",
                build_command.argv.to_vec(),
                build_error_message_from_outcome(
                    "build command",
                    build_command.argv,
                    project_root,
                    &build_outcome,
                ),
            );
        }
    }

    if cancelled.load(Ordering::Acquire) {
        return MutationOutcome::cancelled();
    }

    // Run test command; guard will restore the file on drop
    run_command(
        command,
        sandbox_command,
        project_root,
        timeout,
        capture_output,
        env,
        cancelled,
    )
}

fn run_command(
    command: &[String],
    sandbox_command: &[String],
    cwd: &Path,
    timeout_dur: Duration,
    capture_output: bool,
    env: &HashMap<String, String>,
    cancelled: &AtomicBool,
) -> MutationOutcome {
    if cancelled.load(Ordering::Acquire) {
        return MutationOutcome::cancelled();
    }

    if command.is_empty() {
        return MutationOutcome::build_error_with("command", vec![], "command is empty");
    }

    let command = sandboxed_command(sandbox_command, command);
    // foxguard: ignore[rs/no-command-injection]
    // User-provided argv is executed directly without a shell; this is the
    // core feature of the runner, not a string interpolation sink.
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(cwd).envs(env);
    configure_command_for_process_tree(&mut cmd);

    let mut stdout_capture = None;
    let mut stderr_capture = None;
    if capture_output {
        stdout_capture = match OutputCapture::new("stdout") {
            Ok(capture) => Some(capture),
            Err(e) => {
                eprintln!("warning: could not create stdout capture file: {e}");
                return MutationOutcome::build_error_with(
                    "command",
                    command.to_vec(),
                    format!("could not create stdout capture file: {e}"),
                );
            }
        };
        stderr_capture = match OutputCapture::new("stderr") {
            Ok(capture) => Some(capture),
            Err(e) => {
                eprintln!("warning: could not create stderr capture file: {e}");
                return MutationOutcome::build_error_with(
                    "command",
                    command.to_vec(),
                    format!("could not create stderr capture file: {e}"),
                );
            }
        };
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }

    if cancelled.load(Ordering::Acquire) {
        return MutationOutcome::cancelled();
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: could not spawn command {:?}: {e}", command[0]);
            return MutationOutcome::build_error_with(
                "command",
                command.to_vec(),
                format!("could not spawn command {:?}: {e}", command[0]),
            );
        }
    };
    let mut process_tree = ProcessTreeGuard::attach(&child);

    if capture_output {
        let Some(stdout) = child.stdout.take() else {
            terminate_and_wait(&mut child, &mut process_tree);
            return MutationOutcome::build_error_with(
                "command",
                command.to_vec(),
                "could not capture command stdout",
            );
        };
        if let Some(capture) = stdout_capture.as_mut() {
            if let Err(e) = capture.start(stdout) {
                eprintln!("warning: could not open stdout capture file: {e}");
                terminate_and_wait(&mut child, &mut process_tree);
                return MutationOutcome::build_error_with(
                    "command",
                    command.to_vec(),
                    format!("could not open stdout capture file: {e}"),
                );
            }
        }

        let Some(stderr) = child.stderr.take() else {
            terminate_and_wait(&mut child, &mut process_tree);
            finish_capture_threads(
                &mut stdout_capture,
                &mut stderr_capture,
                capture_cleanup_deadline(),
            );
            return MutationOutcome::build_error_with(
                "command",
                command.to_vec(),
                "could not capture command stderr",
            );
        };
        if let Some(capture) = stderr_capture.as_mut() {
            if let Err(e) = capture.start(stderr) {
                eprintln!("warning: could not open stderr capture file: {e}");
                terminate_and_wait(&mut child, &mut process_tree);
                finish_capture_threads(
                    &mut stdout_capture,
                    &mut stderr_capture,
                    capture_cleanup_deadline(),
                );
                return MutationOutcome::build_error_with(
                    "command",
                    command.to_vec(),
                    format!("could not open stderr capture file: {e}"),
                );
            }
        }
    }

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if cancelled.load(Ordering::Acquire) {
                    terminate_and_wait(&mut child, &mut process_tree);
                    finish_capture_threads(
                        &mut stdout_capture,
                        &mut stderr_capture,
                        capture_cleanup_deadline(),
                    );
                    return MutationOutcome::cancelled();
                }
                if started.elapsed() >= timeout_dur {
                    terminate_and_wait(&mut child, &mut process_tree);
                    finish_capture_threads(
                        &mut stdout_capture,
                        &mut stderr_capture,
                        capture_cleanup_deadline(),
                    );
                    return MutationOutcome::new(MutationResult::Timeout, None);
                }
                let remaining = timeout_dur.saturating_sub(started.elapsed());
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(e) => {
                terminate_and_wait(&mut child, &mut process_tree);
                finish_capture_threads(
                    &mut stdout_capture,
                    &mut stderr_capture,
                    capture_cleanup_deadline(),
                );
                return MutationOutcome::build_error_with(
                    "command",
                    command.to_vec(),
                    format!("could not wait for command: {e}"),
                );
            }
        }
    };

    process_tree.terminate(child.id());

    let result = if status.success() {
        MutationResult::Survived
    } else {
        MutationResult::Killed
    };
    let test_output = if capture_output {
        let capture_deadline = capture_cleanup_deadline();
        let stdout = stdout_capture
            .as_mut()
            .expect("stdout capture file should exist")
            .finish(capture_deadline);
        let stderr = stderr_capture
            .as_mut()
            .expect("stderr capture file should exist")
            .finish(capture_deadline);
        let mut combined = String::from_utf8_lossy(&stdout.bytes).into_owned();
        append_truncation_notice(&mut combined, stdout.truncated, "stdout");

        let stderr_text = String::from_utf8_lossy(&stderr.bytes);
        if !stderr_text.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&stderr_text);
        }
        append_truncation_notice(&mut combined, stderr.truncated, "stderr");
        Some(combined)
    } else {
        None
    };

    MutationOutcome::new(result, test_output)
}

fn terminate_and_wait(child: &mut std::process::Child, process_tree: &mut ProcessTreeGuard) {
    process_tree.terminate(child.id());
    let _ = child.wait();
}

#[cfg(unix)]
struct ProcessTreeGuard;

#[cfg(unix)]
impl ProcessTreeGuard {
    fn attach(_child: &std::process::Child) -> Self {
        Self
    }

    fn terminate(&mut self, pid: u32) {
        terminate_process_tree(pid);
    }
}

#[cfg(windows)]
struct ProcessTreeGuard {
    job: Option<WindowsJobHandle>,
}

#[cfg(windows)]
impl ProcessTreeGuard {
    fn attach(child: &std::process::Child) -> Self {
        match WindowsJobHandle::new_kill_on_close().and_then(|job| {
            job.assign(child)?;
            Ok(job)
        }) {
            Ok(job) => Self { job: Some(job) },
            Err(e) => {
                eprintln!("warning: could not attach command to Windows job object: {e}");
                Self { job: None }
            }
        }
    }

    fn terminate(&mut self, pid: u32) {
        if self.job.take().is_none() {
            terminate_process_tree(pid);
        }
    }
}

#[cfg(windows)]
struct WindowsJobHandle {
    handle: WindowsHandle,
}

#[cfg(windows)]
impl WindowsJobHandle {
    fn new_kill_on_close() -> std::io::Result<Self> {
        use std::mem::size_of;
        use std::ptr::null;

        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }

        let job = Self { handle };
        let mut info = WindowsJobExtendedLimitInformation::default();
        info.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job.handle,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                &info as *const _ as *const std::ffi::c_void,
                size_of::<WindowsJobExtendedLimitInformation>() as u32,
            )
        };
        if configured == 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(job)
    }

    fn assign(&self, child: &std::process::Child) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;

        let assigned = unsafe {
            AssignProcessToJobObject(self.handle, child.as_raw_handle() as WindowsHandle)
        };
        if assigned == 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsJobHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
type WindowsHandle = *mut std::ffi::c_void;

#[cfg(windows)]
const INVALID_HANDLE_VALUE: WindowsHandle = -1isize as WindowsHandle;
#[cfg(windows)]
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
#[cfg(windows)]
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct WindowsJobBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct WindowsIoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct WindowsJobExtendedLimitInformation {
    basic_limit_information: WindowsJobBasicLimitInformation,
    io_info: WindowsIoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(job_attributes: *const std::ffi::c_void, name: *const u16)
    -> WindowsHandle;
    fn SetInformationJobObject(
        job: WindowsHandle,
        information_class: i32,
        information: *const std::ffi::c_void,
        information_length: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: WindowsHandle, process: WindowsHandle) -> i32;
    fn CloseHandle(handle: WindowsHandle) -> i32;
}

#[cfg(not(any(unix, windows)))]
struct ProcessTreeGuard;

#[cfg(not(any(unix, windows)))]
impl ProcessTreeGuard {
    fn attach(_child: &std::process::Child) -> Self {
        Self
    }

    fn terminate(&mut self, _pid: u32) {}
}

#[cfg(unix)]
fn configure_command_for_process_tree(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn configure_command_for_process_tree(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn configure_command_for_process_tree(_cmd: &mut std::process::Command) {}

#[cfg(unix)]
fn terminate_process_tree(pid: u32) {
    let pgid = pid as libc::pid_t;
    unsafe {
        let _ = libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn terminate_process_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(any(unix, windows)))]
fn terminate_process_tree(_pid: u32) {}

struct OutputCapture {
    file: tempfile::NamedTempFile,
    reader: Option<thread::JoinHandle<bool>>,
    stream: &'static str,
}

impl OutputCapture {
    fn new(stream: &'static str) -> std::io::Result<Self> {
        Ok(Self {
            file: tempfile::NamedTempFile::new()?,
            reader: None,
            stream,
        })
    }

    fn start<R>(&mut self, reader: R) -> std::io::Result<()>
    where
        R: Read + Send + 'static,
    {
        let writer = self.file.reopen()?;
        self.reader = Some(spawn_output_capture(reader, writer, self.stream));
        Ok(())
    }

    fn finish(&mut self, deadline: Instant) -> CapturedOutput {
        let truncated = self
            .reader
            .take()
            .is_some_and(|reader| finish_capture_reader(reader, deadline, self.stream));
        read_captured_output(self.file.path(), self.stream, truncated)
    }
}

fn capture_cleanup_deadline() -> Instant {
    Instant::now() + CAPTURE_CLEANUP_TIMEOUT
}

fn finish_capture_reader(
    reader: thread::JoinHandle<bool>,
    deadline: Instant,
    stream: &'static str,
) -> bool {
    loop {
        if reader.is_finished() {
            return match reader.join() {
                Ok(truncated) => truncated,
                Err(_) => {
                    eprintln!("warning: {stream} capture thread panicked");
                    true
                }
            };
        }

        let now = Instant::now();
        if now >= deadline {
            eprintln!("warning: {stream} capture thread did not finish before cleanup deadline");
            return false;
        }

        thread::sleep((deadline - now).min(Duration::from_millis(5)));
    }
}

fn finish_capture_threads(
    stdout_capture: &mut Option<OutputCapture>,
    stderr_capture: &mut Option<OutputCapture>,
    deadline: Instant,
) {
    if let Some(capture) = stdout_capture.as_mut() {
        let _ = capture.finish(deadline);
    }
    if let Some(capture) = stderr_capture.as_mut() {
        let _ = capture.finish(deadline);
    }
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_output_capture<R>(
    mut reader: R,
    mut writer: fs::File,
    stream: &'static str,
) -> thread::JoinHandle<bool>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut written = 0usize;
        let mut truncated = false;
        let mut buffer = [0; 8192];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let available = CAPTURED_OUTPUT_LIMIT.saturating_sub(written);
                    let keep = n.min(available);
                    if keep > 0 {
                        if let Err(e) = writer.write_all(&buffer[..keep]) {
                            eprintln!("warning: could not write {stream} capture output: {e}");
                            break;
                        }
                        written += keep;
                    }
                    if keep < n {
                        truncated = true;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    eprintln!("warning: could not read {stream} capture output: {e}");
                    break;
                }
            }
        }
        let _ = writer.flush();
        truncated
    })
}

fn read_captured_output(path: &Path, stream: &str, already_truncated: bool) -> CapturedOutput {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("warning: could not read {stream} capture file: {e}");
            return CapturedOutput {
                bytes: Vec::new(),
                truncated: false,
            };
        }
    };
    let mut bytes = Vec::new();
    let mut limited = std::io::Read::by_ref(&mut file).take((CAPTURED_OUTPUT_LIMIT + 1) as u64);
    if let Err(e) = limited.read_to_end(&mut bytes) {
        eprintln!("warning: could not read {stream} capture output: {e}");
        return CapturedOutput {
            bytes: Vec::new(),
            truncated: false,
        };
    }
    let truncated = already_truncated || bytes.len() > CAPTURED_OUTPUT_LIMIT;
    if truncated {
        bytes.truncate(CAPTURED_OUTPUT_LIMIT);
    }
    CapturedOutput { bytes, truncated }
}

fn append_truncation_notice(output: &mut String, truncated: bool, stream: &str) {
    if !truncated {
        return;
    }
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&format!(
        "[{stream} truncated after {CAPTURED_OUTPUT_LIMIT} bytes]"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mutation;

    fn successful_command() -> Vec<String> {
        #[cfg(windows)]
        {
            vec!["cmd".into(), "/C".into(), "exit 0".into()]
        }
        #[cfg(not(windows))]
        {
            vec!["sh".into(), "-c".into(), "true".into()]
        }
    }

    fn failing_command() -> Vec<String> {
        #[cfg(windows)]
        {
            vec!["cmd".into(), "/C".into(), "exit 1".into()]
        }
        #[cfg(not(windows))]
        {
            vec!["sh".into(), "-c".into(), "false".into()]
        }
    }

    #[test]
    fn file_guard_restores_content_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"original").unwrap();

        {
            let _guard = FileGuard {
                path: path.clone(),
                original: b"original".to_vec(),
            };
            std::fs::write(&path, b"modified").unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"modified");
        }

        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn file_guard_restores_content_on_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"original").unwrap();
        let path_for_closure = path.clone();

        let result = std::panic::catch_unwind(move || {
            let _guard = FileGuard {
                path: path_for_closure.clone(),
                original: b"original".to_vec(),
            };
            std::fs::write(&path_for_closure, b"mutated").unwrap();
            assert_eq!(std::fs::read(&path_for_closure).unwrap(), b"mutated");
            panic!("simulated panic mid-mutation");
        });

        assert!(
            result.is_err(),
            "panic should propagate out of catch_unwind"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"original",
            "FileGuard must restore original content even when unwinding"
        );
    }

    fn make_test_mutation(file: &std::path::Path) -> Mutation {
        Mutation {
            id: 1,
            file: file.to_path_buf(),
            language: String::new(),
            line: 1,
            column: 1,
            operator: "test".into(),
            description: "test mutation".into(),
            original: "hello".into(),
            replacement: "world".into(),
            byte_range: 0..5,
        }
    }

    /// Creates a tempdir with a "test.txt" containing "hello world" and a matching mutation.
    fn make_test_setup() -> (tempfile::TempDir, PathBuf, Mutation) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();
        let mutation = make_test_mutation(&file);
        (dir, file, mutation)
    }

    fn make_relative_test_setup() -> (tempfile::TempDir, PathBuf, Mutation) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();
        let mutation = make_test_mutation(Path::new("test.txt"));
        (dir, file, mutation)
    }

    fn go_operator_mutation(id: u32, file: &str, source: &str, nth: usize) -> Mutation {
        let mut offset = 0usize;
        for index in 0..=nth {
            let next = source[offset..]
                .find("==")
                .expect("source should contain ==");
            offset += next;
            if index == nth {
                break;
            }
            offset += 2;
        }
        Mutation {
            id,
            file: PathBuf::from(file),
            language: "go".into(),
            line: 1,
            column: 1,
            operator: "eq_to_neq".into(),
            description: "Replace == with !=".into(),
            original: "==".into(),
            replacement: "!=".into(),
            byte_range: offset..offset + 2,
        }
    }

    fn c_operator_mutation(id: u32, file: &str, source: &str, nth: usize) -> Mutation {
        let mut mutation = go_operator_mutation(id, file, source, nth);
        mutation.language = "c".into();
        mutation
    }

    fn cpp_operator_mutation(id: u32, file: &str, source: &str, nth: usize) -> Mutation {
        let mut mutation = go_operator_mutation(id, file, source, nth);
        mutation.language = "cpp".into();
        mutation
    }

    fn rust_operator_mutation(id: u32, file: &str, source: &str, nth: usize) -> Mutation {
        let mut mutation = go_operator_mutation(id, file, source, nth);
        mutation.language = "rust".into();
        mutation
    }

    fn java_operator_mutation(id: u32, file: &str, source: &str, nth: usize) -> Mutation {
        let mut mutation = go_operator_mutation(id, file, source, nth);
        mutation.language = "java".into();
        mutation
    }

    #[test]
    fn cache_context_fingerprint_changes_for_tests_and_config() {
        let dir = tempfile::tempdir().unwrap();
        let initial = cache_context_fingerprint(dir.path());

        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            b"pub fn value() -> i32 { 1 }",
        )
        .unwrap();
        assert_ne!(
            cache_context_fingerprint(dir.path()),
            initial,
            "source files are verdict context: they shape test compilation and module wiring (#410)"
        );

        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        let test_file = dir.path().join("tests/calc_test.go");
        std::fs::write(&test_file, b"package calc\nfunc TestValue() {}\n").unwrap();
        let with_test = cache_context_fingerprint(dir.path());
        assert_ne!(with_test, initial);

        std::fs::write(&test_file, b"package calc\nfunc TestValueChanged() {}\n").unwrap();
        let changed_test = cache_context_fingerprint(dir.path());
        assert_ne!(changed_test, with_test);

        std::fs::write(dir.path().join("togi.toml"), b"[test]\ntimeout = 10\n").unwrap();
        assert_ne!(cache_context_fingerprint(dir.path()), changed_test);
    }

    #[test]
    fn source_content_cache_reads_each_file_once_lazily() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src.txt");
        std::fs::write(&file, b"hello world").unwrap();

        let mut first = make_test_mutation(&file);
        first.id = 1;
        let mut second = make_test_mutation(&file);
        second.id = 2;
        second.byte_range = 6..11;

        let cache = SourceContentCache::default();

        assert_eq!(cache.cached_entry_count(), 0);
        assert_eq!(
            cache.content_for(dir.path(), &first.file).unwrap(),
            b"hello world"
        );
        assert_eq!(
            cache.content_for(dir.path(), &second.file).unwrap(),
            b"hello world"
        );
        assert_eq!(cache.cached_entry_count(), 1);
    }

    #[test]
    fn git_cache_context_uses_index_when_context_is_clean() {
        if !git_available() {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test"]);

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[package]\nname = \"fixture\"\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn value() -> i32 { 1 }\n").unwrap();
        std::fs::write(
            root.join("tests/value_test.rs"),
            b"#[test]\nfn value() {}\n",
        )
        .unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);

        let clean = git_cache_context_fingerprint(root).expect("clean git fingerprint");
        std::fs::write(root.join("src/lib.rs"), b"pub fn value() -> i32 { 2 }\n").unwrap();
        assert!(
            git_cache_context_fingerprint(root).is_none(),
            "dirty source files should fall back to filesystem hashing"
        );
        assert_ne!(
            cache_context_fingerprint(root),
            clean,
            "source edits must change the cache context fingerprint"
        );

        std::fs::write(
            root.join("tests/value_test.rs"),
            b"#[test]\nfn changed() {}\n",
        )
        .unwrap();
        assert!(
            git_cache_context_fingerprint(root).is_none(),
            "dirty test files should fall back to filesystem hashing"
        );
    }

    #[test]
    fn cache_context_fingerprint_changes_for_new_source_file() {
        if !git_available() {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test"]);

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[package]\nname = \"fixture\"\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn value() -> i32 { 1 }\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);

        let before = cache_context_fingerprint(root);
        // A brand-new source file joining the diff (tracked via `git add -N`)
        // must invalidate verdicts cached before it existed (#410).
        std::fs::write(root.join("src/codec.rs"), b"pub fn decode() -> i32 { 1 }\n").unwrap();
        assert_ne!(cache_context_fingerprint(root), before);
    }

    #[test]
    fn cache_context_fingerprint_changes_when_src_test_module_edited() {
        if !git_available() {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test"]);

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[package]\nname = \"fixture\"\n").unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            b"pub fn value() -> i32 { 1 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn value() {\n        assert_eq!(super::value(), 1);\n    }\n}\n",
        )
        .unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);

        let before = cache_context_fingerprint(root);
        // Tests colocated in a source file are verdict context too (#410).
        std::fs::write(
            root.join("src/lib.rs"),
            b"pub fn value() -> i32 { 1 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn value() {\n        assert_eq!(super::value(), 2);\n    }\n}\n",
        )
        .unwrap();
        assert_ne!(cache_context_fingerprint(root), before);
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_clean_git_fixture(root: &Path) {
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test"]);
        run_git(root, &["config", "core.autocrlf", "false"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn f() {}\n").unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "initial"]);
    }

    fn test_command_config() -> CommandConfig {
        CommandConfig {
            command: vec!["cargo".into(), "test".into()],
            force_default_command: false,
            force_default_timeout: false,
            project_commands: vec![],
            language_commands: HashMap::new(),
            build_command: vec![],
            sandbox_command: vec![],
            build_command_explicit: false,
            timeout: Duration::from_secs(30),
            language_timeouts: HashMap::new(),
            test_selection: None,
        }
    }

    #[test]
    fn select_test_command_uses_default_command_and_timeout() {
        let commands = test_command_config();
        let mutation = make_test_mutation(Path::new("src/lib.rs"));

        let selected = select_test_command(Path::new("/repo"), &commands, &mutation);

        assert_eq!(selected.argv, vec!["cargo", "test"]);
        assert_eq!(selected.timeout, Duration::from_secs(30));
    }

    #[test]
    fn select_test_command_uses_language_command_and_timeout() {
        let mut commands = test_command_config();
        commands.language_commands.insert(
            "go".into(),
            vec!["go".into(), "test".into(), "./...".into()],
        );
        commands
            .language_timeouts
            .insert("go".into(), Duration::from_secs(5));
        let mut mutation = make_test_mutation(Path::new("calc.go"));
        mutation.language = "go".into();

        let selected = select_test_command(Path::new("/repo"), &commands, &mutation);

        assert_eq!(selected.argv, vec!["go", "test", "./..."]);
        assert_eq!(selected.timeout, Duration::from_secs(5));
    }

    #[test]
    fn select_test_command_uses_project_command_and_timeout() {
        let mut commands = test_command_config();
        commands.project_commands.push(ProjectCommandConfig {
            path: PathBuf::from("services/api"),
            command: Some(vec![
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "api".into(),
            ]),
            timeout: Some(Duration::from_secs(12)),
        });
        let mutation = make_test_mutation(Path::new("services/api/src/lib.rs"));

        let selected = select_test_command(Path::new("/repo"), &commands, &mutation);

        assert_eq!(selected.argv, vec!["cargo", "test", "-p", "api"]);
        assert_eq!(selected.timeout, Duration::from_secs(12));
    }

    #[test]
    fn select_test_command_project_longest_prefix_wins() {
        let mut commands = test_command_config();
        commands.project_commands.push(ProjectCommandConfig {
            path: PathBuf::from("services"),
            command: Some(vec!["make".into(), "test-services".into()]),
            timeout: Some(Duration::from_secs(10)),
        });
        commands.project_commands.push(ProjectCommandConfig {
            path: PathBuf::from("services/api"),
            command: Some(vec!["make".into(), "test-api".into()]),
            timeout: Some(Duration::from_secs(20)),
        });
        let mutation = make_test_mutation(Path::new("services/api/src/lib.rs"));

        let selected = select_test_command(Path::new("/repo"), &commands, &mutation);

        assert_eq!(selected.argv, vec!["make", "test-api"]);
        assert_eq!(selected.timeout, Duration::from_secs(20));
    }

    #[test]
    fn select_test_command_project_overrides_language() {
        let mut commands = test_command_config();
        commands.language_commands.insert(
            "go".into(),
            vec!["go".into(), "test".into(), "./...".into()],
        );
        commands
            .language_timeouts
            .insert("go".into(), Duration::from_secs(5));
        commands.project_commands.push(ProjectCommandConfig {
            path: PathBuf::from("services/api"),
            command: Some(vec![
                "go".into(),
                "test".into(),
                "./services/api/...".into(),
            ]),
            timeout: Some(Duration::from_secs(9)),
        });
        let mut mutation = make_test_mutation(Path::new("services/api/calc.go"));
        mutation.language = "go".into();

        let selected = select_test_command(Path::new("/repo"), &commands, &mutation);

        assert_eq!(selected.argv, vec!["go", "test", "./services/api/..."]);
        assert_eq!(selected.timeout, Duration::from_secs(9));
    }

    #[test]
    fn select_test_command_project_timeout_can_override_language_command() {
        let mut commands = test_command_config();
        commands.language_commands.insert(
            "go".into(),
            vec!["go".into(), "test".into(), "./...".into()],
        );
        commands
            .language_timeouts
            .insert("go".into(), Duration::from_secs(5));
        commands.project_commands.push(ProjectCommandConfig {
            path: PathBuf::from("services/api"),
            command: None,
            timeout: Some(Duration::from_secs(11)),
        });
        let mut mutation = make_test_mutation(Path::new("services/api/calc.go"));
        mutation.language = "go".into();

        let selected = select_test_command(Path::new("/repo"), &commands, &mutation);

        assert_eq!(selected.argv, vec!["go", "test", "./..."]);
        assert_eq!(selected.timeout, Duration::from_secs(11));
    }

    #[test]
    fn select_test_command_cli_override_keeps_project_timeout() {
        let mut commands = test_command_config();
        commands.command = vec!["make".into(), "ci".into()];
        commands.timeout = Duration::from_secs(30);
        commands.force_default_command = true;
        commands.language_commands.insert(
            "go".into(),
            vec!["go".into(), "test".into(), "./...".into()],
        );
        commands
            .language_timeouts
            .insert("go".into(), Duration::from_secs(5));
        commands.project_commands.push(ProjectCommandConfig {
            path: PathBuf::from("services/api"),
            command: Some(vec![
                "go".into(),
                "test".into(),
                "./services/api/...".into(),
            ]),
            timeout: Some(Duration::from_secs(9)),
        });
        let mut mutation = make_test_mutation(Path::new("services/api/calc.go"));
        mutation.language = "go".into();

        let selected = select_test_command(Path::new("/repo"), &commands, &mutation);

        assert_eq!(selected.argv, vec!["make", "ci"]);
        assert_eq!(selected.timeout, Duration::from_secs(9));
    }

    #[test]
    fn select_test_command_cli_timeout_overrides_project_and_language_timeout() {
        let mut commands = test_command_config();
        commands.timeout = Duration::from_secs(30);
        commands.force_default_timeout = true;
        commands
            .language_timeouts
            .insert("go".into(), Duration::from_secs(5));
        commands.project_commands.push(ProjectCommandConfig {
            path: PathBuf::from("services/api"),
            command: Some(vec![
                "go".into(),
                "test".into(),
                "./services/api/...".into(),
            ]),
            timeout: Some(Duration::from_secs(9)),
        });
        let mut mutation = make_test_mutation(Path::new("services/api/calc.go"));
        mutation.language = "go".into();

        let selected = select_test_command(Path::new("/repo"), &commands, &mutation);

        assert_eq!(selected.argv, vec!["go", "test", "./services/api/..."]);
        assert_eq!(selected.timeout, Duration::from_secs(30));
    }

    #[test]
    fn selected_test_command_cache_context_preserves_argv_boundaries() {
        let selected = SelectedTestCommand {
            argv: vec!["cargo test".into()],
            timeout: Duration::from_secs(2),
            selected_tests: Vec::new(),
        };
        let ambiguous = SelectedTestCommand {
            argv: vec!["cargo".into(), "test".into()],
            timeout: Duration::from_secs(2),
            selected_tests: Vec::new(),
        };

        assert_ne!(
            selected.cache_context(&[], false, &[], &HashMap::new()),
            ambiguous.cache_context(&[], false, &[], &HashMap::new())
        );
    }

    #[test]
    fn sandboxed_command_prefixes_wrapper_argv() {
        let sandbox = vec!["bwrap".into(), "--".into()];
        let command = vec!["cargo".into(), "test".into()];

        assert_eq!(
            sandboxed_command(&sandbox, &command),
            vec!["bwrap", "--", "cargo", "test"]
        );
        assert_eq!(sandboxed_command(&[], &command), command);
    }

    #[test]
    fn select_test_command_narrows_go_command_when_tests_cover_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        let file = tmp.path().join("src/calc.go");
        std::fs::write(&file, b"package calc\n").unwrap();

        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            &file,
            7,
            vec!["TestAdd".into(), "TestMax+Fast".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["go".into(), "test".into(), "./...".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/calc.go"));
        mutation.line = 7;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec!["go", "test", "-run", "^(TestAdd|TestMax\\+Fast)$", "./..."]
        );
    }

    #[test]
    fn select_test_command_replaces_go_run_flag_value() {
        let tmp = tempfile::tempdir().unwrap();
        let mut selection = TestSelectionConfig::new();
        selection.insert(tmp.path(), Path::new("calc.go"), 4, vec!["TestAdd".into()]);

        let mut commands = test_command_config();
        commands.command = vec![
            "go".into(),
            "test".into(),
            "-count=1".into(),
            "-run".into(),
            "OldPattern".into(),
            "./...".into(),
        ];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("calc.go"));
        mutation.line = 4;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec!["go", "test", "-count=1", "-run", "^(TestAdd)$", "./..."]
        );
    }

    #[test]
    fn select_test_command_replaces_go_run_equals_flag_value() {
        let tmp = tempfile::tempdir().unwrap();
        let mut selection = TestSelectionConfig::new();
        selection.insert(tmp.path(), Path::new("calc.go"), 4, vec!["TestAdd".into()]);

        let mut commands = test_command_config();
        commands.command = vec![
            "go".into(),
            "test".into(),
            "-count=1".into(),
            "-run=OldPattern".into(),
            "./...".into(),
        ];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("calc.go"));
        mutation.line = 4;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec!["go", "test", "-count=1", "-run=^(TestAdd)$", "./..."]
        );
    }

    #[test]
    fn force_go_no_test_cache_inserts_count_once() {
        assert_eq!(
            force_go_no_test_cache(vec!["go".into(), "test".into(), "./...".into()]),
            vec!["go", "test", "-count=1", "./..."]
        );
        assert_eq!(
            force_go_no_test_cache(vec![
                "go".into(),
                "test".into(),
                "-count=1".into(),
                "./...".into()
            ]),
            vec!["go", "test", "-count=1", "./..."]
        );
        assert_eq!(
            force_go_no_test_cache(vec!["cargo".into(), "test".into()]),
            vec!["cargo", "test"]
        );
    }

    #[test]
    fn select_test_command_falls_back_when_no_tests_cover_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let mut commands = test_command_config();
        commands.command = vec!["go".into(), "test".into(), "./...".into()];
        commands.test_selection = Some(TestSelectionConfig::new());
        let mutation = make_test_mutation(Path::new("src/calc.go"));

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(selected.argv, vec!["go", "test", "./..."]);
    }

    #[test]
    fn select_test_command_orders_selected_tests_by_duration() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert_tests(
            tmp.path(),
            Path::new("src/calc.py"),
            3,
            vec![
                SelectedTest::new("slow_test", Some(50)),
                SelectedTest::new("fast_test", Some(5)),
                SelectedTest::new("unknown_test", None),
            ],
        );

        let mut commands = test_command_config();
        commands.command = vec!["pytest".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/calc.py"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec!["pytest", "-k", "fast_test or slow_test or unknown_test"]
        );
        Ok(())
    }

    #[test]
    fn select_test_command_narrows_pytest_node_ids() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/calc.py"),
            3,
            vec![
                "tests/test_calc.py::test_add".into(),
                "tests/test_calc.py::test_max".into(),
            ],
        );

        let mut commands = test_command_config();
        commands.command = vec!["python".into(), "-m".into(), "pytest".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/calc.py"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec![
                "python",
                "-m",
                "pytest",
                "tests/test_calc.py::test_add",
                "tests/test_calc.py::test_max"
            ]
        );
        Ok(())
    }

    #[test]
    fn select_test_command_falls_back_for_unsupported_pytest_names() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/calc.py"),
            3,
            vec!["test_add".into(), "test with space".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["pytest".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/calc.py"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(selected.argv, vec!["pytest"]);
        Ok(())
    }

    #[test]
    fn select_test_command_narrows_jest_and_vitest_names() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/calc.test.ts"),
            3,
            vec!["adds numbers".into(), "handles max+".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["npx".into(), "vitest".into(), "run".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/calc.test.ts"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec![
                "npx",
                "vitest",
                "run",
                "-t",
                "^(adds numbers|handles max\\+)$"
            ]
        );
        Ok(())
    }

    #[test]
    fn select_test_command_narrows_single_cargo_test_filter() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/lib.rs"),
            3,
            vec!["math::adds".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec![
            "cargo".into(),
            "test".into(),
            "--workspace".into(),
            "--".into(),
            "--nocapture".into(),
        ];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/lib.rs"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec![
                "cargo",
                "test",
                "--workspace",
                "math::adds",
                "--",
                "--nocapture"
            ]
        );
        Ok(())
    }

    #[test]
    fn select_test_command_falls_back_for_multiple_cargo_tests() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/lib.rs"),
            3,
            vec!["math::adds".into(), "math::max".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["cargo".into(), "test".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/lib.rs"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(selected.argv, vec!["cargo", "test"]);
        Ok(())
    }

    #[test]
    fn select_test_command_narrows_maven_tests() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/main/java/Calc.java"),
            3,
            vec!["CalcTest#adds".into(), "CalcTest#max".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["mvn".into(), "-q".into(), "test".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/main/java/Calc.java"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec!["mvn", "-q", "test", "-Dtest=CalcTest#adds,CalcTest#max"]
        );
        Ok(())
    }

    #[test]
    fn select_test_command_narrows_gradle_tests() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/main/java/Calc.java"),
            3,
            vec!["CalcTest.adds".into(), "CalcTest.max".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["./gradlew".into(), "test".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/main/java/Calc.java"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(
            selected.argv,
            vec![
                "./gradlew",
                "test",
                "--tests",
                "CalcTest.adds",
                "--tests",
                "CalcTest.max"
            ]
        );
        Ok(())
    }

    #[test]
    fn select_test_command_falls_back_for_unsupported_command() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/lib.rs"),
            3,
            vec!["test_name".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["make".into(), "test".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/lib.rs"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(selected.argv, vec!["make", "test"]);
        Ok(())
    }

    #[test]
    fn select_test_command_prioritizes_history_killer() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/calc.py"),
            3,
            vec!["test_slow".into(), "test_fast".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["pytest".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/calc.py"));
        mutation.line = 3;
        let history = cache::IncrementalHistoryStore::load(tmp.path());
        history.record(cache::IncrementalHistoryEntry {
            mutation_identity: cache_identity(tmp.path(), &mutation),
            mutation_description: mutation.description.clone(),
            result: MutationResult::Killed,
            source_hash: 1,
            command_hash: 2,
            relevant_test_hash: 3,
            covering_tests: vec!["test_slow".into(), "test_fast".into()],
            killer_test: Some("test_fast".into()),
        });

        let selected =
            select_test_command_with_history(tmp.path(), &commands, &mutation, Some(&history));

        assert_eq!(
            selected.argv,
            vec!["pytest", "-k", "test_fast or test_slow"]
        );
        Ok(())
    }

    #[test]
    fn incremental_history_reuses_result_and_force_rerun_bypasses_it() -> anyhow::Result<()> {
        let (dir, file, mutation) = make_test_setup();
        std::fs::create_dir_all(dir.path().join("tests"))?;
        std::fs::write(
            dir.path().join("tests/test_calc.rs"),
            "fn test_add() { assert!(true); }\n",
        )?;

        let mut selection = TestSelectionConfig::new();
        selection.insert(dir.path(), &file, mutation.line, vec!["test_add".into()]);
        let commands = || CommandConfig {
            command: failing_command(),
            force_default_command: false,
            force_default_timeout: false,
            project_commands: vec![],
            language_commands: HashMap::new(),
            build_command: vec![],
            sandbox_command: vec![],
            build_command_explicit: false,
            timeout: Duration::from_secs(5),
            language_timeouts: HashMap::new(),
            test_selection: Some(selection.clone()),
        };

        let command_config = commands();
        let selected =
            select_test_command_with_history(dir.path(), &command_config, &mutation, None);
        let command_ctx = selected.cache_context(
            &command_config.build_command,
            false,
            &command_config.sandbox_command,
            &HashMap::new(),
        );
        let context_hash = cache_context_fingerprint(dir.path());
        let test_context_index = TestContextIndex::build(dir.path());
        let relevant_test_hash =
            test_context_index.fingerprint_for_tests(&selected.selected_tests, context_hash);
        let source = std::fs::read(&file)?;
        let query = incremental_history_query(
            dir.path(),
            &mutation,
            &source,
            &command_ctx,
            relevant_test_hash,
        );
        cache::IncrementalHistoryStore::load(dir.path()).record(cache::IncrementalHistoryEntry {
            mutation_identity: query.mutation_identity.clone(),
            mutation_description: query.mutation_description.clone(),
            result: MutationResult::Survived,
            source_hash: query.source_hash,
            command_hash: query.command_hash,
            relevant_test_hash: query.relevant_test_hash,
            covering_tests: selected.selected_tests.clone(),
            killer_test: None,
        });

        let cached_runner = TestRunner {
            commands: commands(),
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let cached = cached_runner.run(vec![mutation.clone()]).report;
        assert_eq!(cached.results[0].1, MutationResult::Survived);

        let forced_runner = TestRunner {
            commands: commands(),
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: true,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let forced = forced_runner.run(vec![mutation]).report;
        assert_eq!(forced.results[0].1, MutationResult::Killed);
        Ok(())
    }

    #[test]
    fn incremental_history_invalidated_by_source_context_change() -> anyhow::Result<()> {
        let (dir, file, mutation) = make_test_setup();

        let commands = || CommandConfig {
            command: failing_command(),
            force_default_command: false,
            force_default_timeout: false,
            project_commands: vec![],
            language_commands: HashMap::new(),
            build_command: vec![],
            sandbox_command: vec![],
            build_command_explicit: false,
            timeout: Duration::from_secs(5),
            language_timeouts: HashMap::new(),
            test_selection: None,
        };

        let command_config = commands();
        let selected = select_test_command(dir.path(), &command_config, &mutation);
        let command_ctx = selected.cache_context(
            &command_config.build_command,
            false,
            &command_config.sandbox_command,
            &HashMap::new(),
        );
        let context_hash = cache_context_fingerprint(dir.path());
        let test_context_index = TestContextIndex::build(dir.path());
        let relevant_test_hash =
            test_context_index.fingerprint_for_tests(&selected.selected_tests, context_hash);
        let source = std::fs::read(&file)?;
        let query = incremental_history_query(
            dir.path(),
            &mutation,
            &source,
            &command_ctx,
            relevant_test_hash,
        );
        cache::IncrementalHistoryStore::load(dir.path()).record(cache::IncrementalHistoryEntry {
            mutation_identity: query.mutation_identity.clone(),
            mutation_description: query.mutation_description.clone(),
            result: MutationResult::Survived,
            source_hash: query.source_hash,
            command_hash: query.command_hash,
            relevant_test_hash: query.relevant_test_hash,
            covering_tests: selected.selected_tests.clone(),
            killer_test: None,
        });

        // A source file added after the verdict was recorded — e.g. a module
        // wiring change or a colocated test — must invalidate the restore (#410).
        std::fs::create_dir_all(dir.path().join("src"))?;
        std::fs::write(
            dir.path().join("src/codec.rs"),
            "pub fn decode() -> i32 { 1 }\n",
        )?;

        let runner = TestRunner {
            commands: commands(),
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let report = runner.run(vec![mutation]).report;
        assert_eq!(report.results[0].1, MutationResult::Killed);
        Ok(())
    }

    #[test]
    fn mutation_file_path_resolves_relative_paths_against_project_root() {
        let root = Path::new("/repo");

        assert_eq!(
            mutation_file_path(root, Path::new("src/lib.rs")),
            PathBuf::from("/repo/src/lib.rs")
        );
        assert_eq!(
            mutation_file_path(root, Path::new("/tmp/src/lib.rs")),
            PathBuf::from("/tmp/src/lib.rs")
        );
    }

    #[test]
    fn cache_identity_normalizes_absolute_and_relative_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/lib.rs");
        std::fs::write(&file, b"hello world").unwrap();

        let relative = make_test_mutation(Path::new("src/lib.rs"));
        let absolute = make_test_mutation(&file);

        assert_eq!(
            cache_identity(root, &relative),
            cache_identity(root, &absolute)
        );
    }

    #[test]
    fn validate_and_resolve_mutation_path_rejects_parent_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(tmp.path().join("secret.txt"), b"secret").unwrap();

        assert!(validate_and_resolve_mutation_path(&root, Path::new("../secret.txt")).is_err());
    }

    #[test]
    fn copy_workspace_copies_regular_files_and_skips_internal_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join(".venv")).unwrap();
        std::fs::create_dir_all(root.join("dist")).unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::create_dir_all(root.join("services/api/src")).unwrap();
        std::fs::create_dir_all(root.join("services/api/node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join("services/api/build")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::create_dir_all(root.join(".togi")).unwrap();
        std::fs::create_dir_all(root.join(".togi-cache")).unwrap();
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[package]\n").unwrap();
        std::fs::write(root.join(".ignore"), b"src/ignored_by_ignore.rs\n").unwrap();
        std::fs::write(root.join(".gitignore"), b"ignored-by-gitignore.txt\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn f() {}\n").unwrap();
        std::fs::write(
            root.join("src/ignored_by_ignore.rs"),
            b"pub fn ignored() {}\n",
        )
        .unwrap();
        std::fs::write(root.join("ignored-by-gitignore.txt"), b"skip").unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), b"skip").unwrap();
        std::fs::write(root.join(".venv/pyvenv.cfg"), b"skip").unwrap();
        std::fs::write(root.join("dist/bundle.js"), b"skip").unwrap();
        std::fs::write(root.join("build/artifact"), b"skip").unwrap();
        std::fs::write(root.join("services/api/src/lib.rs"), b"pub fn api() {}\n").unwrap();
        std::fs::write(root.join("services/api/node_modules/pkg/index.js"), b"skip").unwrap();
        std::fs::write(root.join("services/api/build/artifact"), b"skip").unwrap();
        std::fs::write(root.join("target/debug/build-artifact"), b"skip").unwrap();
        std::fs::write(root.join(".git/HEAD"), b"skip").unwrap();
        std::fs::write(root.join(".togi/cache"), b"skip").unwrap();
        std::fs::write(root.join(".togi-cache/cache-entry"), b"skip").unwrap();
        std::fs::write(root.join(".togi.lock"), b"skip").unwrap();
        std::fs::write(root.join(".codex/session"), b"skip").unwrap();
        std::fs::write(root.join(".claude/session"), b"skip").unwrap();

        let copy = copy_workspace(root).unwrap();

        assert_eq!(
            std::fs::read(copy.root().join("src/lib.rs")).unwrap(),
            b"pub fn f() {}\n"
        );
        assert!(copy.root().join("Cargo.toml").exists());
        assert!(!copy.root().join("src/ignored_by_ignore.rs").exists());
        assert!(!copy.root().join("ignored-by-gitignore.txt").exists());
        assert!(!copy.root().join("node_modules").exists());
        assert!(!copy.root().join(".venv").exists());
        assert!(!copy.root().join("dist").exists());
        assert!(!copy.root().join("build").exists());
        assert!(copy.root().join("services/api/src/lib.rs").exists());
        assert!(!copy.root().join("services/api/node_modules").exists());
        assert!(!copy.root().join("services/api/build").exists());
        assert!(!copy.root().join("target").exists());
        assert!(!copy.root().join(".git").exists());
        assert!(!copy.root().join(".togi").exists());
        assert!(!copy.root().join(".togi-cache").exists());
        assert!(!copy.root().join(".togi.lock").exists());
        assert!(!copy.root().join(".codex").exists());
        assert!(!copy.root().join(".claude").exists());
    }

    #[test]
    fn copy_workspace_can_include_ignored_files_when_requested() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join(".gitignore"), b"ignored.txt\n").unwrap();
        std::fs::write(root.join("ignored.txt"), b"copy me").unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn f() {}\n").unwrap();
        std::fs::write(root.join("target/debug/build-artifact"), b"skip").unwrap();

        let copy = copy_workspace_with_options(root, false).unwrap();

        assert_eq!(
            std::fs::read(copy.root().join("ignored.txt")).unwrap(),
            b"copy me"
        );
        assert!(copy.root().join("src/lib.rs").exists());
        assert!(!copy.root().join("target").exists());
    }

    #[test]
    fn reset_workspace_preserves_target_cache_and_removes_other_side_effects() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("source");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn f() {}\n").unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(workspace.join("target/debug")).unwrap();
        std::fs::write(workspace.join("target/debug/cache"), b"keep").unwrap();
        std::fs::write(workspace.join("side-effect"), b"drop").unwrap();

        reset_copied_workspace(&root, &workspace, true).unwrap();

        assert_eq!(
            std::fs::read(workspace.join("target/debug/cache")).unwrap(),
            b"keep"
        );
        assert!(workspace.join("src/lib.rs").exists());
        assert!(!workspace.join("side-effect").exists());
    }

    #[test]
    fn reset_workspace_handles_missing_workspace_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("source");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn f() {}\n").unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("side-effect"), b"drop").unwrap();

        reset_copied_workspace(&root, &workspace, true).unwrap();

        assert!(workspace.join("src/lib.rs").exists());
        assert!(!workspace.join("side-effect").exists());
        assert!(!workspace.join("target/debug/cache").exists());
    }

    #[test]
    fn copy_workspace_uses_git_worktree_for_clean_repo_root() {
        if !git_available() {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_clean_git_fixture(root);

        let copy = copy_workspace(root).unwrap();

        assert!(copy.root().join(".git").exists());
        assert_eq!(
            std::fs::read(copy.root().join("src/lib.rs")).unwrap(),
            b"pub fn f() {}\n"
        );

        std::fs::write(copy.root().join("src/lib.rs"), b"pub fn f() -> i32 { 2 }\n").unwrap();
        std::fs::write(copy.root().join("side-effect"), b"drop").unwrap();
        std::fs::create_dir_all(copy.root().join("target/debug")).unwrap();
        std::fs::write(copy.root().join("target/debug/cache"), b"keep").unwrap();

        copy.reset(root, true).unwrap();

        assert_eq!(
            std::fs::read(copy.root().join("src/lib.rs")).unwrap(),
            b"pub fn f() {}\n"
        );
        assert!(!copy.root().join("side-effect").exists());
        assert_eq!(
            std::fs::read(copy.root().join("target/debug/cache")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn copy_workspace_uses_git_worktree_with_dirty_overlay() -> std::io::Result<()> {
        if !git_available() {
            return Ok(());
        }

        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        init_clean_git_fixture(root);
        std::fs::write(root.join("src/old.rs"), b"pub fn old() {}\n")?;
        std::fs::write(root.join("src/replaced"), b"old file\n")?;
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "add old file"]);

        std::fs::write(root.join("src/lib.rs"), b"pub fn f() -> i32 { 1 }\n")?;
        std::fs::remove_file(root.join("src/old.rs"))?;
        std::fs::remove_file(root.join("src/replaced"))?;
        std::fs::create_dir(root.join("src/replaced"))?;
        std::fs::write(root.join("src/replaced/nested.rs"), b"pub fn nested() {}\n")?;
        std::fs::write(root.join("local.txt"), b"copy me")?;

        let copy = copy_workspace(root)?;

        assert!(copy.root().join(".git").exists());
        assert_eq!(
            std::fs::read(copy.root().join("src/lib.rs"))?,
            b"pub fn f() -> i32 { 1 }\n"
        );
        assert_eq!(std::fs::read(copy.root().join("local.txt"))?, b"copy me");
        assert!(!copy.root().join("src/old.rs").exists());
        assert_eq!(
            std::fs::read(copy.root().join("src/replaced/nested.rs"))?,
            b"pub fn nested() {}\n"
        );

        std::fs::write(copy.root().join("src/lib.rs"), b"side effect")?;
        std::fs::write(copy.root().join("local.txt"), b"changed")?;
        std::fs::write(copy.root().join("src/old.rs"), b"stale")?;
        std::fs::remove_dir_all(copy.root().join("src/replaced"))?;
        std::fs::write(copy.root().join("src/replaced"), b"stale file")?;
        std::fs::write(copy.root().join("side-effect"), b"drop")?;
        std::fs::create_dir_all(copy.root().join("target/debug"))?;
        std::fs::write(copy.root().join("target/debug/cache"), b"keep")?;

        copy.reset(root, true)?;

        assert_eq!(
            std::fs::read(copy.root().join("src/lib.rs"))?,
            b"pub fn f() -> i32 { 1 }\n"
        );
        assert_eq!(std::fs::read(copy.root().join("local.txt"))?, b"copy me");
        assert!(!copy.root().join("src/old.rs").exists());
        assert_eq!(
            std::fs::read(copy.root().join("src/replaced/nested.rs"))?,
            b"pub fn nested() {}\n"
        );
        assert!(!copy.root().join("side-effect").exists());
        assert_eq!(
            std::fs::read(copy.root().join("target/debug/cache"))?,
            b"keep"
        );
        Ok(())
    }

    #[test]
    fn baseline_timing_measures_build_and_test_in_workspace() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("source.txt"), "clean")?;
        let command = successful_command();

        let measurement = measure_baseline_timing(
            dir.path(),
            BaselineTimingConfig {
                test_command: &command,
                build_command: &command,
                sandbox_command: &[],
                build_command_explicit: true,
                timeout: Duration::from_secs(5),
                env: &HashMap::new(),
                cancelled: &AtomicBool::new(false),
                respect_workspace_ignores: false,
            },
        )?;

        assert!(measurement.build_duration.is_some());
        assert!(measurement.test_duration <= Duration::from_secs(5));
        Ok(())
    }

    #[test]
    fn workspace_pool_creates_slots_and_reuses_after_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), b"pub fn f() {}\n").unwrap();

        let pool = WorkspacePool::new(root, 2).unwrap();
        assert_eq!(pool.len(), 2);

        let first = pool.acquire();
        let second = pool.acquire();
        assert!(!first.needs_reset());
        assert!(!second.needs_reset());
        assert_ne!(first.root(), second.root());
        assert!(first.root().join("src/lib.rs").exists());
        assert!(second.root().join("src/lib.rs").exists());

        let first_root = first.root().to_path_buf();
        let second_root = second.root().to_path_buf();
        drop(second);

        let third = pool.acquire();
        assert!(third.needs_reset());
        assert_eq!(third.root(), second_root.as_path());
        assert_ne!(third.root(), first_root.as_path());
    }

    #[test]
    fn workspace_pool_uses_at_least_one_slot() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("file.txt"), b"content").unwrap();

        let pool = WorkspacePool::new(tmp.path(), 0).unwrap();

        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn workspace_pool_slot_count_is_bounded_by_total_mutations() {
        assert_eq!(workspace_pool_slot_count(0, 3), 1);
        assert_eq!(workspace_pool_slot_count(1, 3), 1);
        assert_eq!(workspace_pool_slot_count(2, 5), 2);
        assert_eq!(workspace_pool_slot_count(8, 3), 3);
    }

    #[test]
    fn command_succeeds_returns_survived() {
        let (dir, file, mutation) = make_relative_test_setup();

        let outcome = run_single_mutation(
            &["true".to_string()],
            &[],
            BuildCommand {
                argv: &[],
                explicit: false,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Survived);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn command_fails_returns_killed() {
        let (dir, file, mutation) = make_test_setup();

        let outcome = run_single_mutation(
            &["false".to_string()],
            &[],
            BuildCommand {
                argv: &[],
                explicit: false,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Killed);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn empty_replacement_splices_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        // Replace "hello" with "" (empty), making the file shorter
        let mutation = Mutation {
            id: 1,
            file: file.clone(),
            language: String::new(),
            line: 1,
            column: 1,
            operator: "removal".into(),
            description: "remove text".into(),
            original: "hello".into(),
            replacement: "".into(),
            byte_range: 0..5,
        };

        let outcome = run_single_mutation(
            &["true".to_string()],
            &[],
            BuildCommand {
                argv: &[],
                explicit: false,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Survived);
        // File should be restored to original
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn command_not_found_returns_build_error() {
        let (dir, file, mutation) = make_test_setup();

        let outcome = run_single_mutation(
            &["nonexistent_binary_xyz_12345".to_string()],
            &[],
            BuildCommand {
                argv: &[],
                explicit: false,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::BuildError);
        // File should be restored
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn command_timeout_returns_timeout() {
        let (dir, file, mutation) = make_test_setup();

        let outcome = run_single_mutation(
            &["sleep".to_string(), "10".to_string()],
            &[],
            BuildCommand {
                argv: &[],
                explicit: false,
            },
            Duration::from_millis(100),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Timeout);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn empty_build_error_output_falls_back_to_command_context() {
        let outcome = MutationOutcome::new(MutationResult::Killed, Some(String::new()));
        let message = build_error_message_from_outcome(
            "build command",
            &["cargo".to_string(), "check".to_string()],
            Path::new("/tmp/workspace"),
            &outcome,
        );

        assert!(message.contains("build command returned killed"));
        assert!(message.contains("command: cargo check"));
        assert!(message.contains("cwd: /tmp/workspace"));
    }

    #[cfg(unix)]
    #[test]
    fn build_check_failure_skips_test() {
        let (dir, file, mutation) = make_test_setup();

        let marker = dir.path().join("test_ran.marker");
        // Build fails → should return BuildError without running test
        let outcome = run_single_mutation(
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("touch {}", marker.display()),
            ],
            &[],
            BuildCommand {
                argv: &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "echo compile nope >&2; exit 1".to_string(),
                ],
                explicit: true,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::BuildError);
        let detail = outcome
            .build_error_detail
            .as_ref()
            .expect("build failure should carry diagnostics");
        assert_eq!(detail.phase, "build_command");
        assert!(detail.message.contains("compile nope"));
        assert!(!marker.exists(), "test command should not have run");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn build_check_success_runs_test() {
        let (dir, _file, mutation) = make_test_setup();

        // Build succeeds → test runs and fails → Killed
        let outcome = run_single_mutation(
            &["false".to_string()], // test fails = killed
            &[],
            BuildCommand {
                argv: &["true".to_string()], // build succeeds
                explicit: true,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Killed);
    }

    #[cfg(unix)]
    #[test]
    fn non_explicit_build_command_does_not_pre_filter() {
        let (dir, _file, mutation) = make_test_setup();

        let build_marker = dir.path().join("build_ran.marker");
        let test_marker = dir.path().join("test_ran.marker");

        let outcome = run_single_mutation(
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("touch {}", test_marker.display()),
            ],
            &[],
            BuildCommand {
                argv: &[
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("touch {}; exit 1", build_marker.display()),
                ],
                explicit: false,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Survived);
        assert!(
            !build_marker.exists(),
            "non-explicit build command should not run"
        );
        assert!(test_marker.exists(), "test command should still run");
    }

    #[test]
    fn out_of_range_byte_range_returns_build_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hi").unwrap();

        let mut mutation = make_test_mutation(&file);
        mutation.byte_range = 0..100; // way past end

        let outcome = run_single_mutation(
            &["true".to_string()],
            &[],
            BuildCommand {
                argv: &[],
                explicit: false,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::BuildError);
    }

    #[test]
    fn missing_file_returns_build_error() {
        let dir = tempfile::tempdir().unwrap();
        let mutation = make_test_mutation(&dir.path().join("nonexistent.txt"));

        let outcome = run_single_mutation(
            &["true".to_string()],
            &[],
            BuildCommand {
                argv: &[],
                explicit: false,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::BuildError);
    }

    #[test]
    fn empty_command_returns_build_error() {
        let outcome = run_command(
            &[],
            &[],
            &PathBuf::from("."),
            Duration::from_secs(5),
            false,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::BuildError);
    }

    #[cfg(unix)]
    #[test]
    fn capture_output_collects_stdout_stderr() {
        let outcome = run_command(
            &[
                "sh".to_string(),
                "-c".to_string(),
                "echo out; echo err >&2".to_string(),
            ],
            &[],
            &PathBuf::from("."),
            Duration::from_secs(5),
            true,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Survived);
        let output = outcome.test_output.unwrap();
        assert!(output.contains("out"), "should capture stdout");
        assert!(output.contains("err"), "should capture stderr");
    }

    #[cfg(unix)]
    #[test]
    fn capture_output_truncates_and_drains_large_stdout() {
        let bytes_to_write = CAPTURED_OUTPUT_LIMIT + 256 * 1024;
        let outcome = run_command(
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("yes x | head -c {bytes_to_write}"),
            ],
            &[],
            &PathBuf::from("."),
            Duration::from_secs(5),
            true,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Survived);
        let output = outcome.test_output.unwrap();
        assert!(
            output.contains("[stdout truncated after"),
            "large stdout should be marked as truncated"
        );
        assert!(
            output.len() < CAPTURED_OUTPUT_LIMIT + 128,
            "captured output should stay close to the configured cap"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_output_does_not_timeout_when_background_writer_holds_stdout() {
        let started = Instant::now();
        let outcome = run_command(
            &[
                "sh".to_string(),
                "-c".to_string(),
                "printf out; (sleep 1; printf late) &".to_string(),
            ],
            &[],
            &PathBuf::from("."),
            Duration::from_secs(5),
            true,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Survived);
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "finished child should not wait for background writer EOF"
        );
        assert!(
            outcome.test_output.unwrap().contains("out"),
            "captured output should include immediate stdout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_output_timeout_returns_promptly_with_background_writer() {
        let started = Instant::now();
        let outcome = run_command(
            &[
                "sh".to_string(),
                "-c".to_string(),
                "(sleep 1; printf late) & sleep 10".to_string(),
            ],
            &[],
            &PathBuf::from("."),
            Duration::from_millis(100),
            true,
            &HashMap::new(),
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "timeout path should not wait for descendant-held stdout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_output_timeout_returns_promptly_when_setsid_descendant_holds_stdout() {
        let setsid_available = match std::process::Command::new("setsid").arg("true").status() {
            Ok(status) => status.success(),
            Err(_) => false,
        };
        if !setsid_available {
            eprintln!("skipping setsid capture cleanup test because setsid is unavailable");
            return;
        }

        struct EscapedProcessGuard(PathBuf);

        impl Drop for EscapedProcessGuard {
            fn drop(&mut self) {
                let Ok(pid) = std::fs::read_to_string(&self.0) else {
                    return;
                };
                let Ok(pid) = pid.trim().parse::<libc::pid_t>() else {
                    return;
                };
                unsafe {
                    let _ = libc::kill(pid, libc::SIGKILL);
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("escaped.pid");
        let _guard = EscapedProcessGuard(pid_file.clone());
        let mut env = HashMap::new();
        env.insert("ESCAPED_PID".to_string(), pid_file.display().to_string());

        let started = Instant::now();
        let outcome = run_command(
            &[
                "sh".to_string(),
                "-c".to_string(),
                r#"setsid sh -c 'printf "%s\n" "$$" > "$ESCAPED_PID"; while :; do sleep 1; done' &
while [ ! -s "$ESCAPED_PID" ]; do sleep 0.01; done
sleep 10"#
                    .to_string(),
            ],
            &[],
            dir.path(),
            Duration::from_millis(200),
            true,
            &env,
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "timeout cleanup should not wait for a setsid descendant-held stdout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_capture_normal_completion_terminates_background_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("descendant.marker");
        let mut env = HashMap::new();
        env.insert("MARKER".to_string(), marker.display().to_string());

        let outcome = run_command(
            &[
                "sh".to_string(),
                "-c".to_string(),
                "(sleep 0.5; touch \"$MARKER\") &".to_string(),
            ],
            &[],
            dir.path(),
            Duration::from_secs(5),
            false,
            &env,
            &AtomicBool::new(false),
        );

        assert_eq!(outcome.result, MutationResult::Survived);
        thread::sleep(Duration::from_millis(800));
        assert!(
            !marker.exists(),
            "background descendant should be killed even when output is not captured"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runner_cancellation_stops_promptly_and_returns_cancelled() {
        let (dir, file, mutation) = make_test_setup();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_from_thread = cancelled.clone();

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sleep".into(), "10".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(30),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled,
        };

        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel_from_thread.store(true, Ordering::Release);
        });

        let started = Instant::now();
        let outcome = runner.run(vec![mutation]);
        canceller.join().unwrap();

        assert!(outcome.cancelled);
        assert_eq!(outcome.report.total, 0);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "runner should not wait for the command timeout after cancellation"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_schemata_reports_unsupported_runner_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let source = "def same(a, b):\n    return a == b\n";
        std::fs::write(dir.path().join("app.py"), source).unwrap();
        let start = source.find("a == b").unwrap();
        let mutation = Mutation {
            id: 0,
            file: PathBuf::from("app.py"),
            language: "python".into(),
            line: 2,
            column: 12,
            operator: "eq_to_neq".into(),
            description: "Replace == with !=".into(),
            original: "a == b".into(),
            replacement: "a != b".into(),
            byte_range: start..start + "a == b".len(),
        };

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["true".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run_with_schemata(vec![mutation]).report;
        let schemata = report.schemata.expect("schemata run should report stats");

        assert_eq!(report.total, 1);
        assert_eq!(report.survived, 1);
        assert_eq!(schemata.fast_path, 0);
        assert_eq!(schemata.fallback, 1);
        assert_eq!(schemata.fallback_reasons.len(), 1);
        assert_eq!(schemata.fallback_reasons[0].reason, "unsupported_runner");
        assert_eq!(schemata.fallback_reasons[0].count, 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_with_schemata_executes_c_mutation_with_active_env() {
        if std::process::Command::new("cc")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping C schemata runner test because cc is unavailable");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let source = "\
int same(int a, int b) {
    return a == b;
}

int main(void) {
    if (!same(1, 1)) {
        return 1;
    }
    if (same(1, 2)) {
        return 2;
    }
    return 0;
}
";
        std::fs::write(dir.path().join("calc.c"), source).unwrap();
        let mutation = c_operator_mutation(0, "calc.c", source, 0);

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["./calc".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec!["cc".into(), "calc.c".into(), "-o".into(), "calc".into()],
                sandbox_command: vec![],
                build_command_explicit: true,
                timeout: Duration::from_secs(30),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run_with_schemata(vec![mutation]).report;

        assert_eq!(report.total, 1);
        assert_eq!(report.results[0].1, MutationResult::Killed);
    }

    #[cfg(unix)]
    #[test]
    fn run_with_schemata_executes_cpp_mutation_with_active_env() {
        if std::process::Command::new("c++")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping C++ schemata runner test because c++ is unavailable");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let source = "\
bool same(int a, int b) {
    return a == b;
}

int main() {
    if (!same(1, 1)) {
        return 1;
    }
    if (same(1, 2)) {
        return 2;
    }
    return 0;
}
";
        std::fs::write(dir.path().join("calc.cpp"), source).unwrap();
        let mutation = cpp_operator_mutation(0, "calc.cpp", source, 0);

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["./calc".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![
                    "c++".into(),
                    "calc.cpp".into(),
                    "-std=c++11".into(),
                    "-o".into(),
                    "calc".into(),
                ],
                sandbox_command: vec![],
                build_command_explicit: true,
                timeout: Duration::from_secs(30),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run_with_schemata(vec![mutation]).report;

        assert_eq!(report.total, 1);
        assert_eq!(report.results[0].1, MutationResult::Killed);
    }

    #[cfg(unix)]
    #[test]
    fn run_with_schemata_executes_go_mutations_with_active_env() {
        let dir = tempfile::tempdir().unwrap();
        let source = "\
package calc
func first(a, b int) bool { return a == b }
func second(c, d int) bool { return c == d }
";
        std::fs::write(dir.path().join("calc.go"), source).unwrap();
        let first = go_operator_mutation(0, "calc.go", source, 0);
        let second = go_operator_mutation(1, "calc.go", source, 1);
        let script = r#"
case "$(cat calc.go)" in
  *__togi_active*) ;;
  *) exit 2 ;;
esac
case "$TOGI_MUTANT" in
  0) exit 1 ;;
  1) exit 0 ;;
  *) exit 2 ;;
esac
"#;

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sh".into(), "-c".into(), script.into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run_with_schemata(vec![first, second]).report;

        assert_eq!(report.total, 2);
        assert_eq!(report.results[0].1, MutationResult::Killed);
        assert_eq!(report.results[1].1, MutationResult::Survived);
    }

    #[cfg(unix)]
    #[test]
    fn run_with_schemata_resets_workspace_between_schema_mutants() {
        let dir = tempfile::tempdir().unwrap();
        let source = "\
package calc
func first(a, b int) bool { return a == b }
func second(c, d int) bool { return c == d }
";
        std::fs::write(dir.path().join("calc.go"), source).unwrap();
        let first = go_operator_mutation(0, "calc.go", source, 0);
        let second = go_operator_mutation(1, "calc.go", source, 1);
        let script = r#"
case "$(cat calc.go)" in
  *__togi_active*) ;;
  *) exit 2 ;;
esac
test ! -f side_effect || exit 1
touch side_effect
"#;

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sh".into(), "-c".into(), script.into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run_with_schemata(vec![first, second]).report;

        assert_eq!(report.total, 2);
        assert_eq!(report.survived, 2);
        assert_eq!(report.killed, 0);
    }

    #[cfg(unix)]
    #[test]
    fn run_with_schemata_uses_cache_and_releases_max_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let source = "\
package calc
func first(a, b int) bool { return a == b }
func second(c, d int) bool { return c == d }
";
        std::fs::write(dir.path().join("calc.go"), source).unwrap();
        let first = go_operator_mutation(0, "calc.go", source, 0);
        let second = go_operator_mutation(1, "calc.go", source, 1);
        let script = r#"
runs=0
if [ -f "$STATE_DIR/runs" ]; then runs=$(cat "$STATE_DIR/runs"); fi
runs=$((runs + 1))
printf '%s\n' "$runs" > "$STATE_DIR/runs"
case "$TOGI_MUTANT" in
  1) exit 0 ;;
  *) exit 2 ;;
esac
"#;
        let mut env = HashMap::new();
        env.insert("STATE_DIR".to_string(), state.path().display().to_string());
        let commands = CommandConfig {
            command: vec!["sh".into(), "-c".into(), script.into()],
            force_default_command: false,
            force_default_timeout: false,
            project_commands: vec![],
            language_commands: HashMap::new(),
            build_command: vec![],
            sandbox_command: vec![],
            build_command_explicit: false,
            timeout: Duration::from_secs(5),
            language_timeouts: HashMap::new(),
            test_selection: None,
        };
        // The seeded entry is computed without TOGI_MUTANT; the schemata runner
        // sets it per mutant at execution time but must still hit the cache.
        let cache_env = env.clone();
        let selected = select_test_command(dir.path(), &commands, &first);
        let cache_selected = SelectedTestCommand {
            argv: selected.argv,
            timeout: selected.timeout,
            selected_tests: selected.selected_tests,
        };
        let cache_ctx = cache_selected.cache_context(
            &commands.build_command,
            commands.build_command_explicit,
            &commands.sandbox_command,
            &cache_env,
        );
        let cache_ctx = format!(
            "{cache_ctx};context={:016x}",
            cache_context_fingerprint(dir.path())
        );
        let key = CacheKey::new(
            source.as_bytes(),
            &cache_identity(dir.path(), &first),
            &first.description,
            &cache_ctx,
        );
        cache::store(dir.path(), &key, MutationResult::Survived);

        let runner = TestRunner {
            commands,
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: Some(1),
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env,
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run_with_schemata(vec![first, second]).report;
        let runs: usize = std::fs::read_to_string(state.path().join("runs"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        assert_eq!(report.total, 2);
        assert_eq!(report.results[0].1, MutationResult::Survived);
        assert_eq!(report.results[1].1, MutationResult::Survived);
        assert_eq!(runs, 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_with_schemata_executes_java_mutation_with_active_env() {
        if std::process::Command::new("javac")
            .arg("-version")
            .output()
            .is_err()
            || std::process::Command::new("java")
                .arg("-version")
                .output()
                .is_err()
        {
            eprintln!("skipping Java schemata runner test because javac/java is unavailable");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let source = "\
class Calc {
    static boolean same(int a, int b) {
        return a == b;
    }

    public static void main(String[] args) {
        if (!same(1, 1)) {
            throw new AssertionError(\"same values should match\");
        }
        if (same(1, 2)) {
            throw new AssertionError(\"different values should not match\");
        }
    }
}
";
        std::fs::write(dir.path().join("Calc.java"), source).unwrap();
        let mutation = java_operator_mutation(0, "Calc.java", source, 0);

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["java".into(), "Calc".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec!["javac".into(), "Calc.java".into()],
                sandbox_command: vec![],
                build_command_explicit: true,
                timeout: Duration::from_secs(30),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run_with_schemata(vec![mutation]).report;

        assert_eq!(report.total, 1);
        assert_eq!(report.results[0].1, MutationResult::Killed);
    }

    #[test]
    fn run_with_schemata_executes_rust_mutation_with_active_env() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"schemata_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let source = "\
pub fn same(a: i32, b: i32) -> bool { a == b }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_values_match() {
        assert!(same(1, 1));
        assert!(!same(1, 2));
    }
}
";
        std::fs::write(dir.path().join("src/lib.rs"), source).unwrap();
        let mutation = rust_operator_mutation(0, "src/lib.rs", source, 0);

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["cargo".into(), "test".into(), "--quiet".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(60),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: {
                let mut env = HashMap::new();
                env.insert(
                    "CARGO_TARGET_DIR".to_string(),
                    dir.path().join("target").display().to_string(),
                );
                env
            },
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run_with_schemata(vec![mutation]).report;

        assert_eq!(report.total, 1);
        assert_eq!(report.results[0].1, MutationResult::Killed);
    }

    #[test]
    fn max_tested_limits_mutations() {
        let (dir, file, _) = make_test_setup();

        let mutations: Vec<Mutation> = (0..5)
            .map(|i| {
                let mut m = make_test_mutation(&file);
                m.id = i;
                m.description = format!("max tested limit {i}");
                m
            })
            .collect();

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["true".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: Some(2),
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;
        assert_eq!(report.total, 2, "should stop after max_tested");
    }

    #[test]
    fn max_survivors_stops_scheduling_new_mutations() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let mutations: Vec<Mutation> = (0..3)
            .map(|i| {
                let file = dir.path().join(format!("survivor{i}.txt"));
                std::fs::write(&file, b"hello world").expect("fixture file should be written");
                let mut mutation = make_test_mutation(&file);
                mutation.id = i;
                mutation.description = format!("survivor limit {i}");
                mutation
            })
            .collect();

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["true".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: EarlyStopConfig {
                max_survivors: Some(1),
                fail_under: None,
            },
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;

        assert_eq!(report.planned_total, 3);
        assert_eq!(report.total, 1);
        assert_eq!(report.survived, 1);
        assert!(
            report
                .early_stop_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("--max-survivors 1"))
        );
    }

    #[test]
    fn fail_under_stops_when_threshold_cannot_be_reached() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let mutations: Vec<Mutation> = (0..3)
            .map(|i| {
                let file = dir.path().join(format!("threshold{i}.txt"));
                std::fs::write(&file, b"hello world").expect("fixture file should be written");
                let mut mutation = make_test_mutation(&file);
                mutation.id = i;
                mutation.description = format!("threshold gate {i}");
                mutation
            })
            .collect();

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["true".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: EarlyStopConfig {
                max_survivors: None,
                fail_under: Some(80.0),
            },
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;

        assert_eq!(report.planned_total, 3);
        assert_eq!(report.total, 1);
        assert_eq!(report.survived, 1);
        assert!(
            report
                .early_stop_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("--fail-under 80.0"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn max_tested_reservation_caps_execution_before_commands_run() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        let mutations: Vec<Mutation> = (0..4)
            .map(|i| {
                let file = dir.path().join(format!("test{i}.txt"));
                std::fs::write(&file, b"hello world").unwrap();
                let mut mutation = make_test_mutation(&file);
                mutation.id = i;
                mutation.description = format!("max reservation {i}");
                mutation
            })
            .collect();

        let script = r#"
lock="$STATE_DIR/lock"
while ! mkdir "$lock" 2>/dev/null; do sleep 0.01; done
runs=0
if [ -f "$STATE_DIR/runs" ]; then runs=$(cat "$STATE_DIR/runs"); fi
runs=$((runs + 1))
printf '%s\n' "$runs" > "$STATE_DIR/runs"
rmdir "$lock"
sleep 0.2
"#;
        let mut env = HashMap::new();
        env.insert("STATE_DIR".to_string(), state.path().display().to_string());

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sh".into(), "-c".into(), script.into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 4,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: Some(1),
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env,
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;
        let runs: usize = std::fs::read_to_string(state.path().join("runs"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        assert_eq!(report.total, 1);
        assert_eq!(report.survived, 1);
        assert_eq!(
            runs, 1,
            "max_tested should reserve before execution, not after commands finish"
        );
    }

    #[cfg(unix)]
    #[test]
    fn max_tested_does_not_read_sources_for_unscheduled_mutations() {
        struct ReadHookGuard;

        impl Drop for ReadHookGuard {
            fn drop(&mut self) {
                set_source_content_read_hook(None);
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mutations: Vec<Mutation> = (0..25)
            .map(|i| {
                let file = dir.path().join(format!("mutation{i}.txt"));
                std::fs::write(&file, b"hello world").unwrap();
                let mut mutation = make_test_mutation(&file);
                mutation.id = i;
                mutation.description = format!("lazy setup {i}");
                mutation
            })
            .collect();

        let reads = Arc::new(AtomicUsize::new(0));
        let root = dir.path().to_path_buf();
        let reads_for_hook = reads.clone();
        set_source_content_read_hook(Some(Arc::new(move |path: &Path| {
            if path.starts_with(&root) {
                reads_for_hook.fetch_add(1, Ordering::SeqCst);
            }
        })));
        let _guard = ReadHookGuard;

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["true".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 8,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: Some(1),
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;

        assert_eq!(report.total, 1);
        assert_eq!(report.survived, 1);
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "source content should be read only after a mutation reserves execution budget"
        );
    }

    #[cfg(unix)]
    #[test]
    fn max_tested_cache_hits_release_reservation_for_uncached_execution() {
        fn run_case(cached_result: MutationResult) {
            let dir = tempfile::tempdir().unwrap();
            let state = tempfile::tempdir().unwrap();
            let cached_file = dir.path().join("cached.txt");
            let uncached_file = dir.path().join("uncached.txt");
            std::fs::write(&cached_file, b"hello world").unwrap();
            std::fs::write(&uncached_file, b"hello world").unwrap();

            let mut cached_mutation = make_test_mutation(&cached_file);
            cached_mutation.id = 1;
            cached_mutation.description = format!("cached {cached_result}");
            let mut uncached_mutation = make_test_mutation(&uncached_file);
            uncached_mutation.id = 2;
            uncached_mutation.description = "uncached execution".into();

            let script = r#"
lock="$STATE_DIR/lock"
while ! mkdir "$lock" 2>/dev/null; do sleep 0.01; done
runs=0
if [ -f "$STATE_DIR/runs" ]; then runs=$(cat "$STATE_DIR/runs"); fi
runs=$((runs + 1))
printf '%s\n' "$runs" > "$STATE_DIR/runs"
rmdir "$lock"
"#;
            let mut env = HashMap::new();
            env.insert("STATE_DIR".to_string(), state.path().display().to_string());
            let commands = CommandConfig {
                command: vec!["sh".into(), "-c".into(), script.into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            };

            let selected = select_test_command(dir.path(), &commands, &cached_mutation);
            let cache_ctx = selected.cache_context(
                &commands.build_command,
                commands.build_command_explicit,
                &commands.sandbox_command,
                &env,
            );
            let cache_ctx = format!(
                "{cache_ctx};context={:016x}",
                cache_context_fingerprint(dir.path())
            );
            let cached_content = std::fs::read(&cached_file).unwrap();
            let key = CacheKey::new(
                &cached_content,
                &cache_identity(dir.path(), &cached_mutation),
                &cached_mutation.description,
                &cache_ctx,
            );
            cache::store(dir.path(), &key, cached_result);

            let runner = TestRunner {
                commands,
                parallelism: 2,
                project_root: dir.path().to_path_buf(),
                verbose: false,
                show_output: false,
                max_tested: Some(1),
                early_stop: Default::default(),
                respect_workspace_ignores: true,
                env,
                incremental_history: true,
                force_rerun: false,
                cancelled: Arc::new(AtomicBool::new(false)),
            };

            let report = runner.run(vec![cached_mutation, uncached_mutation]).report;
            let runs: usize = std::fs::read_to_string(state.path().join("runs"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();

            assert_eq!(report.total, 2);
            assert_eq!(report.results[0].1, cached_result);
            assert_eq!(report.results[1].1, MutationResult::Survived);
            assert_eq!(
                runs, 1,
                "cache hits should not consume the max_tested execution budget"
            );
        }

        run_case(MutationResult::Survived);
        run_case(MutationResult::Killed);
    }

    #[cfg(unix)]
    #[test]
    fn max_tested_does_not_count_build_errors() {
        let dir = tempfile::tempdir().unwrap();

        let mutations: Vec<Mutation> = ["first.txt", "second.txt", "third.txt"]
            .into_iter()
            .enumerate()
            .map(|(i, file_name)| {
                let file = dir.path().join(file_name);
                std::fs::write(&file, b"hello world").unwrap();
                let mut mutation = make_test_mutation(&file);
                mutation.id = i as u32;
                mutation.description = format!("build classification {i}");
                if i < 2 {
                    mutation.replacement = "build_error".into();
                } else {
                    mutation.replacement = "ok".into();
                }
                mutation
            })
            .collect();

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["true".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![
                    "sh".into(),
                    "-c".into(),
                    "grep -q build_error *.txt && exit 1; exit 0".into(),
                ],
                sandbox_command: vec![],
                build_command_explicit: true,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 4,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: Some(1),
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;

        assert_eq!(report.total, 3);
        assert_eq!(report.build_errors, 2);
        assert_eq!(report.survived, 1);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_queue_preserves_report_order_and_caps_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        let mutations: Vec<Mutation> = (0..5)
            .map(|i| {
                let file = dir.path().join(format!("test{i}.txt"));
                std::fs::write(&file, b"hello world").unwrap();
                let mut mutation = make_test_mutation(&file);
                mutation.id = (100 - i) as u32;
                mutation.description = format!("queue mutation {i}");
                mutation
            })
            .collect();
        let expected_ids: Vec<u32> = mutations.iter().map(|mutation| mutation.id).collect();

        let script = r#"
lock="$STATE_DIR/lock"
while ! mkdir "$lock" 2>/dev/null; do sleep 0.01; done
active=0
if [ -f "$STATE_DIR/active" ]; then active=$(cat "$STATE_DIR/active"); fi
active=$((active + 1))
printf '%s\n' "$active" > "$STATE_DIR/active"
max=0
if [ -f "$STATE_DIR/max" ]; then max=$(cat "$STATE_DIR/max"); fi
if [ "$active" -gt "$max" ]; then printf '%s\n' "$active" > "$STATE_DIR/max"; fi
rmdir "$lock"
sleep 0.15
while ! mkdir "$lock" 2>/dev/null; do sleep 0.01; done
active=$(cat "$STATE_DIR/active")
active=$((active - 1))
printf '%s\n' "$active" > "$STATE_DIR/active"
rmdir "$lock"
"#;
        let mut env = HashMap::new();
        env.insert("STATE_DIR".to_string(), state.path().display().to_string());

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sh".into(), "-c".into(), script.into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 2,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env,
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;
        let actual_ids: Vec<u32> = report
            .results
            .iter()
            .map(|(mutation, _)| mutation.id)
            .collect();
        let max_active: usize = std::fs::read_to_string(state.path().join("max"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        assert_eq!(report.total, 5);
        assert_eq!(report.survived, 5);
        assert_eq!(actual_ids, expected_ids);
        assert!(
            max_active <= 2,
            "runner should not execute more commands concurrently than parallelism"
        );
    }

    #[test]
    fn report_aggregates_results_correctly() {
        let dir = tempfile::tempdir().unwrap();

        let survived_file = dir.path().join("survived.txt");
        std::fs::write(&survived_file, b"hello world").unwrap();
        let mut m_survived = make_test_mutation(&survived_file);
        m_survived.language = "survived_lang".into();

        let killed_file = dir.path().join("killed.txt");
        std::fs::write(&killed_file, b"hello world").unwrap();
        let mut m_killed = make_test_mutation(&killed_file);
        m_killed.id = 2;
        m_killed.language = "killed_lang".into();

        let mut lang_cmds = HashMap::new();
        lang_cmds.insert("survived_lang".into(), vec!["true".into()]);
        lang_cmds.insert("killed_lang".into(), vec!["false".into()]);

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["true".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: lang_cmds,
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 2,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(vec![m_survived, m_killed]).report;
        assert_eq!(report.total, 2);
        assert_eq!(report.killed, 1);
        assert_eq!(report.survived, 1);
        assert_eq!(report.timeout, 0);
        assert_eq!(report.build_errors, 0);
    }

    #[test]
    fn language_commands_override_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        let mut mutation = make_test_mutation(&file);
        mutation.language = "go".into();

        let mut lang_cmds = HashMap::new();
        lang_cmds.insert("go".into(), vec!["false".into()]); // "false" = always fails = killed

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["true".into()], // default would survive
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: lang_cmds,
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(vec![mutation]).report;
        assert_eq!(report.killed, 1, "should use language-specific command");
        assert_eq!(
            report.test_command, None,
            "mixed language-specific commands should not report the default command as global context"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mutations_on_same_file_run_in_isolated_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        // Each mutation has a unique description so cache can't short-circuit
        let mutations: Vec<Mutation> = ["alpha", "bravo", "charlie", "delta"]
            .into_iter()
            .enumerate()
            .map(|(i, replacement)| Mutation {
                id: i as u32,
                file: file.clone(),
                language: String::new(),
                line: 1,
                column: 1,
                operator: "test".into(),
                description: format!("unique mutation {i}"),
                original: "hello".into(),
                replacement: replacement.into(),
                byte_range: 0..5,
            })
            .collect();

        let barrier = tempfile::tempdir().unwrap();
        let script = format!(
            r#"value="$(cat test.txt)"
case "$value" in
  "alpha world"|"bravo world"|"charlie world"|"delta world") ;;
  *) exit 1 ;;
esac
printf '%s\n' "$PWD" > "{barrier}/$value"
while [ "$(find "{barrier}" -type f | wc -l)" -lt 4 ]; do sleep 0.1; done"#,
            barrier = barrier.path().display()
        );

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sh".into(), "-c".into(), script],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 4,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;
        assert_eq!(report.total, 4);
        assert_eq!(report.survived, 4);
        assert_eq!(report.killed, 0);
        assert_eq!(report.timeout, 0);
        assert_eq!(report.build_errors, 0);
        let roots: std::collections::HashSet<String> = std::fs::read_dir(barrier.path())
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        assert_eq!(
            roots.len(),
            4,
            "expected one distinct workspace root per same-file mutation"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn mutations_on_different_files_run_in_isolated_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        std::fs::write(&first, b"hello world").unwrap();
        std::fs::write(&second, b"hello world").unwrap();

        let mutations: Vec<Mutation> = [first.clone(), second.clone()]
            .into_iter()
            .enumerate()
            .map(|(i, file)| Mutation {
                id: i as u32,
                file,
                language: String::new(),
                line: 1,
                column: 1,
                operator: "test".into(),
                description: format!("different file mutation {i}"),
                original: "hello".into(),
                replacement: "world".into(),
                byte_range: 0..5,
            })
            .collect();

        let script = r#"first=$(cat first.txt); second=$(cat second.txt)
if { [ "$first" = "world world" ] && [ "$second" = "hello world" ]; } ||
   { [ "$first" = "hello world" ] && [ "$second" = "world world" ]; }; then
  exit 0
fi
echo "unexpected workspace contents: first=$first second=$second"
exit 1"#
            .to_string();

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sh".into(), "-c".into(), script],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 4,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;
        assert_eq!(report.total, 2);
        assert_eq!(
            report.killed, 0,
            "each workspace should contain exactly one active mutation"
        );
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "hello world");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn reused_workspace_is_reset_between_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        std::fs::write(&first, b"hello world").unwrap();
        std::fs::write(&second, b"hello world").unwrap();

        let mut first_mutation = make_test_mutation(&first);
        first_mutation.description = "first side-effect mutation".into();
        let mut second_mutation = make_test_mutation(&second);
        second_mutation.id = 2;
        second_mutation.description = "second side-effect mutation".into();

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "test ! -f side_effect && touch side_effect".into(),
                ],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(vec![first_mutation, second_mutation]).report;

        assert_eq!(report.total, 2);
        assert_eq!(report.survived, 2);
        assert_eq!(report.killed, 0);
    }

    #[test]
    fn per_language_timeout_overrides_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        // "slow_lang" gets a 100ms timeout → sleep 1s will timeout
        // "fast_lang" uses the default 5s → sleep 0.01s will survive
        let mut language_timeouts = HashMap::new();
        language_timeouts.insert("slow_lang".into(), Duration::from_millis(100));

        let mut m_slow = make_test_mutation(&file);
        m_slow.language = "slow_lang".into();
        m_slow.description = "slow".into();

        let mut m_fast = make_test_mutation(&file);
        m_fast.id = 2;
        m_fast.file = dir.path().join("test2.txt");
        std::fs::write(&m_fast.file, b"hello world").unwrap();
        m_fast.language = "fast_lang".into();
        m_fast.description = "fast".into();

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sleep".into(), "1".into()],
                force_default_command: false,
                force_default_timeout: false,
                project_commands: vec![],
                language_commands: HashMap::new(),
                build_command: vec![],
                sandbox_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts,
                test_selection: None,
            },
            parallelism: 2,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            early_stop: Default::default(),
            respect_workspace_ignores: true,
            env: HashMap::new(),
            incremental_history: true,
            force_rerun: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(vec![m_slow, m_fast]).report;
        let slow_result = report
            .results
            .iter()
            .find(|(m, _)| m.description == "slow")
            .map(|(_, r)| *r);
        let fast_result = report
            .results
            .iter()
            .find(|(m, _)| m.description == "fast")
            .map(|(_, r)| *r);

        assert_eq!(slow_result, Some(MutationResult::Timeout));
        assert_eq!(fast_result, Some(MutationResult::Survived));
    }
}
