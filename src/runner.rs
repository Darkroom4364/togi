// Parallel test execution with timeouts

use crate::cache::{self, CacheKey};
use crate::{BuildErrorDiagnostic, Mutation, MutationReport, MutationResult};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::hash::Hasher;
use std::io::{IsTerminal, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
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
    tests_by_line: HashMap<(String, usize), Vec<String>>,
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
        self.tests_by_line.insert(
            (normalized_cache_path(project_root, file.as_ref()), line),
            tests,
        );
    }

    fn tests_for(&self, project_root: &Path, mutation: &Mutation) -> Option<&[String]> {
        self.tests_by_line
            .get(&(
                normalized_cache_path(project_root, &mutation.file),
                mutation.line,
            ))
            .map(Vec::as_slice)
            .filter(|tests| !tests.is_empty())
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
    /// Whether workspace copies should honor `.ignore`/`.gitignore` rules.
    pub respect_workspace_ignores: bool,
    /// Extra environment variables passed to every spawned command.
    pub env: HashMap<String, String>,
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
struct SelectedTestCommand {
    argv: Vec<String>,
    timeout: Duration,
}

impl SelectedTestCommand {
    fn cache_context(
        &self,
        build_command: &[String],
        build_command_explicit: bool,
        env: &HashMap<String, String>,
    ) -> String {
        let build_str = if build_command_explicit {
            format!("{build_command:?}")
        } else {
            String::new()
        };
        let mut env_parts: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        env_parts.sort();
        format!(
            "test={:?};build={};timeout={};env={}",
            self.argv,
            build_str,
            self.timeout.as_millis(),
            env_parts.join(",")
        )
    }
}

fn select_test_command(
    project_root: &Path,
    commands: &CommandConfig,
    mutation: &Mutation,
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

    if let Some(test_selection) = &commands.test_selection {
        if let Some(tests) = test_selection.tests_for(project_root, mutation) {
            argv = narrow_go_test_command(argv, tests);
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

fn escape_go_test_regex(test: &str) -> String {
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
}

impl WorkspaceCopy {
    pub(crate) fn root(&self) -> &Path {
        &self.root
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

fn is_cache_context_file(relative: &Path) -> bool {
    let path_key = normalized_cache_path(Path::new(""), relative).to_ascii_lowercase();
    let file_name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if matches!(
        path_key.as_str(),
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
    fs::create_dir(&root)?;
    populate_workspace(project_root, &root, respect_ignores)?;

    Ok(WorkspaceCopy {
        _tempdir: tempdir,
        root,
    })
}

fn reset_workspace(
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

        Ok(Self {
            slots: Arc::new(copies),
            free_slots: Arc::new((Mutex::new(free_slots), Condvar::new())),
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
        WorkspaceSlot {
            slots: self.slots.clone(),
            free_slots: self.free_slots.clone(),
            index,
        }
    }
}

pub(crate) struct WorkspaceSlot {
    slots: Arc<Vec<WorkspaceCopy>>,
    free_slots: Arc<(Mutex<VecDeque<usize>>, Condvar)>,
    index: usize,
}

impl WorkspaceSlot {
    pub(crate) fn root(&self) -> &Path {
        self.slots[self.index].root()
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
    respect_workspace_ignores: bool,
    cache_context_fingerprint: u64,
    source_contents: &'a SourceContentCache,
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

    let selected_test = select_test_command(shared.project_root, shared.commands, &mutation);
    let cache_ctx = selected_test.cache_context(
        shared.build_command,
        shared.build_command_explicit,
        shared.env,
    );
    let cache_ctx = format!(
        "{cache_ctx};context={:016x}",
        shared.cache_context_fingerprint
    );

    // Check cache before acquiring a workspace slot.
    let cache_key = shared
        .source_contents
        .content_for(shared.project_root, &mutation.file)
        .map(|content| {
            CacheKey::new(
                &content,
                &cache_identity(shared.project_root, &mutation),
                &mutation.description,
                &cache_ctx,
            )
        });
    if let Some(ref key) = cache_key {
        if let Some(result) = cache::lookup(shared.project_root, key) {
            reservation.release();
            record_progress(&shared, &mutation, result, None, true);
            let diagnostic = cached_build_error_diagnostic(&mutation, "regular", result);
            return Some((index, MutationRunRecord::new(mutation, result, diagnostic)));
        }
    }

    let outcome = {
        let workspace_slot = shared.workspace_pool.acquire();
        let workspace_root = workspace_slot.root().to_path_buf();
        if let Err(e) = reset_workspace(
            shared.project_root,
            &workspace_root,
            shared.respect_workspace_ignores,
        ) {
            eprintln!(
                "warning: could not reset isolated mutation workspace {}: {e}",
                workspace_root.display()
            );
            reservation.release();
            record_progress(&shared, &mutation, MutationResult::BuildError, None, false);
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
        let workspace_target =
            ResolvedMutation::new_for_execution(shared.project_root, &workspace_root, &mutation);
        run_single_mutation(
            &selected_test.argv,
            BuildCommand {
                argv: shared.build_command,
                explicit: shared.build_command_explicit,
            },
            selected_test.timeout,
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

    if let Some(ref key) = cache_key {
        cache::store(shared.project_root, key, outcome.result);
    }

    let result = outcome.result;
    record_progress(
        &shared,
        &mutation,
        result,
        outcome.test_output.as_deref(),
        false,
    );
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
        self.run_regular(mutations)
    }

    pub fn run_with_schemata(&self, mutations: Vec<Mutation>) -> RunOutcome {
        let start = Instant::now();
        if mutations.is_empty() {
            return self.outcome_from_records(Vec::new(), start.elapsed());
        }

        let index_by_id: HashMap<u32, usize> = mutations
            .iter()
            .enumerate()
            .map(|(index, mutation)| (mutation.id, index))
            .collect();
        let plan = crate::schemata::plan(&self.project_root, mutations);
        let mut schema_by_language = HashMap::<String, Vec<crate::schemata::SchemaMutation>>::new();
        let mut fallback_mutations = Vec::new();

        for schema_mutation in plan.selected {
            match schema_mutation.mutation.language.as_str() {
                "c" | "cpp" | "go" | "java" | "rust" => {
                    schema_by_language
                        .entry(schema_mutation.mutation.language.clone())
                        .or_default()
                        .push(schema_mutation);
                }
                _ => fallback_mutations.push(schema_mutation.mutation),
            }
        }
        fallback_mutations.extend(plan.fallback.into_iter().map(|fallback| fallback.mutation));

        if schema_by_language.is_empty() {
            return self.run_regular(fallback_mutations);
        }

        let mut all_records = Vec::new();
        for (language, schema_mutations) in schema_by_language {
            match self.run_schema_mutations(&language, &schema_mutations) {
                Ok(records) => all_records.extend(records),
                Err(err) => {
                    eprintln!("warning: could not run {language} schemata: {err} — falling back");
                    fallback_mutations.extend(
                        schema_mutations
                            .into_iter()
                            .map(|schema_mutation| schema_mutation.mutation),
                    );
                }
            }
        }

        if !self.cancelled.load(Ordering::Acquire) && !fallback_mutations.is_empty() {
            let fallback = self.run_regular(fallback_mutations);
            all_records.extend(records_from_report(fallback.report));
        }

        all_records.sort_by_key(|record| {
            index_by_id
                .get(&record.mutation.id)
                .copied()
                .unwrap_or(usize::MAX)
        });
        self.outcome_from_records(all_records, start.elapsed())
    }

    #[allow(clippy::manual_is_multiple_of)]
    fn run_regular(&self, mutations: Vec<Mutation>) -> RunOutcome {
        let start = Instant::now();
        let total = mutations.len();
        if total == 0 {
            return self.outcome_from_records(Vec::new(), start.elapsed());
        }
        if self.cancelled.load(Ordering::Acquire) {
            return self.outcome_from_records(Vec::new(), start.elapsed());
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

                scope.spawn(move || {
                    loop {
                        if cancelled.load(Ordering::Relaxed) {
                            break;
                        }

                        let Some(reservation) =
                            TestSlotReservation::try_reserve(max_tested, &tested_counter)
                        else {
                            break;
                        };
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

                        let outcome = catch_unwind(AssertUnwindSafe(|| {
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
                                    respect_workspace_ignores: self.respect_workspace_ignores,
                                    cache_context_fingerprint: cache_context_hash,
                                    source_contents,
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

        self.outcome_from_records(all_records, start.elapsed())
    }

    fn run_schema_mutations(
        &self,
        language: &str,
        schema_mutations: &[crate::schemata::SchemaMutation],
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
        for schema_mutation in schema_mutations {
            if self.cancelled.load(Ordering::Acquire) {
                break;
            }
            let Some(reservation) =
                TestSlotReservation::try_reserve(self.max_tested, &tested_counter)
            else {
                break;
            };
            let mutation = &schema_mutation.mutation;
            let selected = select_test_command(&self.project_root, &self.commands, mutation);
            let argv = if language == "go" {
                force_go_no_test_cache(selected.argv)
            } else {
                selected.argv
            };
            let mut env = self.env.clone();
            env.insert("TOGI_MUTANT".to_string(), mutation.id.to_string());
            let cache_selected = SelectedTestCommand {
                argv: argv.clone(),
                timeout: selected.timeout,
            };
            let cache_ctx = cache_selected.cache_context(
                &self.commands.build_command,
                self.commands.build_command_explicit,
                &env,
            );
            let cache_ctx = format!("{cache_ctx};context={cache_context_hash:016x}");
            let cache_key = source_contents
                .content_for(&self.project_root, &mutation.file)
                .map(|content| {
                    CacheKey::new(
                        &content,
                        &cache_identity(&self.project_root, mutation),
                        &mutation.description,
                        &cache_ctx,
                    )
                });
            if let Some(ref key) = cache_key {
                if let Some(result) = cache::lookup(&self.project_root, key) {
                    reservation.release();
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
            }

            let mut cacheable = true;
            let outcome = if let Err(e) = reset_workspace(
                &self.project_root,
                workspace.root(),
                self.respect_workspace_ignores,
            ) {
                cacheable = false;
                MutationOutcome::build_error_with(
                    "schema_workspace_reset",
                    vec![],
                    format!(
                        "could not reset schema workspace {}: {e}",
                        workspace.root().display()
                    ),
                )
            } else if let Err(e) =
                apply_schema_rewrites_to_workspace(&self.project_root, workspace.root(), &rewrites)
            {
                cacheable = false;
                MutationOutcome::build_error_with(
                    "schema_rewrite",
                    vec![],
                    format!("could not apply schema rewrites: {e}"),
                )
            } else if self.commands.build_command_explicit
                && !self.commands.build_command.is_empty()
            {
                let build = run_command(
                    &self.commands.build_command,
                    workspace.root(),
                    self.commands.timeout,
                    true,
                    &self.env,
                    &self.cancelled,
                );
                if build.cancelled {
                    break;
                }
                if build.result != MutationResult::Survived {
                    MutationOutcome::build_error_with(
                        "schema_build",
                        self.commands.build_command.clone(),
                        build_error_message_from_outcome(
                            "schema build command",
                            &self.commands.build_command,
                            workspace.root(),
                            &build,
                        ),
                    )
                } else {
                    run_command(
                        &argv,
                        workspace.root(),
                        selected.timeout,
                        self.show_output,
                        &env,
                        &self.cancelled,
                    )
                }
            } else {
                run_command(
                    &argv,
                    workspace.root(),
                    selected.timeout,
                    self.show_output,
                    &env,
                    &self.cancelled,
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
            if cacheable {
                if let Some(ref key) = cache_key {
                    cache::store(&self.project_root, key, outcome.result);
                }
            }
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
        RunOutcome {
            report: self.report_from_records(all_records, duration),
            cancelled: self.cancelled.load(Ordering::Acquire),
        }
    }

    fn report_from_records(
        &self,
        all_records: Vec<MutationRunRecord>,
        duration: Duration,
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
            duration,
            test_command: if self.commands.language_commands.is_empty()
                && self.commands.project_commands.is_empty()
            {
                Some(self.commands.command.clone())
            } else {
                None
            },
            build_command: if self.commands.build_command_explicit {
                self.commands.build_command.clone()
            } else {
                vec![]
            },
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
        project_root,
        timeout,
        capture_output,
        env,
        cancelled,
    )
}

fn run_command(
    command: &[String],
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
            eprintln!("warning: could not spawn command {:?}: {e}", &command[0]);
            return MutationOutcome::build_error_with(
                "command",
                command.to_vec(),
                format!("could not spawn command {:?}: {e}", &command[0]),
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
        assert_eq!(
            cache_context_fingerprint(dir.path()),
            initial,
            "regular source files are already covered by per-mutation content hashes"
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
        assert_eq!(
            git_cache_context_fingerprint(root).expect("dirty source should still use git"),
            clean
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

    fn test_command_config() -> CommandConfig {
        CommandConfig {
            command: vec!["cargo".into(), "test".into()],
            force_default_command: false,
            force_default_timeout: false,
            project_commands: vec![],
            language_commands: HashMap::new(),
            build_command: vec![],
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
        };
        let ambiguous = SelectedTestCommand {
            argv: vec!["cargo".into(), "test".into()],
            timeout: Duration::from_secs(2),
        };

        assert_ne!(
            selected.cache_context(&[], false, &HashMap::new()),
            ambiguous.cache_context(&[], false, &HashMap::new())
        );
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
    fn select_test_command_falls_back_for_unsupported_command() {
        let tmp = tempfile::tempdir().unwrap();
        let mut selection = TestSelectionConfig::new();
        selection.insert(
            tmp.path(),
            Path::new("src/lib.rs"),
            3,
            vec!["test_name".into()],
        );

        let mut commands = test_command_config();
        commands.command = vec!["cargo".into(), "test".into()];
        commands.test_selection = Some(selection);

        let mut mutation = make_test_mutation(Path::new("src/lib.rs"));
        mutation.line = 3;

        let selected = select_test_command(tmp.path(), &commands, &mutation);

        assert_eq!(selected.argv, vec!["cargo", "test"]);
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

        reset_workspace(&root, &workspace, true).unwrap();

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

        reset_workspace(&root, &workspace, true).unwrap();

        assert!(workspace.join("src/lib.rs").exists());
        assert!(!workspace.join("side-effect").exists());
        assert!(!workspace.join("target/debug/cache").exists());
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
        assert_ne!(first.root(), second.root());
        assert!(first.root().join("src/lib.rs").exists());
        assert!(second.root().join("src/lib.rs").exists());

        let first_root = first.root().to_path_buf();
        let second_root = second.root().to_path_buf();
        drop(second);

        let third = pool.acquire();
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            build_command_explicit: false,
            timeout: Duration::from_secs(5),
            language_timeouts: HashMap::new(),
            test_selection: None,
        };
        let mut cache_env = env.clone();
        cache_env.insert("TOGI_MUTANT".to_string(), first.id.to_string());
        let selected = select_test_command(dir.path(), &commands, &first);
        let cache_selected = SelectedTestCommand {
            argv: selected.argv,
            timeout: selected.timeout,
        };
        let cache_ctx = cache_selected.cache_context(
            &commands.build_command,
            commands.build_command_explicit,
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
            respect_workspace_ignores: true,
            env,
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: {
                let mut env = HashMap::new();
                env.insert(
                    "CARGO_TARGET_DIR".to_string(),
                    dir.path().join("target").display().to_string(),
                );
                env
            },
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).report;
        assert_eq!(report.total, 2, "should stop after max_tested");
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
            respect_workspace_ignores: true,
            env,
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
                test_selection: None,
            };

            let selected = select_test_command(dir.path(), &commands, &cached_mutation);
            let cache_ctx = selected.cache_context(
                &commands.build_command,
                commands.build_command_explicit,
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
                respect_workspace_ignores: true,
                env,
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env,
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
            respect_workspace_ignores: true,
            env: HashMap::new(),
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
