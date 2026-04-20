// Parallel test execution with timeouts

use crate::{Mutation, MutationReport, MutationResult};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub struct TestRunner {
    pub command: Vec<String>,
    pub timeout: Duration,
    pub parallelism: usize,
    pub project_root: PathBuf,
}

struct FileGuard {
    path: PathBuf,
    original: Vec<u8>,
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.path, &self.original);
    }
}

impl TestRunner {
    pub async fn run(&self, mutations: Vec<Mutation>) -> MutationReport {
        let start = Instant::now();

        // Group mutations by file to serialize mutations on the same file
        let mut by_file: HashMap<PathBuf, Vec<Mutation>> = HashMap::new();
        for m in mutations {
            by_file.entry(m.file.clone()).or_default().push(m);
        }

        let semaphore = Arc::new(Semaphore::new(self.parallelism));
        let mut handles = Vec::new();

        for (_file, file_mutations) in by_file {
            let sem = semaphore.clone();
            let command = self.command.clone();
            let timeout = self.timeout;
            let project_root = self.project_root.clone();

            let handle = tokio::spawn(async move {
                let mut results = Vec::new();
                for mutation in file_mutations {
                    let _permit = sem.acquire().await.unwrap();
                    let result =
                        run_single_mutation(&command, timeout, &project_root, &mutation).await;
                    results.push((mutation, result));
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

async fn run_single_mutation(
    command: &[String],
    timeout: Duration,
    project_root: &PathBuf,
    mutation: &Mutation,
) -> MutationResult {
    let file_path = project_root.join(&mutation.file);

    // Read original content
    let original = match std::fs::read(&file_path) {
        Ok(content) => content,
        Err(_) => return MutationResult::BuildError,
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
        return MutationResult::BuildError;
    }
    mutated.splice(range, mutation.replacement.as_bytes().iter().copied());

    if std::fs::write(&file_path, &mutated).is_err() {
        return MutationResult::BuildError;
    }

    // Run test command
    let result = run_command(command, project_root, timeout).await;
    // Guard will restore the file on drop
    result
}

async fn run_command(command: &[String], cwd: &PathBuf, timeout_dur: Duration) -> MutationResult {
    if command.is_empty() {
        return MutationResult::BuildError;
    }

    let mut cmd = tokio::process::Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(cwd);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return MutationResult::BuildError,
    };

    match tokio::time::timeout(timeout_dur, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if output.status.success() {
                MutationResult::Survived
            } else {
                MutationResult::Killed
            }
        }
        Ok(Err(_)) => MutationResult::BuildError,
        Err(_) => MutationResult::Timeout,
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

        let result = run_single_mutation(
            &["true".to_string()],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
        )
        .await;

        assert_eq!(result, MutationResult::Survived);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn command_fails_returns_killed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        let mutation = make_test_mutation(&file);

        let result = run_single_mutation(
            &["false".to_string()],
            Duration::from_secs(5),
            &dir.path().to_path_buf(),
            &mutation,
        )
        .await;

        assert_eq!(result, MutationResult::Killed);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn command_timeout_returns_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, b"hello world").unwrap();

        let mutation = make_test_mutation(&file);

        let result = run_single_mutation(
            &["sleep".to_string(), "10".to_string()],
            Duration::from_millis(100),
            &dir.path().to_path_buf(),
            &mutation,
        )
        .await;

        assert_eq!(result, MutationResult::Timeout);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    }
}
