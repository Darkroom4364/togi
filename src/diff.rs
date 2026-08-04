// Unified diff parsing → Vec<ChangedFile>

use crate::{ChangedFile, LineRange};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const DEFAULT_DIFF_BASE: &str = "origin/main";

/// Select a locally resolvable base for a newly generated configuration.
pub(crate) fn init_diff_base(project_root: &Path) -> String {
    if let Some(reference) = origin_head(project_root) {
        return reference;
    }

    for reference in [DEFAULT_DIFF_BASE, "HEAD~1", "HEAD"] {
        if git_ref_is_commit(project_root, reference) {
            return reference.into();
        }
    }

    DEFAULT_DIFF_BASE.into()
}

fn origin_head(project_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args([
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let reference = String::from_utf8(output.stdout)
        .ok()?
        .trim_end_matches(&['\r', '\n'][..])
        .to_owned();
    (!reference.is_empty() && git_ref_is_commit(project_root, &reference)).then_some(reference)
}

/// Verifies a candidate resolves to a commit before it reaches `git diff`.
fn git_ref_is_commit(project_root: &Path, reference: &str) -> bool {
    Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ])
        .current_dir(project_root)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Build ChangedFile entries for every supported file in the project tree.
/// Respects `.gitignore` rules. Skips test/migration/seed files by default.
pub fn collect_all_supported_files(
    project_root: &Path,
    skip_noisy: bool,
    exclude_globs: &[String],
) -> anyhow::Result<Vec<ChangedFile>> {
    use crate::languages;

    let langs = languages::all();
    let supported_extensions: Vec<&str> = langs
        .iter()
        .flat_map(|lang| lang.extensions().to_vec())
        .collect();

    let mut files = Vec::new();

    let mut entries: Vec<_> = ignore::WalkBuilder::new(project_root)
        .hidden(true)
        .git_ignore(true)
        .build()
        .filter_map(|e| match e {
            Ok(entry) => Some(entry),
            Err(err) => {
                eprintln!("warning: skipping path during walk: {err}");
                None
            }
        })
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .collect();
    entries.sort_by(|a, b| a.path().cmp(b.path()));

    for entry in &entries {
        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        if !supported_extensions.contains(&ext) {
            continue;
        }
        let rel_path = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .to_path_buf();
        if skip_noisy && is_noisy_file(&rel_path) {
            continue;
        }
        if matches_user_excludes(&rel_path, exclude_globs) {
            continue;
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("warning: could not read {}: {e}", path.display());
                continue;
            }
        };
        let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
        let line_count = if bytes.is_empty() {
            0
        } else if bytes.last() == Some(&b'\n') {
            newlines
        } else {
            newlines + 1
        };
        if line_count == 0 {
            continue;
        }

        files.push(ChangedFile {
            path: rel_path,
            hunks: vec![LineRange {
                start: 1,
                end: line_count,
            }],
        });
    }

    Ok(files)
}

/// Collect changed files and line ranges since a given git ref or date.
///
/// `since` can be a commit SHA, branch name, tag, or a date string that
/// `git log --since` understands (e.g. "2024-01-01", "3 days ago").
///
/// Returns the same `Vec<ChangedFile>` format as `parse_diff`, filtering
/// out test files and files with no added lines.
pub fn collect_changed_since(
    project_root: &Path,
    since: &str,
    skip_noisy: bool,
    exclude_globs: &[String],
) -> anyhow::Result<Vec<ChangedFile>> {
    let ref_valid = git_ref_is_commit(project_root, since);

    // Try as a commit ref first (SHA, branch, tag).
    let output = if ref_valid {
        let o = Command::new("git")
            .args([
                "-c",
                "core.quotePath=false",
                "diff",
                &format!("{since}..HEAD"),
            ])
            .current_dir(project_root)
            .output()?;
        if o.status.success() {
            Some(String::from_utf8(o.stdout)?)
        } else {
            None
        }
    } else {
        None
    };

    let diff_output = if let Some(diff) = output {
        diff
    } else {
        // Fall back to date-based: resolve to a baseline commit, then diff.
        let rev_output = Command::new("git")
            .args(["rev-list", "-1", &format!("--before={since}"), "HEAD"])
            .current_dir(project_root)
            .output()?;
        if !rev_output.status.success() {
            anyhow::bail!(
                "git rev-list --before failed: {}",
                String::from_utf8_lossy(&rev_output.stderr)
            );
        }
        let base = String::from_utf8(rev_output.stdout)?.trim().to_string();
        if base.is_empty() {
            // No commit before that date — diff the entire history
            // against the well-known empty tree SHA.
            let tree_sha = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
            let out = Command::new("git")
                .args([
                    "-c",
                    "core.quotePath=false",
                    "diff",
                    &format!("{tree_sha}..HEAD"),
                ])
                .current_dir(project_root)
                .output()?;
            if !out.status.success() {
                anyhow::bail!(
                    "git diff (empty tree) failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            String::from_utf8(out.stdout)?
        } else {
            let out = Command::new("git")
                .args([
                    "-c",
                    "core.quotePath=false",
                    "diff",
                    &format!("{base}..HEAD"),
                ])
                .current_dir(project_root)
                .output()?;
            if !out.status.success() {
                anyhow::bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            String::from_utf8(out.stdout)?
        }
    };

    let all = parse_diff(&diff_output);
    Ok(all
        .into_iter()
        .filter(|f| !skip_noisy || !is_noisy_file(&f.path))
        .filter(|f| !matches_user_excludes(&f.path, exclude_globs))
        .collect())
}

/// Returns true if the path matches any user-provided exclude glob pattern.
pub fn matches_user_excludes(path: &Path, globs: &[String]) -> bool {
    let path_str = path.to_string_lossy().replace('\\', "/");
    globs.iter().any(|pattern| glob_match(pattern, &path_str))
}

/// Glob matching via `globset`. Bare names (no `/`) are treated as directory
/// names and match anywhere in the path tree.
fn glob_match(pattern: &str, text: &str) -> bool {
    let build = |pat: &str| -> bool {
        globset::GlobBuilder::new(pat)
            .literal_separator(true)
            .build()
            .map(|g| g.compile_matcher().is_match(text))
            .unwrap_or(false)
    };
    if !pattern.contains('/') {
        if pattern.contains('*') {
            // Wildcard like "*.generated.ts" — match at any depth
            return build(&format!("**/{pattern}"));
        }
        // Bare name like "vendor" — match as a directory anywhere
        return build(&format!("**/{pattern}/**"));
    }
    build(pattern)
}

/// Returns true for files that should be excluded from mutation testing:
/// test files, migrations, seeds, config files, and barrel/index re-exports.
pub fn is_noisy_file(path: &Path) -> bool {
    // Check if any ancestor directory is a known skip directory
    for component in path.components() {
        let s = component.as_os_str().to_str().unwrap_or("");
        if matches!(
            s,
            "tests"
                | "__tests__"
                | "__test__"
                | "spec"
                | "specs"
                | "testdata"
                | "fixtures"
                | "migration"
                | "migrations"
                | "seeds"
                | "examples"
        ) {
            return true;
        }
    }
    // Java/Kotlin/Gradle: src/test/... — match "test" only when preceded by "src"
    let comps: Vec<_> = path.components().collect();
    if comps
        .windows(2)
        .any(|w| w[0].as_os_str() == "src" && w[1].as_os_str() == "test")
    {
        return true;
    }
    let name = match path.file_stem().and_then(|s| s.to_str()) {
        Some(n) => n,
        None => return false,
    };
    // Go: *_test.go
    // Python: test_*.py / *_test.py
    // Ruby: *_test.rb / test_*.rb / *_spec.rb
    // Java/C#: *Test.java / *Tests.java / *Test.cs
    // JS/TS: *.test.ts / *.spec.ts (stem after first dot)
    if name.ends_with("_test") || name.starts_with("test_") || name.ends_with("_spec") {
        return true;
    }
    if name.ends_with("Test") || name.ends_with("Tests") || name.ends_with("Spec") {
        return true;
    }
    // *.test.ts, *.spec.ts — file_stem gives "foo.test"
    if name.ends_with(".test") || name.ends_with(".spec") {
        return true;
    }
    // Config files: vite.config.ts, jest.config.js, quasar.config.ts, etc.
    if name.ends_with(".config") || name.contains(".config.") {
        return true;
    }
    false
}

/// Parse unified diff output (from `git diff`) into a list of changed files
/// with their modified line ranges. Only tracks added/modified lines.
///
/// Parsing fails closed per file: an unsafe post-image path, a malformed or
/// truncated hunk, or inconsistent hunk line counts drop the whole file
/// instead of emitting partially parsed data. Emitted hunks are normalized
/// to sorted, non-overlapping, 1-based ranges, as `mapper::overlaps` requires.
pub fn parse_diff(input: &str) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_hunks: Vec<LineRange> = Vec::new();
    let mut file_poisoned = false;
    let mut saw_old_file_header = false;

    // Per-hunk state
    let mut in_hunk = false;
    let mut new_line: usize = 0; // current line number in the new file
    let mut remaining_old: usize = 0; // old-side lines left in the hunk
    let mut remaining_new: usize = 0; // new-side lines left in the hunk
    let mut range_start: Option<usize> = None;
    let mut range_end: usize = 0;

    for line in input.lines() {
        if line.starts_with("diff --git ") {
            // Structural boundary: finalize the previous file now, according
            // to its poison/completion state, so a headerless or malformed
            // following section can never be attributed to it.
            flush_range(&mut current_hunks, &mut range_start, range_end);
            if in_hunk {
                // Previous file's hunk was cut short — its counts were never
                // satisfied, so drop the file rather than trust partial data.
                file_poisoned = true;
            }
            if let Some(path) = current_path.take() {
                if !file_poisoned && !current_hunks.is_empty() {
                    normalize_hunks(&mut current_hunks);
                    files.push(ChangedFile {
                        path,
                        hunks: current_hunks,
                    });
                }
            }
            current_hunks = Vec::new();
            file_poisoned = false;
            in_hunk = false;
            saw_old_file_header = false;
            continue;
        }

        // File headers are the out-of-hunk `---`/`+++` pair. Added source
        // lines beginning with `++` are encoded as `+++ ...` inside hunks.
        if !in_hunk {
            if line.starts_with("--- ") {
                saw_old_file_header = true;
                continue;
            }

            if saw_old_file_header {
                if let Some(path_str) = line.strip_prefix("+++ ") {
                    saw_old_file_header = false;

                    // Flush previous hunk range
                    flush_range(&mut current_hunks, &mut range_start, range_end);
                    in_hunk = false;

                    // Flush previous file
                    if let Some(path) = current_path.take() {
                        if !file_poisoned && !current_hunks.is_empty() {
                            normalize_hunks(&mut current_hunks);
                            files.push(ChangedFile {
                                path,
                                hunks: current_hunks,
                            });
                        }
                    }
                    current_hunks = Vec::new();
                    file_poisoned = false;

                    current_path = post_image_path(path_str);
                    continue;
                }
            }

            saw_old_file_header = false;
        }

        if line.starts_with("@@ ") {
            // Flush any open range from a previous hunk
            flush_range(&mut current_hunks, &mut range_start, range_end);
            if in_hunk {
                // Previous hunk's counts were never satisfied — fail closed.
                file_poisoned = true;
            }
            in_hunk = false;
            saw_old_file_header = false;

            // Parse `@@ -old_start,old_count +new_start,new_count @@`
            match parse_hunk_header(line) {
                Some(header) => {
                    new_line = header.new_start;
                    remaining_old = header.old_count;
                    remaining_new = header.new_count;
                    in_hunk = remaining_old > 0 || remaining_new > 0;
                }
                None => {
                    // Malformed hunk header — fail closed for this file.
                    file_poisoned = true;
                }
            }
        } else if in_hunk && line.starts_with("\\ ") {
            // Diff metadata such as "\ No newline at end of file" is not a source line.
        } else if in_hunk {
            let mut counts_holds = true;
            if line.starts_with('+') {
                // Added line — track it
                if remaining_new == 0 {
                    counts_holds = false;
                } else {
                    remaining_new -= 1;
                    if range_start.is_none() {
                        range_start = Some(new_line);
                    }
                    range_end = new_line;
                    match new_line.checked_add(1) {
                        Some(next) => new_line = next,
                        None => counts_holds = false,
                    }
                }
            } else if line.starts_with('-') {
                // Deleted line — flush any open range, don't advance new_line
                if remaining_old == 0 {
                    counts_holds = false;
                } else {
                    remaining_old -= 1;
                    flush_range(&mut current_hunks, &mut range_start, range_end);
                }
            } else {
                // Context line
                if remaining_old == 0 || remaining_new == 0 {
                    counts_holds = false;
                } else {
                    remaining_old -= 1;
                    remaining_new -= 1;
                    flush_range(&mut current_hunks, &mut range_start, range_end);
                    match new_line.checked_add(1) {
                        Some(next) => new_line = next,
                        None => counts_holds = false,
                    }
                }
            }
            if !counts_holds {
                // Hunk content contradicts the header counts — fail closed.
                file_poisoned = true;
                in_hunk = false;
            } else if remaining_old == 0 && remaining_new == 0 {
                in_hunk = false;
            }
        } else if current_path.is_some()
            && (line.starts_with('+') || line.starts_with('-') || line.starts_with(' '))
        {
            // Source-shaped content outside any active hunk: the preceding
            // hunk's declared counts are already exhausted, so this file
            // section is malformed. Legitimate out-of-hunk lines (metadata
            // such as `index`, `rename from`, `Binary files ...`, and the
            // `\ No newline` marker) never start with '+', '-', or ' '.
            file_poisoned = true;
        }
    }

    // Flush final state
    flush_range(&mut current_hunks, &mut range_start, range_end);
    if in_hunk {
        // Input ended before the final hunk's counts were satisfied.
        file_poisoned = true;
    }
    if let Some(path) = current_path.take() {
        if !file_poisoned && !current_hunks.is_empty() {
            normalize_hunks(&mut current_hunks);
            files.push(ChangedFile {
                path,
                hunks: current_hunks,
            });
        }
    }

    files
}

/// Extract the post-image path from a `+++` header line, or `None` when the
/// file was deleted (`/dev/null`) or the path is unsafe to use. Git diff
/// commands run with `core.quotePath=false`, so paths arrive as raw UTF-8;
/// C-quoted headers (control characters, quotes, backslashes) are rejected
/// outright instead of being partially decoded.
fn post_image_path(header: &str) -> Option<PathBuf> {
    if header == "/dev/null" {
        return None;
    }
    if header.starts_with('"') {
        return None;
    }
    let stripped = header.strip_prefix("b/").unwrap_or(header);
    if stripped.chars().any(char::is_control) {
        return None;
    }
    if !crate::source_identity::is_normalized_project_relative_path(stripped) {
        return None;
    }
    Some(PathBuf::from(stripped))
}

/// Sort ranges and merge overlaps so downstream mapping can rely on
/// deterministic, sorted, non-overlapping, 1-based line ranges.
fn normalize_hunks(hunks: &mut Vec<LineRange>) {
    hunks.retain(|r| r.start >= 1 && r.start <= r.end);
    hunks.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<LineRange> = Vec::with_capacity(hunks.len());
    for range in hunks.drain(..) {
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }
    *hunks = merged;
}

fn flush_range(hunks: &mut Vec<LineRange>, start: &mut Option<usize>, end: usize) {
    if let Some(s) = start.take() {
        hunks.push(LineRange { start: s, end });
    }
}

struct HunkHeader {
    new_start: usize,
    old_count: usize,
    new_count: usize,
}

/// Parse `@@ -old_start[,old_count] +new_start[,new_count @@ ...`.
/// Rejects unparseable numbers and 1-based line starts of zero.
fn parse_hunk_header(line: &str) -> Option<HunkHeader> {
    let body = line.strip_prefix("@@ ")?;
    let ranges = body.find(" @@").map(|idx| &body[..idx])?;
    let (old, new) = ranges.split_once(' ')?;
    let (old_start, old_count) = parse_range_spec(old.strip_prefix('-')?)?;
    let (new_start, new_count) = parse_range_spec(new.strip_prefix('+')?)?;
    if (old_count > 0 && old_start == 0) || (new_count > 0 && new_start == 0) {
        return None;
    }
    Some(HunkHeader {
        new_start,
        old_count,
        new_count,
    })
}

/// Parse `start,count` or bare `start` (implicit count of 1).
fn parse_range_spec(spec: &str) -> Option<(usize, usize)> {
    match spec.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((spec.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DIFF: &str = r#"diff --git a/src/main.rs b/src/main.rs
index 1234567..abcdefg 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,6 +1,8 @@
 use std::io;

 fn main() {
+    println!("hello");
     let x = 1;
     let y = 2;
+    let z = x + y;
 }
@@ -10,2 +11,4 @@ fn other() {
     foo();
+    bar();
+    baz();
 }
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -5,2 +5,4 @@ pub mod config;
 pub mod diff;
+pub mod languages;
+pub mod mapper;
 pub mod runner;
"#;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_base_test_repo(with_parent: bool) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        git(root, &["init"]);
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "initial"]);
        if with_parent {
            std::fs::write(root.join("main.rs"), "fn main() { println!(\"hi\"); }\n").unwrap();
            git(root, &["add", "."]);
            git(root, &["commit", "-m", "second"]);
        }
        repo
    }

    #[test]
    fn init_diff_base_prefers_verified_origin_head() {
        let repo = init_base_test_repo(true);
        let root = repo.path();
        git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        assert_eq!(init_diff_base(root), "origin/main");
    }

    #[test]
    fn init_diff_base_prefers_non_main_origin_head_target() {
        let repo = init_base_test_repo(true);
        let root = repo.path();
        git(root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        assert_eq!(init_diff_base(root), "origin/trunk");
    }

    #[test]
    fn init_diff_base_ignores_unresolved_origin_head() {
        let repo = init_base_test_repo(true);
        let root = repo.path();
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/missing",
            ],
        );
        assert_eq!(init_diff_base(root), "HEAD~1");
    }

    #[test]
    fn init_diff_base_uses_origin_main_when_origin_head_is_stale() {
        let repo = init_base_test_repo(true);
        let root = repo.path();
        git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/missing",
            ],
        );
        assert_eq!(init_diff_base(root), "origin/main");
    }

    #[test]
    fn init_diff_base_uses_origin_main_without_origin_head() {
        let repo = init_base_test_repo(true);
        let root = repo.path();
        git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(init_diff_base(root), "origin/main");
    }

    #[test]
    fn init_diff_base_uses_head_parent_without_remote() {
        let repo = init_base_test_repo(true);
        assert_eq!(init_diff_base(repo.path()), "HEAD~1");
    }

    #[test]
    fn init_diff_base_uses_head_for_root_commit() {
        let repo = init_base_test_repo(false);
        assert_eq!(init_diff_base(repo.path()), "HEAD");
    }

    #[test]
    fn init_diff_base_falls_back_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(init_diff_base(dir.path()), DEFAULT_DIFF_BASE);
    }

    #[test]
    fn init_diff_base_falls_back_for_unborn_git_repository() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init"]);
        assert_eq!(init_diff_base(repo.path()), DEFAULT_DIFF_BASE);
    }

    #[test]
    fn parses_multiple_files() {
        let files = parse_diff(SAMPLE_DIFF);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(files[1].path, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn parses_hunks_correctly() {
        let files = parse_diff(SAMPLE_DIFF);
        let main_rs = &files[0];
        // First hunk: line 4 added, then line 7 added (with gap)
        assert_eq!(main_rs.hunks.len(), 3);
        assert_eq!(main_rs.hunks[0], LineRange { start: 4, end: 4 });
        assert_eq!(main_rs.hunks[1], LineRange { start: 7, end: 7 });
        // Second hunk: lines 12-13 added consecutively
        assert_eq!(main_rs.hunks[2], LineRange { start: 12, end: 13 });
    }

    #[test]
    fn handles_deletions_only() {
        let diff = r#"diff --git a/foo.rs b/foo.rs
--- a/foo.rs
+++ b/foo.rs
@@ -1,5 +1,3 @@
 line1
-deleted1
-deleted2
 line2
 line3
"#;
        let files = parse_diff(diff);
        // No added lines → no hunks → file not included
        assert!(files.is_empty());
    }

    #[test]
    fn ignores_no_newline_marker_when_tracking_added_lines() {
        let diff = "diff --git a/main.go b/main.go\n\
--- a/main.go\n\
+++ b/main.go\n\
@@ -1 +1 @@\n\
-old\n\
\\ No newline at end of file\n\
+new\n\
\\ No newline at end of file\n";
        let files = parse_diff(diff);

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks, vec![LineRange { start: 1, end: 1 }]);
    }

    #[test]
    fn skips_binary_files() {
        let diff = r#"diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 fn main() {
+    println!("hi");
 }
"#;
        let files = parse_diff(diff);
        // Binary file has no `+++ b/...` line, so it is skipped
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn deleted_file_produces_no_mutations() {
        let diff = r#"diff --git a/removed.rs b/removed.rs
--- a/removed.rs
+++ b/removed.rs
@@ -1,4 +1,0 @@
-fn old() {
-    // deleted
-    println!("gone");
-}
"#;
        let files = parse_diff(diff);
        assert!(
            files.is_empty(),
            "deleted-only file should produce no hunks"
        );
    }

    #[test]
    fn strips_b_prefix() {
        let diff = r#"diff --git a/path/to/file.rs b/path/to/file.rs
--- a/path/to/file.rs
+++ b/path/to/file.rs
@@ -1,2 +1,3 @@
 existing
+new_line
 end
"#;
        let files = parse_diff(diff);
        assert_eq!(files[0].path, PathBuf::from("path/to/file.rs"));
    }

    #[test]
    fn treats_added_double_plus_source_line_as_hunk_content() {
        let diff = r#"diff --git a/src/counter.rs b/src/counter.rs
index 1111111..2222222 100644
--- a/src/counter.rs
+++ b/src/counter.rs
@@ -1,4 +1,6 @@
 fn tick() {
     let mut counter = 0;
+++ counter
     counter += 1;
+    println!("{}", counter);
 }
diff --git a/src/next.rs b/src/next.rs
index 3333333..4444444 100644
--- a/src/next.rs
+++ b/src/next.rs
@@ -10,2 +10,3 @@
 fn next() {
+    next();
 }
"#;
        let files = parse_diff(diff);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, PathBuf::from("src/counter.rs"));
        assert_eq!(
            files[0].hunks,
            vec![
                LineRange { start: 3, end: 3 },
                LineRange { start: 5, end: 5 }
            ]
        );
        assert_eq!(files[1].path, PathBuf::from("src/next.rs"));
        assert_eq!(files[1].hunks, vec![LineRange { start: 11, end: 11 }]);
    }

    #[test]
    fn rejects_malformed_hunk_header() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ this is not a hunk header @@
 fn main() {
+    println!("hi");
 }
"#;
        let files = parse_diff(diff);
        assert!(
            files.is_empty(),
            "file with malformed hunk header must be dropped, got {files:?}"
        );
    }

    #[test]
    fn rejects_hunk_with_zero_new_start_and_additions() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +0,3 @@
 fn main() {
+    println!("hi");
+    println!("again");
 }
"#;
        let files = parse_diff(diff);
        assert!(
            files.is_empty(),
            "new_start 0 with nonzero new_count is not a valid 1-based range"
        );
    }

    #[test]
    fn rejects_hunk_with_more_added_lines_than_declared() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,1 +1,1 @@
+first
+second
"#;
        let files = parse_diff(diff);
        assert!(
            files.is_empty(),
            "hunk content contradicting the header counts must drop the file"
        );
    }

    #[test]
    fn rejects_source_content_after_declared_counts() {
        // Counts are satisfied by ` ctx` + `+added`; the surplus source line
        // afterwards must invalidate the file, not be silently ignored.
        let diff = r#"diff --git a/src/x.rs b/src/x.rs
--- a/src/x.rs
+++ b/src/x.rs
@@ -1,1 +1,2 @@
 ctx
+added
+surplus
"#;
        let files = parse_diff(diff);
        assert!(
            files.is_empty(),
            "content after the declared counts must drop the file, got {files:?}"
        );

        // Same for a surplus context-shaped line.
        let diff = r#"diff --git a/src/x.rs b/src/x.rs
--- a/src/x.rs
+++ b/src/x.rs
@@ -1,1 +1,2 @@
 ctx
+added
 surplus
"#;
        let files = parse_diff(diff);
        assert!(files.is_empty());
    }

    #[test]
    fn boundary_finalizes_previous_file_before_headerless_section() {
        // The second file section has no `---`/`+++` headers; its hunk must
        // not be attributed to the previous, completed file.
        let diff = r#"diff --git a/src/good.rs b/src/good.rs
--- a/src/good.rs
+++ b/src/good.rs
@@ -1,1 +1,2 @@
 ctx
+added
diff --git a/src/headerless.rs b/src/headerless.rs
@@ -5,1 +5,2 @@
 ctx
+stray
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("src/good.rs"));
        assert_eq!(
            files[0].hunks,
            vec![LineRange { start: 2, end: 2 }],
            "headerless section must not leak ranges into the previous file"
        );
    }

    #[test]
    fn metadata_lines_after_completed_hunk_do_not_poison() {
        // `index`/`\ No newline`-style lines legitimately appear outside
        // hunks and must not invalidate an otherwise well-formed file.
        let diff = "diff --git a/src/x.rs b/src/x.rs\n\
index 1234567..abcdefg 100644\n\
--- a/src/x.rs\n\
+++ b/src/x.rs\n\
@@ -1 +1 @@\n\
-old\n\
\\ No newline at end of file\n\
+new\n\
\\ No newline at end of file\n";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks, vec![LineRange { start: 1, end: 1 }]);
    }

    #[test]
    fn drops_file_with_truncated_hunk_but_keeps_next_file() {
        // First file's hunk declares 3 new lines but the next `diff --git`
        // arrives after 1 — the file is dropped, the next file still parses.
        let diff = r#"diff --git a/src/truncated.rs b/src/truncated.rs
--- a/src/truncated.rs
+++ b/src/truncated.rs
@@ -1,3 +1,3 @@
 ctx
+only_one_addition
diff --git a/src/ok.rs b/src/ok.rs
--- a/src/ok.rs
+++ b/src/ok.rs
@@ -1,1 +1,2 @@
 ctx
+added
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("src/ok.rs"));
        assert_eq!(files[0].hunks, vec![LineRange { start: 2, end: 2 }]);
    }

    #[test]
    fn drops_file_when_input_ends_mid_hunk() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 ctx
+added
"#;
        let files = parse_diff(diff);
        assert!(files.is_empty(), "truncated final hunk must drop the file");
    }

    #[test]
    fn rejects_hunk_header_with_oversized_numbers() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1 +99999999999999999999999999 @@
+added
"#;
        let files = parse_diff(diff);
        assert!(
            files.is_empty(),
            "numbers that do not fit usize must not wrap into valid ranges"
        );
    }

    #[test]
    fn rejects_unsafe_post_image_paths() {
        for header in [
            "+++ /etc/passwd",
            "+++ b/../escape.rs",
            "+++ b//double.rs",
            "+++ b/./dot.rs",
            "+++ b/a\\b.rs",
            "+++ \"b/quoted\\ttab.rs\"",
            "+++ ",
        ] {
            let diff = format!(
                "diff --git a/x.rs b/x.rs\n--- a/x.rs\n{header}\n@@ -1,1 +1,2 @@\n ctx\n+added\n"
            );
            let files = parse_diff(&diff);
            assert!(
                files.is_empty(),
                "unsafe post-image path {header:?} must drop the file, got {files:?}"
            );
        }
    }

    #[test]
    fn skips_deleted_file_via_dev_null_post_image() {
        let diff = r#"diff --git a/removed.rs b/removed.rs
deleted file mode 100644
--- a/removed.rs
+++ /dev/null
@@ -1,2 +1,0 @@
-fn gone() {
-}
"#;
        let files = parse_diff(diff);
        assert!(files.is_empty());
    }

    #[test]
    fn accepts_unicode_post_image_path() {
        // With `core.quotePath=false`, git emits non-ASCII paths as raw UTF-8.
        let diff = "diff --git a/src/\u{fc}n\u{ef}code.rs b/src/\u{fc}n\u{ef}code.rs\n\
--- a/src/\u{fc}n\u{ef}code.rs\n\
+++ b/src/\u{fc}n\u{ef}code.rs\n\
@@ -1,1 +1,2 @@\n\
 ctx\n\
+added\n";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("src/\u{fc}n\u{ef}code.rs"));
        assert_eq!(files[0].hunks, vec![LineRange { start: 2, end: 2 }]);
    }

    #[test]
    fn rename_uses_post_image_path() {
        let diff = r#"diff --git a/old/name.rs b/new/name.rs
similarity index 90%
rename from old/name.rs
rename to new/name.rs
--- a/old/name.rs
+++ b/new/name.rs
@@ -1,1 +1,2 @@
 ctx
+added
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("new/name.rs"));
        assert_eq!(files[0].hunks, vec![LineRange { start: 2, end: 2 }]);
    }

    #[test]
    fn crlf_hunk_content_tracks_lines_like_lf() {
        // Diff of a CRLF file: content lines carry a trailing \r, which must
        // not disturb added-line tracking.
        let diff = "diff --git a/win.rs b/win.rs\n\
--- a/win.rs\n\
+++ b/win.rs\n\
@@ -1,2 +1,3 @@\n\
 fn main() {\r\n\
+    println!(\"hi\");\r\n\
 }\r\n";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks, vec![LineRange { start: 2, end: 2 }]);
    }

    #[test]
    fn merges_overlapping_ranges_deterministically() {
        // Two hunks whose new-side ranges overlap (git never emits this, but
        // adversarial input must still normalize to sorted, non-overlapping
        // ranges).
        let diff = r#"diff --git a/x.rs b/x.rs
--- a/x.rs
+++ b/x.rs
@@ -1,1 +2,3 @@
 ctx
+a
+b
@@ -2,1 +4,3 @@
 ctx
+c
+d
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        // First hunk adds {3,4}; second adds {5,6} — adjacent, not merged.
        assert_eq!(
            files[0].hunks,
            vec![
                LineRange { start: 3, end: 4 },
                LineRange { start: 5, end: 6 }
            ]
        );

        let overlapping = r#"diff --git a/x.rs b/x.rs
--- a/x.rs
+++ b/x.rs
@@ -1,1 +2,3 @@
 ctx
+a
+b
@@ -2,1 +3,2 @@
 ctx
+c
"#;
        let files = parse_diff(overlapping);
        assert_eq!(files.len(), 1);
        // {3,4} and {4,4} overlap → merged.
        assert_eq!(files[0].hunks, vec![LineRange { start: 3, end: 4 }]);
    }

    #[test]
    fn parse_diff_output_invariants_hold_for_arbitrary_input() {
        // Deterministic xorshift PRNG — no external dependency, stable in CI.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const FRAGMENTS: &[&str] = &[
            "diff --git a/x.rs b/x.rs",
            "diff --git a/../evil.rs b/../evil.rs",
            "--- a/x.rs",
            "+++ b/x.rs",
            "+++ b/sub dir/y.rs",
            "+++ \"b/quoted.rs\"",
            "+++ /dev/null",
            "@@ -1,2 +1,2 @@",
            "@@ -0,0 +1,3 @@",
            "@@ -1 +1 @@ fn()",
            "@@ garbage @@",
            "@@ -1,5 +0,1 @@",
            "+added",
            "++double",
            "-removed",
            " context",
            "",
            "\\ No newline at end of file",
            "garbage line",
            "Binary files a/x and b/x differ",
        ];

        for _ in 0..2_000 {
            let line_count = (next() % 24) as usize;
            let mut input = String::new();
            for _ in 0..line_count {
                input.push_str(FRAGMENTS[(next() as usize) % FRAGMENTS.len()]);
                input.push('\n');
            }
            for file in parse_diff(&input) {
                let path = file.path.to_string_lossy();
                assert!(
                    crate::source_identity::is_normalized_project_relative_path(&path),
                    "emitted path must be a safe project-relative path: {path:?}"
                );
                assert!(!file.hunks.is_empty(), "emitted file must have hunks");
                for (i, range) in file.hunks.iter().enumerate() {
                    assert!(range.start >= 1, "ranges are 1-based: {range:?}");
                    assert!(range.start <= range.end, "range inverted: {range:?}");
                    if let Some(prev) = i.checked_sub(1).map(|p| &file.hunks[p]) {
                        assert!(
                            prev.end < range.start,
                            "ranges must be sorted and non-overlapping: {prev:?} then {range:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn collect_changed_since_preserves_unicode_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        run(&["init"]);
        // Force quotePath on in the repo config — togi's diff invocation must
        // still produce deterministic raw UTF-8 paths.
        run(&["config", "core.quotePath", "true"]);
        let name = "\u{fc}n\u{ef}code.rs";
        std::fs::write(root.join(name), "fn a() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "initial"]);

        let out = run(&["rev-parse", "HEAD"]);
        let base = String::from_utf8(out.stdout).unwrap().trim().to_string();

        std::fs::write(root.join(name), "fn a() {\n    1\n}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "edit"]);

        let files = collect_changed_since(root, &base, true, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from(name));
        assert!(!files[0].hunks.is_empty());
    }

    #[test]
    fn collect_all_finds_supported_files_and_respects_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Init a real git repo so the ignore crate respects .gitignore
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .expect("git init failed");

        // Supported file
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();

        // Unsupported extension
        std::fs::write(root.join("readme.txt"), "hi\n").unwrap();

        // Hidden dir (should be skipped)
        let hidden = root.join(".hidden");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("secret.rs"), "fn x() {}\n").unwrap();

        // Gitignored dir
        std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
        let generated = root.join("generated");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::write(generated.join("out.rs"), "fn gen() {}\n").unwrap();

        // Test file (should be skipped)
        std::fs::write(src.join("main_test.go"), "package main\n").unwrap();

        // File inside tests/ directory (should be skipped)
        let tests_dir = root.join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("helper.rs"), "fn help() {}\n").unwrap();

        let files = collect_all_supported_files(root, true, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].hunks[0], LineRange { start: 1, end: 1 });
    }

    #[test]
    fn is_noisy_file_detects_common_patterns() {
        assert!(is_noisy_file(Path::new("foo_test.go")));
        assert!(is_noisy_file(Path::new("test_utils.py")));
        assert!(is_noisy_file(Path::new("UserTest.java")));
        assert!(is_noisy_file(Path::new("UserTests.cs")));
        assert!(is_noisy_file(Path::new("app.test.ts")));
        assert!(is_noisy_file(Path::new("app.spec.tsx")));
        assert!(is_noisy_file(Path::new("widget_spec.rb")));
        assert!(!is_noisy_file(Path::new("main.rs")));
        assert!(!is_noisy_file(Path::new("utils.py")));
        assert!(!is_noisy_file(Path::new("contest.go")));
        // Directory-based detection
        assert!(is_noisy_file(Path::new("tests/helper.rs")));
        assert!(is_noisy_file(Path::new("__tests__/utils.ts")));
        assert!(is_noisy_file(Path::new("src/test/java/Foo.java")));
        assert!(is_noisy_file(Path::new("module/src/test/kotlin/Bar.kt")));
        assert!(!is_noisy_file(Path::new("test/integration/main.go")));
        assert!(!is_noisy_file(Path::new("cmd/test/server.go")));
        assert!(is_noisy_file(Path::new("testdata/input.go")));
        assert!(is_noisy_file(Path::new("fixtures/setup.py")));
        // Migration, seed, example directories
        assert!(is_noisy_file(Path::new("migration/001_init.ts")));
        assert!(is_noisy_file(Path::new("migrations/add_index.ts")));
        assert!(is_noisy_file(Path::new(
            "backend/migration/migrations/init.ts"
        )));
        assert!(is_noisy_file(Path::new("seeds/data.ts")));
        assert!(is_noisy_file(Path::new("examples/demo.py")));
        // Config files
        assert!(is_noisy_file(Path::new("vite.config.ts")));
        assert!(is_noisy_file(Path::new("jest.config.js")));
        assert!(is_noisy_file(Path::new("quasar.config.ts")));
        assert!(!is_noisy_file(Path::new("src/config.ts"))); // not a .config.ts file
        assert!(is_noisy_file(Path::new("vite.config.local.ts")));
    }

    #[test]
    fn matches_user_excludes_works() {
        let globs = vec!["*.config.ts".into(), "seeds/**".into()];
        assert!(matches_user_excludes(Path::new("vite.config.ts"), &globs));
        assert!(matches_user_excludes(Path::new("seeds/data.ts"), &globs));
        assert!(!matches_user_excludes(Path::new("src/main.ts"), &globs));

        // Wildcard patterns without `/` match at any depth
        let globs2 = vec!["*.generated.ts".into()];
        assert!(matches_user_excludes(
            Path::new("src/foo.generated.ts"),
            &globs2
        ));
        assert!(!matches_user_excludes(Path::new("src/foo.ts"), &globs2));
    }

    #[test]
    fn glob_match_respects_directory_boundaries() {
        // "test/**" should match test/ but not test-utils/
        let globs = vec!["test/**".into()];
        assert!(matches_user_excludes(Path::new("test/unit/foo.rs"), &globs));
        assert!(!matches_user_excludes(
            Path::new("test-utils/foo.rs"),
            &globs
        ));

        // Direct pattern should match whole component
        let globs2 = vec!["vendor".into()];
        assert!(matches_user_excludes(Path::new("vendor/lib.rs"), &globs2));
        assert!(!matches_user_excludes(
            Path::new("vendor-extra/lib.rs"),
            &globs2
        ));

        // General glob patterns: **/seeds/**, src/*/gen.rs
        let globs3 = vec!["**/seeds/**".into()];
        assert!(matches_user_excludes(
            Path::new("db/seeds/data.ts"),
            &globs3
        ));
        assert!(!matches_user_excludes(
            Path::new("db/other/data.ts"),
            &globs3
        ));

        let globs4 = vec!["src/*/gen.rs".into()];
        assert!(matches_user_excludes(Path::new("src/foo/gen.rs"), &globs4));
        assert!(!matches_user_excludes(
            Path::new("src/foo/bar/gen.rs"),
            &globs4
        ));
    }

    #[test]
    fn collect_changed_since_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        run(&["init"]);
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "initial"]);

        let out = run(&["rev-parse", "HEAD"]);
        let base = String::from_utf8(out.stdout).unwrap().trim().to_string();

        std::fs::write(
            root.join("main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "add print"]);

        let files = collect_changed_since(root, &base, true, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("main.rs"));
        assert!(!files[0].hunks.is_empty());
    }

    #[test]
    fn collect_changed_since_filters_test_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        run(&["init"]);
        std::fs::write(root.join("lib.rs"), "pub fn x() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "initial"]);

        let out = run(&["rev-parse", "HEAD"]);
        let base = String::from_utf8(out.stdout).unwrap().trim().to_string();

        std::fs::write(root.join("lib.rs"), "pub fn x() { 1 }\n").unwrap();
        std::fs::write(root.join("lib_test.rs"), "fn test() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "add changes"]);

        let files = collect_changed_since(root, &base, true, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("lib.rs"));
    }

    #[test]
    fn collect_changed_since_date_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let run = |args: &[&str], date: &str| {
            let out = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        run(&["init"], "2024-01-01T00:00:00Z");
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        run(&["add", "."], "2024-01-01T00:00:00Z");
        run(&["commit", "-m", "initial"], "2024-01-01T00:00:00Z");

        std::fs::write(
            root.join("main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        run(&["add", "."], "2024-06-15T00:00:00Z");
        run(&["commit", "-m", "add print"], "2024-06-15T00:00:00Z");

        // Use a date between the two commits — should pick up the second commit.
        let files = collect_changed_since(root, "2024-03-01", true, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("main.rs"));
        assert!(!files[0].hunks.is_empty());
    }

    #[test]
    fn collect_changed_since_root_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let run = |args: &[&str], date: &str| {
            let out = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        run(&["init"], "2024-06-01T00:00:00Z");
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        run(&["add", "."], "2024-06-01T00:00:00Z");
        run(&["commit", "-m", "initial"], "2024-06-01T00:00:00Z");

        // Date before any commit — triggers empty-tree SHA fallback.
        let files = collect_changed_since(root, "2024-01-01", true, &[]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("main.rs"));
        assert!(!files[0].hunks.is_empty());
    }
}
