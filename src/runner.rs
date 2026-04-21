// Parallel test execution with timeouts

use crate::{Mutation, MutationReport, MutationResult};
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub struct TestRunner {
    pub command: Vec<String>,
    pub language_commands: HashMap<String, Vec<String>>,
    pub timeout: Duration,
    pub parallelism: usize,
    pub project_root: PathBuf,
    pub verbose: bool,
    pub show_output: bool,
}

struct FileGuard {
    path: PathBuf,
    original: Vec<u8>,
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::write(&self.path, &self.original) {
            eprintln!("error: failed to restore {}: {}", self.path.display(), e);
        }
    }
}

impl TestRunner {
    #[allow(clippy::manual_is_multiple_of)]
    pub async fn run(&self, mutations: Vec<Mutation>) -> MutationReport {
        let start = Instant::now();
        let total = mutations.len();
        let counter = Arc::new(AtomicUsize::new(0));
        let verbose = self.verbose;
        let is_tty = std::io::stderr().is_terminal();

        // Group mutations by file to serialize mutations on the same file
        let mut by_file: HashMap<PathBuf, Vec<Mutation>> = HashMap::new();
        for m in mutations {
            by_file.entry(m.file.clone()).or_default().push(m);
        }

        let semaphore = Arc::new(Semaphore::new(self.parallelism));
        let mut handles = Vec::new();

        for (_file, file_mutations) in by_file {
            let sem = semaphore.clone();
            let language = file_mutations
                .first()
                .map(|m| m.language.as_str())
                .unwrap_or("");
            let command = self
                .language_commands
                .get(language)
                .unwrap_or(&self.command)
                .clone();
            let timeout = self.timeout;
            let project_root = self.project_root.clone();
            let counter = counter.clone();
            let show_output = self.show_output;

            let handle = tokio::spawn(async move {
                let mut results = Vec::new();
                for mutation in file_mutations {
                    // Semaphore is never closed, so acquire cannot fail
                    let _permit = sem.acquire().await.unwrap();
                    let outcome = run_single_mutation(
                        &command,
                        timeout,
                        &project_root,
                        &mutation,
                        show_output,
                    )
                    .await;
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
            if let Ok(results) = handle.await {
                all_results.extend(results);
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
    timeout: Duration,
    project_root: &PathBuf,
    mutation: &Mutation,
    capture_output: bool,
) -> MutationOutcome {
    let file_path = project_root.join(&mutation.file);

    // Read original content
    let original = match std::fs::read(&file_path) {
        Ok(content) => content,
        Err(_) => {
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
    if range.end > mutated.len() {
        return MutationOutcome {
            result: MutationResult::BuildError,
            test_output: None,
        };
    }
    mutated.splice(range, mutation.replacement.as_bytes().iter().copied());

    if std::fs::write(&file_path, &mutated).is_err() {
        return MutationOutcome {
            result: MutationResult::BuildError,
            test_output: None,
        };
    }

    // Run test command; guard will restore the file on drop
    run_command(command, project_root, timeout, capture_output).await
}

async fn run_command(
    command: &[String],
    cwd: &PathBuf,
    timeout_dur: Duration,
    capture_output: bool,
) -> MutationOutcome {
    if command.is_empty() {
        return MutationOutcome {
            result: MutationResult::BuildError,
            test_output: None,
        };
    }

    let mut cmd = tokio::process::Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(cwd);

    if capture_output {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
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

    #[tokio::test]
    async fn command_succeeds_returns_survived() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        let mutation = make_test_mutation(&file);

        let outcome = run_single_mutation(
            &["true".to_string()],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Survived);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn command_fails_returns_killed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        let mutation = make_test_mutation(&file);

        let outcome = run_single_mutation(
            &["false".to_string()],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
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
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Survived);
        // File should be restored to original
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn command_not_found_returns_build_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        let mutation = make_test_mutation(&file);

        let outcome = run_single_mutation(
            &["nonexistent_binary_xyz_12345".to_string()],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
            false,
        )
        .await;

        assert_eq!(outcome.result, MutationResult::BuildError);
        // File should be restored
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn command_timeout_returns_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        let mutation = make_test_mutation(&file);

        let outcome = run_single_mutation(
            &["sleep".to_string(), "10".to_string()],
            Duration::from_millis(100),
            &dir.path().to_path_buf(),
            &mutation,
            false,
        )
        .await;

        assert_eq!(outcome.result, MutationResult::Timeout);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
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
            command: vec!["true".into()], // default would survive
            language_commands: lang_cmds,
            timeout: Duration::from_secs(5),
            parallelism: 1,
            project_root: dir.path().to_path_buf(),
            verbose: false,
            show_output: false,
        };

        let report = runner.run(vec![mutation]).await;
        assert_eq!(report.killed, 1, "should use language-specific command");
    }
}
