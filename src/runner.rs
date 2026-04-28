// Parallel test execution with timeouts

use crate::cache::{self, CacheKey};
use crate::{Mutation, MutationReport, MutationResult};
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Write `data` to `path` atomically: write to a temp file in the same
/// directory, fsync, then rename over the target.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    Ok(())
}

/// Command configuration: what to run and how long to wait.
pub struct CommandConfig {
    pub command: Vec<String>,
    pub language_commands: HashMap<String, Vec<String>>,
    pub build_command: Vec<String>,
    pub build_command_explicit: bool,
    pub timeout: Duration,
    pub language_timeouts: HashMap<String, Duration>,
}

pub struct TestRunner {
    pub commands: CommandConfig,
    pub parallelism: usize,
    pub project_root: PathBuf,
    pub verbose: bool,
    pub show_output: bool,
    pub max_tested: Option<usize>,
    /// Extra environment variables passed to every spawned command.
    pub env: HashMap<String, String>,
    /// Set to true externally (e.g. Ctrl+C handler) to stop spawning new mutations.
    pub cancelled: Arc<AtomicBool>,
}

struct FileGuard {
    path: PathBuf,
    original: Vec<u8>,
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        if let Err(e) = atomic_write(&self.path, &self.original) {
            eprintln!(
                "error: failed to restore {}: {} — file may be corrupted, check git status",
                self.path.display(),
                e
            );
        }
    }
}

impl TestRunner {
    #[allow(clippy::manual_is_multiple_of)]
    pub async fn run(&self, mutations: Vec<Mutation>) -> MutationReport {
        let start = Instant::now();
        let total = mutations.len();
        let counter = Arc::new(AtomicUsize::new(0));
        let tested_counter = Arc::new(AtomicUsize::new(0));
        let verbose = self.verbose;
        let is_tty = std::io::stderr().is_terminal();

        // Group mutations by file to serialize mutations on the same file
        let mut by_file: HashMap<PathBuf, Vec<Mutation>> = HashMap::new();
        for m in mutations {
            by_file.entry(m.file.clone()).or_default().push(m);
        }

        let semaphore = Arc::new(Semaphore::new(self.parallelism));
        let project_root = Arc::new(self.project_root.clone());
        let build_command = Arc::new(self.commands.build_command.clone());
        let mut handles = Vec::new();

        for (_file, file_mutations) in by_file {
            let sem = semaphore.clone();
            let language = file_mutations
                .first()
                .map(|m| m.language.as_str())
                .unwrap_or("");
            let command = self
                .commands
                .language_commands
                .get(language)
                .unwrap_or(&self.commands.command)
                .clone();
            let timeout = self
                .commands
                .language_timeouts
                .get(language)
                .copied()
                .unwrap_or(self.commands.timeout);
            let project_root = project_root.clone();
            let build_command = build_command.clone();
            let counter = counter.clone();
            let tested_counter = tested_counter.clone();
            let max_tested = self.max_tested;
            let show_output = self.show_output;
            let env = self.env.clone();
            let cancelled = self.cancelled.clone();

            let cmd_str = command.join(" ");
            let build_str = build_command.join(" ");
            let mut env_parts: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            env_parts.sort();
            let cache_ctx = format!(
                "{};build={};timeout={};env={}",
                cmd_str,
                build_str,
                timeout.as_millis(),
                env_parts.join(",")
            );

            let handle = tokio::spawn(async move {
                let mut results = Vec::new();
                for mutation in file_mutations {
                    // Stop if cancelled (Ctrl+C) or enough mutations have been tested
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some(max) = max_tested
                        && tested_counter.load(Ordering::Acquire) >= max
                    {
                        break;
                    }

                    // Check cache before running
                    let file_path = project_root.join(&mutation.file);
                    let file_content = std::fs::read(&file_path).ok();
                    let cache_key = file_content
                        .as_ref()
                        .map(|content| CacheKey::new(content, &mutation.description, &cache_ctx));
                    if let Some(ref key) = cache_key
                        && let Some(cached) = cache::lookup(&project_root, key)
                    {
                        let result = cached;
                        if result != MutationResult::BuildError {
                            tested_counter.fetch_add(1, Ordering::Release);
                        }
                        let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
                        if verbose {
                            eprintln!(
                                "  [{}/{}] \u{21bb} cached  {}:{} \u{2014} {}",
                                n,
                                total,
                                mutation.file.display(),
                                mutation.line,
                                mutation.operator
                            );
                        } else if is_tty {
                            eprint!("\r  [{}/{}] testing mutations...", n, total);
                            let _ = std::io::stderr().flush();
                        } else if n == total || (total >= 4 && n % (total / 4) == 0) {
                            eprintln!("  [{}/{}] testing mutations...", n, total);
                        }
                        results.push((mutation, result));
                        continue;
                    }

                    // Semaphore is never closed, so acquire cannot fail
                    let _permit = sem.acquire().await.unwrap();
                    let outcome = run_single_mutation(
                        &command,
                        &build_command,
                        timeout,
                        &project_root,
                        &mutation,
                        show_output,
                        &env,
                    )
                    .await;

                    // Store result in cache
                    if let Some(ref key) = cache_key {
                        cache::store(&project_root, key, outcome.result);
                    }

                    if outcome.result != MutationResult::BuildError {
                        tested_counter.fetch_add(1, Ordering::Release);
                    }
                    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if verbose {
                        let symbol = match outcome.result {
                            MutationResult::Killed => "\u{2713} killed",
                            MutationResult::Survived => "\u{2717} survived",
                            MutationResult::Timeout => "⧖ timeout",
                            MutationResult::BuildError => "⚠ build error",
                        };
                        eprintln!(
                            "  [{}/{}] {}  {}:{} \u{2014} {}",
                            n,
                            total,
                            symbol,
                            mutation.file.display(),
                            mutation.line,
                            mutation.operator
                        );
                    } else {
                        if is_tty {
                            eprint!("\r  [{}/{}] testing mutations...", n, total);
                            let _ = std::io::stderr().flush();
                        } else if n == total || (total >= 4 && n % (total / 4) == 0) {
                            eprintln!("  [{}/{}] testing mutations...", n, total);
                        }
                    }
                    if show_output
                        && outcome.result == MutationResult::Survived
                        && let Some(output) = &outcome.test_output
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
                    results.push((mutation, outcome.result));
                }
                results
            });
            handles.push(handle);
        }

        let mut all_results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(results) => all_results.extend(results),
                Err(e) => eprintln!("warning: mutation task panicked: {e}"),
            }
        }

        // Clear progress line on TTY
        if !verbose && is_tty {
            eprint!("\r                                        \r");
            let _ = std::io::stderr().flush();
        }

        let duration = start.elapsed();
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

async fn run_single_mutation(
    command: &[String],
    build_command: &[String],
    timeout: Duration,
    project_root: &Path,
    mutation: &Mutation,
    capture_output: bool,
    env: &HashMap<String, String>,
) -> MutationOutcome {
    let file_path = project_root.join(&mutation.file);

    // Validate the resolved path stays within project_root to prevent
    // path traversal from crafted diffs (e.g. "../../../etc/passwd").
    if let Ok(canonical) = file_path.canonicalize() {
        if let Ok(root) = project_root.canonicalize() {
            if !canonical.starts_with(&root) {
                eprintln!(
                    "warning: path traversal blocked: {} escapes project root",
                    mutation.file.display()
                );
                return MutationOutcome {
                    result: MutationResult::BuildError,
                    test_output: None,
                };
            }
        }
    }

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

    if let Err(e) = atomic_write(&file_path, &mutated) {
        eprintln!("warning: could not write {}: {e}", file_path.display());
        return MutationOutcome {
            result: MutationResult::BuildError,
            test_output: None,
        };
    }

    // Build check: skip expensive test if mutation doesn't compile
    if !build_command.is_empty() {
        let build_outcome = run_command(build_command, project_root, timeout, false, env).await;
        if build_outcome.result != MutationResult::Survived {
            return MutationOutcome {
                result: MutationResult::BuildError,
                test_output: None,
            };
        }
    }

    // Run test command; guard will restore the file on drop
    run_command(command, project_root, timeout, capture_output, env).await
}

async fn run_command(
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

    let mut cmd = tokio::process::Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(cwd).envs(env);

    if capture_output {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }

    cmd.kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: could not spawn command {:?}: {e}", &command[0]);
            return MutationOutcome {
                result: MutationResult::BuildError,
                test_output: None,
            };
        }
    };

    match tokio::time::timeout(timeout_dur, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let result = if output.status.success() {
                MutationResult::Survived
            } else {
                MutationResult::Killed
            };
            let test_output = if capture_output {
                let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }
                Some(combined)
            } else {
                None
            };
            MutationOutcome {
                result,
                test_output,
            }
        }
        Ok(Err(_)) => MutationOutcome {
            result: MutationResult::BuildError,
            test_output: None,
        },
        Err(_) => MutationOutcome {
            result: MutationResult::Timeout,
            test_output: None,
        },
    }
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

    #[tokio::test]
    async fn command_succeeds_returns_survived() {
        let (dir, file, mutation) = make_test_setup();

        let outcome = run_single_mutation(
            &["true".to_string()],
            &[],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Survived);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn command_fails_returns_killed() {
        let (dir, file, mutation) = make_test_setup();

        let outcome = run_single_mutation(
            &["false".to_string()],
            &[],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Killed);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn empty_replacement_splices_correctly() {
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
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Survived);
        // File should be restored to original
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn command_not_found_returns_build_error() {
        let (dir, file, mutation) = make_test_setup();

        let outcome = run_single_mutation(
            &["nonexistent_binary_xyz_12345".to_string()],
            &[],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::BuildError);
        // File should be restored
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn command_timeout_returns_timeout() {
        let (dir, file, mutation) = make_test_setup();

        let outcome = run_single_mutation(
            &["sleep".to_string(), "10".to_string()],
            &[],
            Duration::from_millis(100),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Timeout);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn build_check_failure_skips_test() {
        let (dir, file, mutation) = make_test_setup();

        let marker = dir.path().join("test_ran.marker");
        // Build fails → should return BuildError without running test
        let outcome = run_single_mutation(
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("touch {}", marker.display()),
            ],
            &["false".to_string()], // build fails
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::BuildError);
        assert!(!marker.exists(), "test command should not have run");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn build_check_success_runs_test() {
        let (dir, _file, mutation) = make_test_setup();

        // Build succeeds → test runs and fails → Killed
        let outcome = run_single_mutation(
            &["false".to_string()], // test fails = killed
            &["true".to_string()],  // build succeeds
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Killed);
    }

    #[tokio::test]
    async fn out_of_range_byte_range_returns_build_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hi").unwrap();

        let mut mutation = make_test_mutation(&file);
        mutation.byte_range = 0..100; // way past end

        let outcome = run_single_mutation(
            &["true".to_string()],
            &[],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::BuildError);
    }

    #[tokio::test]
    async fn missing_file_returns_build_error() {
        let dir = tempfile::tempdir().unwrap();
        let mutation = make_test_mutation(&dir.path().join("nonexistent.txt"));

        let outcome = run_single_mutation(
            &["true".to_string()],
            &[],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::BuildError);
    }

    #[tokio::test]
    async fn empty_command_returns_build_error() {
        let outcome = run_command(
            &[],
            &PathBuf::from("."),
            Duration::from_secs(5),
            false,
            &HashMap::new(),
        )
        .await;

        assert_eq!(outcome.result, MutationResult::BuildError);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn capture_output_collects_stdout_stderr() {
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
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Survived);
        let output = outcome.test_output.unwrap();
        assert!(output.contains("out"), "should capture stdout");
        assert!(output.contains("err"), "should capture stderr");
    }

    #[tokio::test]
    async fn max_tested_limits_mutations() {
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
                language_commands: HashMap::new(),
                build_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: Some(2),
            env: HashMap::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).await;
        assert_eq!(report.total, 2, "should stop after max_tested");
    }

    #[tokio::test]
    async fn report_aggregates_results_correctly() {
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
                language_commands: lang_cmds,
                build_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
            },
            parallelism: 2,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            env: HashMap::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(vec![m_survived, m_killed]).await;
        assert_eq!(report.total, 2);
        assert_eq!(report.killed, 1);
        assert_eq!(report.survived, 1);
        assert_eq!(report.timeout, 0);
        assert_eq!(report.build_errors, 0);
    }

    #[tokio::test]
    async fn language_commands_override_default() {
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
                language_commands: lang_cmds,
                build_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
            },
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            env: HashMap::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(vec![mutation]).await;
        assert_eq!(report.killed, 1, "should use language-specific command");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mutations_on_same_file_run_sequentially() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();
        let lock_file = dir.path().join("running.lock");

        // Each mutation has a unique description so cache can't short-circuit
        let mutations: Vec<Mutation> = (0..5)
            .map(|i| Mutation {
                id: i,
                file: file.clone(),
                language: String::new(),
                line: 1,
                column: 1,
                operator: "test".into(),
                description: format!("unique mutation {i}"),
                original: "hello".into(),
                replacement: "world".into(),
                byte_range: 0..5,
            })
            .collect();

        // Command that creates a lock file, sleeps briefly, then removes it.
        // If two run concurrently on the same file, the second will see
        // the lock file and fail (exit 1 = killed).
        let script = format!(
            "if [ -f {lock} ]; then exit 1; fi; touch {lock}; sleep 0.05; rm {lock}",
            lock = lock_file.display()
        );

        let runner = TestRunner {
            commands: CommandConfig {
                command: vec!["sh".into(), "-c".into(), script],
                language_commands: HashMap::new(),
                build_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts: HashMap::new(),
            },
            parallelism: 4, // high parallelism, but same-file should serialize
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            env: HashMap::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(mutations).await;
        assert_eq!(report.total, 5);
        // All should survive — if any were killed, two ran concurrently
        assert_eq!(
            report.killed, 0,
            "mutations on the same file ran concurrently"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn per_language_timeout_overrides_default() {
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
                language_commands: HashMap::new(),
                build_command: vec![],
                build_command_explicit: false,
                timeout: Duration::from_secs(5),
                language_timeouts,
            },
            parallelism: 2,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
            max_tested: None,
            env: HashMap::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };

        let report = runner.run(vec![m_slow, m_fast]).await;
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
