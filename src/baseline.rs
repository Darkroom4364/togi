use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::Path;

const BASELINE_FILE: &str = ".togi-baseline";

/// Per-file mutation score snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileScore {
    pub killed: usize,
    pub total: usize,
}

/// Baseline snapshot of mutation scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub files: HashMap<String, FileScore>,
    pub killed: usize,
    pub total: usize,
}

/// Build a baseline from a mutation report.
pub fn from_report(report: &crate::MutationReport, project_root: &Path) -> Baseline {
    let mut files: HashMap<String, FileScore> = HashMap::new();
    for (mutation, result) in &report.results {
        let rel = mutation
            .file
            .strip_prefix(project_root)
            .unwrap_or(&mutation.file)
            .display()
            .to_string();
        let entry = files.entry(rel).or_insert(FileScore {
            killed: 0,
            total: 0,
        });
        if *result != crate::MutationResult::BuildError {
            entry.total += 1;
            if *result == crate::MutationResult::Killed {
                entry.killed += 1;
            }
        }
    }
    let killed = report.killed;
    let total = report.total.saturating_sub(report.build_errors);
    Baseline {
        files,
        killed,
        total,
    }
}

/// Persist a baseline snapshot to `.togi-baseline` inside `dir`.
pub fn save_baseline(baseline: &Baseline, dir: &Path) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(baseline)?;
    std::fs::write(dir.join(BASELINE_FILE), json)?;
    Ok(())
}

/// Load a previously saved baseline from `dir`, returning `Ok(None)` if the file doesn't exist.
pub fn load_baseline(dir: &Path) -> anyhow::Result<Option<Baseline>> {
    let path = dir.join(BASELINE_FILE);
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    let baseline = serde_json::from_str(&data)
        .with_context(|| format!("invalid baseline at {}", path.display()))?;
    Ok(Some(baseline))
}

/// Returns `true` if the current overall score is a regression compared to the baseline.
///
/// A regression means the current kill ratio is strictly lower than the baseline kill ratio.
/// If either run has zero total mutations, no regression is reported.
pub fn check_regression(current: &Baseline, baseline: &Baseline) -> bool {
    if current.total == 0 || baseline.total == 0 {
        return false;
    }
    let current_ratio = current.killed as f64 / current.total as f64;
    let baseline_ratio = baseline.killed as f64 / baseline.total as f64;
    current_ratio < baseline_ratio
}

/// A per-file regression: file path and the score drop.
#[derive(Debug)]
pub struct FileRegression {
    pub file: String,
    pub baseline_pct: f64,
    pub current_pct: f64,
}

/// Find files where the mutation score dropped compared to the baseline.
/// Only reports files present in both the current and baseline runs.
pub fn per_file_regressions(current: &Baseline, baseline: &Baseline) -> Vec<FileRegression> {
    let mut regressions = Vec::new();
    for (file, base_score) in &baseline.files {
        if base_score.total == 0 {
            continue;
        }
        if let Some(cur_score) = current.files.get(file) {
            if cur_score.total == 0 {
                continue;
            }
            let base_pct = base_score.killed as f64 / base_score.total as f64 * 100.0;
            let cur_pct = cur_score.killed as f64 / cur_score.total as f64 * 100.0;
            if cur_pct < base_pct {
                regressions.push(FileRegression {
                    file: file.clone(),
                    baseline_pct: base_pct,
                    current_pct: cur_pct,
                });
            }
        }
    }
    regressions.sort_by(|a, b| {
        let a_drop = a.baseline_pct - a.current_pct;
        let b_drop = b.baseline_pct - b.current_pct;
        b_drop
            .partial_cmp(&a_drop)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    regressions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_baseline(killed: usize, total: usize) -> Baseline {
        Baseline {
            files: HashMap::new(),
            killed,
            total,
        }
    }

    #[test]
    fn no_regression_when_score_improves() {
        let baseline = make_baseline(5, 10);
        let current = make_baseline(7, 10);
        assert!(!check_regression(&current, &baseline));
    }

    #[test]
    fn no_regression_when_score_same() {
        let baseline = make_baseline(5, 10);
        let current = make_baseline(5, 10);
        assert!(!check_regression(&current, &baseline));
    }

    #[test]
    fn regression_when_score_drops() {
        let baseline = make_baseline(5, 10);
        let current = make_baseline(3, 10);
        assert!(check_regression(&current, &baseline));
    }

    #[test]
    fn no_regression_when_baseline_empty() {
        let baseline = make_baseline(0, 0);
        let current = make_baseline(3, 10);
        assert!(!check_regression(&current, &baseline));
    }

    #[test]
    fn no_regression_when_current_empty() {
        let baseline = make_baseline(5, 10);
        let current = make_baseline(0, 0);
        assert!(!check_regression(&current, &baseline));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();

        let mut files = HashMap::new();
        files.insert(
            "src/main.rs".to_string(),
            FileScore {
                killed: 3,
                total: 5,
            },
        );
        let baseline = Baseline {
            files,
            killed: 3,
            total: 5,
        };

        save_baseline(&baseline, dir.path()).unwrap();
        let loaded = load_baseline(dir.path()).unwrap().unwrap();

        assert_eq!(loaded.killed, 3);
        assert_eq!(loaded.total, 5);
        assert_eq!(loaded.files["src/main.rs"].killed, 3);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_baseline(dir.path()).unwrap().is_none());
    }

    #[test]
    fn load_returns_error_when_baseline_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(BASELINE_FILE), "not json").unwrap();

        assert!(load_baseline(dir.path()).is_err());
    }

    fn make_baseline_with_files(files: Vec<(&str, usize, usize)>) -> Baseline {
        let mut file_map = HashMap::new();
        let mut total_killed = 0;
        let mut total_total = 0;
        for (path, killed, total) in files {
            total_killed += killed;
            total_total += total;
            file_map.insert(path.to_string(), FileScore { killed, total });
        }
        Baseline {
            files: file_map,
            killed: total_killed,
            total: total_total,
        }
    }

    #[test]
    fn per_file_regression_detected() {
        let baseline = make_baseline_with_files(vec![("src/a.rs", 8, 10), ("src/b.rs", 5, 10)]);
        let current = make_baseline_with_files(vec![("src/a.rs", 6, 10), ("src/b.rs", 5, 10)]);
        let regs = per_file_regressions(&current, &baseline);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].file, "src/a.rs");
    }

    #[test]
    fn per_file_no_regression_when_improved() {
        let baseline = make_baseline_with_files(vec![("src/a.rs", 5, 10)]);
        let current = make_baseline_with_files(vec![("src/a.rs", 8, 10)]);
        assert!(per_file_regressions(&current, &baseline).is_empty());
    }

    #[test]
    fn per_file_ignores_new_files() {
        let baseline = make_baseline_with_files(vec![("src/a.rs", 5, 10)]);
        let current = make_baseline_with_files(vec![("src/a.rs", 5, 10), ("src/new.rs", 0, 5)]);
        assert!(per_file_regressions(&current, &baseline).is_empty());
    }

    #[test]
    fn per_file_sorted_by_largest_drop() {
        let baseline = make_baseline_with_files(vec![("src/a.rs", 10, 10), ("src/b.rs", 10, 10)]);
        let current = make_baseline_with_files(vec![
            ("src/a.rs", 5, 10), // 50% drop
            ("src/b.rs", 8, 10), // 20% drop
        ]);
        let regs = per_file_regressions(&current, &baseline);
        assert_eq!(regs.len(), 2);
        assert_eq!(regs[0].file, "src/a.rs"); // largest drop first
    }
}
