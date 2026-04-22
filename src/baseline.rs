use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// Persist a baseline snapshot to `.togi-baseline` inside `dir`.
pub fn save_baseline(baseline: &Baseline, dir: &Path) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(baseline).map_err(std::io::Error::other)?;
    std::fs::write(dir.join(BASELINE_FILE), json)
}

/// Load a previously saved baseline from `dir`, returning `None` if the file doesn't exist.
pub fn load_baseline(dir: &Path) -> Option<Baseline> {
    let path = dir.join(BASELINE_FILE);
    if !path.exists() {
        return None;
    }
    let data = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&data) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("warning: failed to parse {}: {}", path.display(), e);
            None
        }
    }
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
        let loaded = load_baseline(dir.path()).unwrap();

        assert_eq!(loaded.killed, 3);
        assert_eq!(loaded.total, 5);
        assert_eq!(loaded.files["src/main.rs"].killed, 3);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_baseline(dir.path()).is_none());
    }
}
