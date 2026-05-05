// Parallel test execution with timeouts

use crate::cache::{self, CacheKey};
use crate::{Mutation, MutationReport, MutationResult};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURED_OUTPUT_LIMIT: usize = 1024 * 1024;

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

    if let Some(test_selection) = &commands.test_selection
        && let Some(tests) = test_selection.tests_for(project_root, mutation)
    {
        argv = narrow_go_test_command(argv, tests);
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

    Ok(WorkspaceCopy {
        _tempdir: tempdir,
        root,
    })
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
    max_tested: Option<usize>,
    counter: &'a AtomicUsize,
    scheduled_counter: &'a AtomicUsize,
    tested_counter: &'a AtomicUsize,
    cancelled: &'a AtomicBool,
}

fn run_queued_mutation(
    queued: QueuedMutation,
    shared: RunShared<'_>,
) -> Option<(usize, Mutation, MutationResult)> {
    let QueuedMutation { index, mutation } = queued;

    // Stop if cancelled (Ctrl+C) or enough mutations have been tested.
    if shared.cancelled.load(Ordering::Relaxed) {
        return None;
    }
    if let Some(max) = shared.max_tested {
        let reserved = shared
            .scheduled_counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < max).then_some(current + 1)
            })
            .is_ok();
        if !reserved {
            return None;
        }
    }

    let selected_test = select_test_command(shared.project_root, shared.commands, &mutation);
    let cache_ctx = selected_test.cache_context(
        shared.build_command,
        shared.build_command_explicit,
        shared.env,
    );

    let original_target = ResolvedMutation::new(shared.project_root, &mutation);

    // Check cache before acquiring a workspace slot.
    let file_content = original_target
        .file_path
        .as_ref()
        .ok()
        .and_then(|file_path| fs::read(file_path).ok());
    let cache_key = file_content.as_ref().map(|content| {
        CacheKey::new(
            content,
            &cache_identity(shared.project_root, &mutation),
            &mutation.description,
            &cache_ctx,
        )
    });
    if let Some(ref key) = cache_key
        && let Some(result) = cache::lookup(shared.project_root, key)
    {
        record_progress(&shared, &mutation, result, None, true);
        return Some((index, mutation, result));
    }

    let outcome = {
        let workspace_slot = shared.workspace_pool.acquire();
        let workspace_root = workspace_slot.root().to_path_buf();
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
        )
    };

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
    Some((index, mutation, result))
}

#[allow(clippy::manual_is_multiple_of)]
fn record_progress(
    shared: &RunShared<'_>,
    mutation: &Mutation,
    result: MutationResult,
    test_output: Option<&str>,
    cached: bool,
) {
    if result != MutationResult::BuildError {
        shared.tested_counter.fetch_add(1, Ordering::Release);
    }

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

    if shared.show_output
        && result == MutationResult::Survived
        && let Some(output) = test_output
    {
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

impl TestRunner {
    #[allow(clippy::manual_is_multiple_of)]
    pub fn run(&self, mutations: Vec<Mutation>) -> MutationReport {
        let start = Instant::now();
        let total = mutations.len();
        if total == 0 {
            return self.report_from_results(Vec::new(), start.elapsed());
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let scheduled_counter = Arc::new(AtomicUsize::new(0));
        let tested_counter = Arc::new(AtomicUsize::new(0));
        let verbose = self.verbose;
        let is_tty = std::io::stderr().is_terminal();

        let workspace_pool_result = if self.respect_workspace_ignores {
            WorkspacePool::new(&self.project_root, self.parallelism)
        } else {
            WorkspacePool::new_with_options(&self.project_root, self.parallelism, false)
        };
        let workspace_pool = match workspace_pool_result {
            Ok(pool) => Arc::new(pool),
            Err(e) => {
                eprintln!("warning: could not create isolated mutation workspaces: {e}");
                let results = mutations
                    .into_iter()
                    .map(|mutation| (mutation, MutationResult::BuildError))
                    .collect();
                return self.report_from_results(results, start.elapsed());
            }
        };

        let project_root = Arc::new(self.project_root.clone());
        let build_command = Arc::new(self.commands.build_command.clone());
        let build_command_explicit = self.commands.build_command_explicit;
        let queue = Arc::new(Mutex::new(
            mutations.into_iter().enumerate().collect::<VecDeque<_>>(),
        ));
        let results = Arc::new(Mutex::new(Vec::new()));
        let worker_count = workspace_pool.len().min(total).max(1);

        thread::scope(|scope| {
            for _ in 0..worker_count {
                let queue = queue.clone();
                let results = results.clone();
                let workspace_pool = workspace_pool.clone();
                let project_root = project_root.clone();
                let build_command = build_command.clone();
                let counter = counter.clone();
                let scheduled_counter = scheduled_counter.clone();
                let tested_counter = tested_counter.clone();
                let cancelled = self.cancelled.clone();
                let commands = &self.commands;
                let env = &self.env;
                let max_tested = self.max_tested;
                let show_output = self.show_output;

                scope.spawn(move || {
                    loop {
                        if cancelled.load(Ordering::Relaxed) {
                            break;
                        }

                        let Some((index, mutation)) = queue
                            .lock()
                            .expect("mutation queue mutex poisoned")
                            .pop_front()
                        else {
                            break;
                        };

                        if let Some(result) = run_queued_mutation(
                            QueuedMutation { index, mutation },
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
                                max_tested,
                                counter: &counter,
                                scheduled_counter: &scheduled_counter,
                                tested_counter: &tested_counter,
                                cancelled: &cancelled,
                            },
                        ) {
                            results
                                .lock()
                                .expect("mutation results mutex poisoned")
                                .push(result);
                        }
                    }
                });
            }
        });

        let mut indexed_results = Arc::try_unwrap(results)
            .expect("all result handles should be dropped")
            .into_inner()
            .expect("mutation results mutex poisoned");
        indexed_results.sort_by_key(|(index, _, _)| *index);
        let all_results = indexed_results
            .into_iter()
            .map(|(_, mutation, result)| (mutation, result))
            .collect();

        // Clear progress line on TTY
        if !verbose && is_tty {
            eprint!("\r                                        \r");
            let _ = std::io::stderr().flush();
        }

        self.report_from_results(all_results, start.elapsed())
    }

    fn report_from_results(
        &self,
        all_results: Vec<(Mutation, MutationResult)>,
        duration: Duration,
    ) -> MutationReport {
        let total = all_results.len();
        let killed = all_results
            .iter()
            .filter(|(_, r)| *r == MutationResult::Killed)
            .count();
        let survived = all_results
            .iter()
            .filter(|(_, r)| *r == MutationResult::Survived)
            .count();
        let timeout_count = all_results
            .iter()
            .filter(|(_, r)| *r == MutationResult::Timeout)
            .count();
        let build_errors = all_results
            .iter()
            .filter(|(_, r)| *r == MutationResult::BuildError)
            .count();

        MutationReport {
            results: all_results,
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

struct MutationOutcome {
    result: MutationResult,
    test_output: Option<String>,
}

struct BuildCommand<'a> {
    argv: &'a [String],
    explicit: bool,
}

fn mutation_file_path(project_root: &Path, mutation_file: &Path) -> PathBuf {
    if mutation_file.is_absolute() {
        mutation_file.to_path_buf()
    } else {
        project_root.join(mutation_file)
    }
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

fn run_single_mutation(
    command: &[String],
    build_command: BuildCommand<'_>,
    timeout: Duration,
    project_root: &Path,
    target: ResolvedMutation<'_>,
    capture_output: bool,
    env: &HashMap<String, String>,
) -> MutationOutcome {
    let mutation = target.mutation;
    let file_path = match target.file_path {
        Ok(path) => path,
        Err(()) => {
            return MutationOutcome {
                result: MutationResult::BuildError,
                test_output: None,
            };
        }
    };

    // Read original content
    let original = match std::fs::read(&file_path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("warning: could not read {}: {e}", file_path.display());
            return MutationOutcome {
                result: MutationResult::BuildError,
                test_output: None,
            };
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
        return MutationOutcome {
            result: MutationResult::BuildError,
            test_output: None,
        };
    }
    mutated.splice(range, mutation.replacement.as_bytes().iter().copied());

    if let Err(e) = write_workspace_file(&file_path, &mutated) {
        eprintln!("warning: could not write {}: {e}", file_path.display());
        return MutationOutcome {
            result: MutationResult::BuildError,
            test_output: None,
        };
    }

    // Explicit build check: skip expensive test if mutation doesn't compile.
    if build_command.explicit && !build_command.argv.is_empty() {
        let build_outcome = run_command(build_command.argv, project_root, timeout, false, env);
        if build_outcome.result != MutationResult::Survived {
            return MutationOutcome {
                result: MutationResult::BuildError,
                test_output: None,
            };
        }
    }

    // Run test command; guard will restore the file on drop
    run_command(command, project_root, timeout, capture_output, env)
}

fn run_command(
    command: &[String],
    cwd: &Path,
    timeout_dur: Duration,
    capture_output: bool,
    env: &HashMap<String, String>,
) -> MutationOutcome {
    if command.is_empty() {
        return MutationOutcome {
            result: MutationResult::BuildError,
            test_output: None,
        };
    }

    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(cwd).envs(env);

    if capture_output {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: could not spawn command {:?}: {e}", &command[0]);
            return MutationOutcome {
                result: MutationResult::BuildError,
                test_output: None,
            };
        }
    };

    let stdout_reader = if capture_output {
        child.stdout.take().map(spawn_output_reader)
    } else {
        None
    };
    let stderr_reader = if capture_output {
        child.stderr.take().map(spawn_output_reader)
    } else {
        None
    };

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= timeout_dur {
                    let _ = child.kill();
                    let _ = child.wait();
                    return MutationOutcome {
                        result: MutationResult::Timeout,
                        test_output: None,
                    };
                }
                let remaining = timeout_dur.saturating_sub(started.elapsed());
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return MutationOutcome {
                    result: MutationResult::BuildError,
                    test_output: None,
                };
            }
        }
    };

    let stdout = match recv_output_reader(stdout_reader, started, timeout_dur) {
        Ok(bytes) => bytes,
        Err(result) => return result,
    };
    let stderr = match recv_output_reader(stderr_reader, started, timeout_dur) {
        Ok(bytes) => bytes,
        Err(result) => return result,
    };

    let result = if status.success() {
        MutationResult::Survived
    } else {
        MutationResult::Killed
    };
    let test_output = if capture_output {
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

    MutationOutcome {
        result,
        test_output,
    }
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn spawn_output_reader<R>(mut reader: R) -> mpsc::Receiver<CapturedOutput>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut truncated = false;
        let mut buffer = [0; 8192];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let available = CAPTURED_OUTPUT_LIMIT.saturating_sub(bytes.len());
                    let keep = n.min(available);
                    if keep > 0 {
                        bytes.extend_from_slice(&buffer[..keep]);
                    }
                    if keep < n {
                        truncated = true;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }

        let _ = tx.send(CapturedOutput { bytes, truncated });
    });
    rx
}

fn recv_output_reader(
    reader: Option<mpsc::Receiver<CapturedOutput>>,
    started: Instant,
    timeout_dur: Duration,
) -> Result<CapturedOutput, MutationOutcome> {
    let Some(reader) = reader else {
        return Ok(CapturedOutput {
            bytes: Vec::new(),
            truncated: false,
        });
    };
    let Some(remaining) = timeout_dur.checked_sub(started.elapsed()) else {
        return Err(MutationOutcome {
            result: MutationResult::Timeout,
            test_output: None,
        });
    };
    reader.recv_timeout(remaining).map_err(|_| MutationOutcome {
        result: MutationResult::Timeout,
        test_output: None,
    })
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
        );

        assert_eq!(outcome.result, MutationResult::Timeout);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
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
                argv: &["false".to_string()], // build fails
                explicit: true,
            },
            Duration::from_secs(5),
            dir.path(),
            ResolvedMutation::new(dir.path(), &mutation),
            false,
            &HashMap::new(),
        );

        assert_eq!(outcome.result, MutationResult::BuildError);
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

    #[test]
    fn max_tested_limits_mutations() {
        let (dir, file, _) = make_test_setup();

        let mutations: Vec<Mutation> = (0..5)
            .map(|i| {
                let mut m = make_test_mutation(&file);
                m.id = i;
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

        let report = runner.run(mutations);
        assert_eq!(report.total, 2, "should stop after max_tested");
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

        let report = runner.run(vec![m_survived, m_killed]);
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

        let report = runner.run(vec![mutation]);
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

        let report = runner.run(mutations);
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

        let report = runner.run(mutations);
        assert_eq!(report.total, 2);
        assert_eq!(
            report.killed, 0,
            "each workspace should contain exactly one active mutation"
        );
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "hello world");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "hello world");
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

        let report = runner.run(vec![m_slow, m_fast]);
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
