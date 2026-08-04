//! libFuzzer twin of the deterministic boundary harness in
//! `src/runner.rs` (`fuzz_diff_to_workspace_boundary_preserves_sources`).
//!
//! Drives arbitrary bytes through unified-diff parsing, safe path
//! validation/materialization, mutation generation, and the normal
//! FileGuard-backed mutation apply/restore path inside a disposable project
//! root. It asserts that emitted paths are always normalized project-relative,
//! hunks are sorted/non-overlapping/1-based, mutation originals are lossless
//! source slices, and nothing outside the disposable root is touched.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let changed = togi::diff::parse_diff(&text);
    for file in &changed {
        assert!(!file.hunks.is_empty());
        let path = file.path.to_string_lossy();
        assert!(!path.is_empty() && !path.contains('\\') && !path.starts_with('/'));
        for (i, range) in file.hunks.iter().enumerate() {
            assert!(range.start >= 1 && range.start <= range.end);
            if let Some(prev) = i.checked_sub(1).map(|p| &file.hunks[p]) {
                assert!(prev.end < range.start);
            }
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let case_root = tmp.path();
    let project = case_root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let sentinel_bytes = b"package main // sentinel\n".to_vec();
    let sentinel = case_root.join("outside.go");
    std::fs::write(&sentinel, &sentinel_bytes).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&sentinel, project.join("link.go")).unwrap();

    let subset: Vec<togi::ChangedFile> = changed.iter().take(4).cloned().collect();
    for file in &subset {
        let dest = project.join(&file.path);
        if dest.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        if std::fs::write(&dest, b"package main\n\nfunc f() int {\n\treturn 1 + 2\n}\n").is_err() {
            continue;
        }
    }

    let execution_file = togi::ChangedFile {
        path: "fuzz_apply.go".into(),
        hunks: vec![togi::LineRange { start: 1, end: 5 }],
    };
    std::fs::write(project.join(&execution_file.path), b"package main\n\nfunc f() int {\n\treturn 1 + 2\n}\n").unwrap();
    let mut execution_files = vec![execution_file];
    execution_files.extend(subset);
    let mutations = togi::mutator::generate_mutations(&execution_files, &project, 32, 0, &[]).unwrap();
    assert!(!mutations.is_empty());
    for mutation in mutations.iter().take(8) {
        let path = project.join(&mutation.file);
        let source = std::fs::read(&path).unwrap();
        assert_eq!(&source[mutation.byte_range.clone()], mutation.original.as_bytes());
        togi::runner::fuzz_apply_and_restore(&project, mutation).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), source);
    }
    assert_eq!(std::fs::read(&sentinel).unwrap(), sentinel_bytes);
});
