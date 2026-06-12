// Parse LCOV coverage data into a map of file -> covered lines.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Map from file path (relative to project root) to set of covered line numbers.
pub type CoverageMap = HashMap<PathBuf, HashSet<usize>>;

#[derive(Debug, Clone)]
pub struct CoverageStats {
    pub covered_lines: CoverageMap,
    pub total_lines: CoverageMap,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverageMetric {
    pub covered: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
}

impl CoverageMetric {
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            (self.covered as f64 / self.total as f64) * 100.0
        }
    }

    pub fn meets_threshold(&self) -> bool {
        self.threshold
            .is_none_or(|threshold| self.percent() >= threshold)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverageUncoveredFile {
    pub file: PathBuf,
    pub lines: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoverageGateReport {
    pub line_coverage: CoverageMetric,
    pub diff_coverage: CoverageMetric,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncovered_changed_lines: Vec<CoverageUncoveredFile>,
    pub fail_on_uncovered_diff: bool,
}

impl CoverageGateReport {
    pub fn passes(&self) -> bool {
        self.line_coverage.meets_threshold()
            && self.diff_coverage.meets_threshold()
            && (!self.fail_on_uncovered_diff || self.uncovered_changed_lines.is_empty())
    }
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut rooted = false;

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => {
                normalized.push(component.as_os_str());
                rooted = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)));

                if can_pop {
                    normalized.pop();
                } else if !rooted {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

fn normalize_repo_relative_path(path: &Path, project_root: &Path) -> PathBuf {
    let normalized_path = normalize_path_components(path);
    let normalized_root = normalize_path_components(project_root);

    if normalized_root.as_os_str().is_empty() {
        return normalized_path;
    }

    normalized_path
        .strip_prefix(&normalized_root)
        .map(normalize_path_components)
        .unwrap_or(normalized_path)
}

/// Parse an LCOV file and return a coverage map with paths relative to `project_root`.
///
/// Parses both `DA:` (line coverage) and `BRDA:` (branch coverage) records.
/// Malformed records are logged as warnings and skipped.
pub fn parse_lcov(content: &str, project_root: &Path) -> CoverageMap {
    parse_lcov_stats(content, project_root).covered_lines
}

/// Parse an LCOV file and return both total and covered line sets.
pub fn parse_lcov_stats(content: &str, project_root: &Path) -> CoverageStats {
    let mut map = CoverageMap::new();
    let mut totals = CoverageMap::new();
    let mut current_file: Option<PathBuf> = None;

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if let Some(path_str) = line.strip_prefix("SF:") {
            current_file = Some(normalize_repo_relative_path(
                Path::new(path_str),
                project_root,
            ));
        } else if let Some(da) = line.strip_prefix("DA:") {
            if let Some(ref file) = current_file {
                let mut parts = da.split(',');
                if let (Some(line_str), Some(count_str)) = (parts.next(), parts.next()) {
                    match (line_str.parse::<usize>(), count_str.parse::<u64>()) {
                        (Ok(line_no), Ok(count)) => {
                            totals.entry(file.clone()).or_default().insert(line_no);
                            if count > 0 {
                                map.entry(file.clone()).or_default().insert(line_no);
                            }
                        }
                        _ => {
                            eprintln!(
                                "warning: malformed DA record at line {} in coverage file: {line}",
                                line_num + 1
                            );
                        }
                    }
                } else {
                    eprintln!(
                        "warning: malformed DA record at line {} in coverage file: {line}",
                        line_num + 1
                    );
                }
            }
        } else if let Some(brda) = line.strip_prefix("BRDA:") {
            // BRDA:line,block,branch,taken
            if let Some(ref file) = current_file {
                let parts: Vec<&str> = brda.split(',').collect();
                if parts.len() >= 4 {
                    let Ok(line_no) = parts[0].parse::<usize>() else {
                        eprintln!(
                            "warning: malformed BRDA line number at line {} in coverage file: {line}",
                            line_num + 1
                        );
                        continue;
                    };
                    totals.entry(file.clone()).or_default().insert(line_no);
                    let taken = parts[3].trim();
                    if taken == "-" || taken == "0" {
                        // Never executed or zero count — not covered
                    } else if let Ok(count) = taken.parse::<u64>() {
                        if count > 0 {
                            map.entry(file.clone()).or_default().insert(line_no);
                        }
                    } else {
                        eprintln!(
                            "warning: malformed BRDA taken value at line {} in coverage file: {line}",
                            line_num + 1
                        );
                    }
                } else {
                    eprintln!(
                        "warning: malformed BRDA record at line {} in coverage file: {line}",
                        line_num + 1
                    );
                }
            }
        } else if line == "end_of_record" {
            current_file = None;
        }
    }

    CoverageStats {
        covered_lines: map,
        total_lines: totals,
    }
}

/// Filter mutations to only those on lines present in the coverage map.
/// Warns about files that have mutations but are missing from coverage data.
pub fn filter_by_coverage(
    mutations: Vec<crate::Mutation>,
    coverage: &CoverageMap,
    project_root: &Path,
) -> Vec<crate::Mutation> {
    let mut warned_files: HashSet<PathBuf> = HashSet::new();

    mutations
        .into_iter()
        .filter(|m| {
            let rel = normalize_repo_relative_path(&m.file, project_root);
            match coverage.get(&rel) {
                Some(lines) => lines.contains(&m.line),
                None => {
                    if warned_files.insert(rel.clone()) {
                        eprintln!(
                            "warning: {} has mutations but is missing from coverage data — \
                             all its mutations will be filtered out",
                            rel.display()
                        );
                    }
                    false
                }
            }
        })
        .collect()
}

pub fn line_coverage_metric(coverage: &CoverageStats) -> CoverageMetric {
    let covered = coverage
        .covered_lines
        .values()
        .map(HashSet::len)
        .sum::<usize>();
    let total = coverage
        .total_lines
        .values()
        .map(HashSet::len)
        .sum::<usize>();
    CoverageMetric {
        covered,
        total,
        threshold: None,
    }
}

pub fn diff_coverage_report(
    coverage: &CoverageStats,
    changed_files: &[crate::ChangedFile],
    project_root: &Path,
) -> CoverageGateReport {
    let mut covered = 0usize;
    let mut total = 0usize;
    let mut uncovered_changed_lines: Vec<CoverageUncoveredFile> = Vec::new();

    for changed in changed_files {
        let rel = normalize_repo_relative_path(&changed.path, project_root);
        let covered_lines = coverage.covered_lines.get(&rel);

        let mut missing = Vec::new();
        for hunk in &changed.hunks {
            for line in hunk.start..=hunk.end {
                total += 1;
                let is_covered = covered_lines.is_some_and(|lines| lines.contains(&line));
                if is_covered {
                    covered += 1;
                } else {
                    missing.push(line);
                }
            }
        }
        if !missing.is_empty() {
            missing.sort_unstable();
            missing.dedup();
            uncovered_changed_lines.push(CoverageUncoveredFile {
                file: rel,
                lines: missing,
            });
        }
    }

    CoverageGateReport {
        line_coverage: line_coverage_metric(coverage),
        diff_coverage: CoverageMetric {
            covered,
            total,
            threshold: None,
        },
        uncovered_changed_lines,
        fail_on_uncovered_diff: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lcov_basic() {
        let lcov = "\
SF:/project/src/main.go
DA:1,1
DA:2,0
DA:3,5
end_of_record
SF:/project/src/util.go
DA:10,1
end_of_record
";
        let root = Path::new("/project");
        let map = parse_lcov(lcov, root);

        let main_lines = map.get(Path::new("src/main.go")).unwrap();
        assert!(main_lines.contains(&1));
        assert!(!main_lines.contains(&2));
        assert!(main_lines.contains(&3));

        let util_lines = map.get(Path::new("src/util.go")).unwrap();
        assert!(util_lines.contains(&10));
    }

    #[test]
    fn parse_lcov_empty() {
        let map = parse_lcov("", Path::new("/project"));
        assert!(map.is_empty());
    }

    #[test]
    fn parse_lcov_relative_paths() {
        let lcov = "SF:src/lib.rs\nDA:5,1\nend_of_record\n";
        let map = parse_lcov(lcov, Path::new("/project"));
        assert!(map.contains_key(Path::new("src/lib.rs")));
    }

    #[test]
    fn parse_lcov_stats_tracks_totals_and_coverage() {
        let lcov = "\
SF:/project/src/main.go
DA:1,1
DA:2,0
BRDA:3,0,0,4
end_of_record
";
        let root = Path::new("/project");
        let stats = parse_lcov_stats(lcov, root);
        let covered = stats.covered_lines.get(Path::new("src/main.go")).unwrap();
        let total = stats.total_lines.get(Path::new("src/main.go")).unwrap();
        assert!(covered.contains(&1));
        assert!(covered.contains(&3));
        assert!(total.contains(&1));
        assert!(total.contains(&2));
        assert!(total.contains(&3));
        let metric = line_coverage_metric(&stats);
        assert_eq!(metric.covered, 2);
        assert_eq!(metric.total, 3);
    }

    #[test]
    fn filter_matches_dot_relative_lcov_paths() {
        let root = Path::new("/project");
        let lcov = "SF:./src/lib.rs\nDA:42,1\nend_of_record\n";
        let coverage = parse_lcov(lcov, root);
        assert!(coverage.contains_key(Path::new("src/lib.rs")));

        let mutations = vec![crate::Mutation {
            id: 0,
            file: root.join("src/lib.rs"),
            language: "rust".into(),
            line: 42,
            column: 1,
            operator: "eq_to_ne".into(),
            description: "== to !=".into(),
            original: "==".into(),
            replacement: "!=".into(),
            byte_range: 0..2,
        }];

        let filtered = filter_by_coverage(mutations, &coverage, root);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].line, 42);
    }

    #[test]
    fn parse_lcov_brda_covered() {
        let lcov = "\
SF:/project/src/main.go
BRDA:10,0,0,5
BRDA:10,0,1,0
BRDA:15,1,0,-
end_of_record
";
        let root = Path::new("/project");
        let map = parse_lcov(lcov, root);
        let lines = map.get(Path::new("src/main.go")).unwrap();
        // Line 10 has a taken branch (count=5)
        assert!(lines.contains(&10));
        // Line 15 has only untaken branches ("-")
        assert!(!lines.contains(&15));
    }

    #[test]
    fn parse_lcov_brda_supplements_da() {
        let lcov = "\
SF:/project/src/main.go
DA:10,1
BRDA:20,0,0,3
end_of_record
";
        let root = Path::new("/project");
        let map = parse_lcov(lcov, root);
        let lines = map.get(Path::new("src/main.go")).unwrap();
        assert!(lines.contains(&10)); // from DA
        assert!(lines.contains(&20)); // from BRDA
    }

    #[test]
    fn filter_removes_uncovered_mutations() {
        let root = Path::new("/project");
        let mut coverage = CoverageMap::new();
        coverage
            .entry(PathBuf::from("src/main.go"))
            .or_default()
            .insert(10);

        let mutations = vec![
            crate::Mutation {
                id: 0,
                file: root.join("src/main.go"),
                language: "go".into(),
                line: 10,
                column: 1,
                operator: "lt_to_lte".into(),
                description: "< to <=".into(),
                original: "<".into(),
                replacement: "<=".into(),
                byte_range: 0..1,
            },
            crate::Mutation {
                id: 1,
                file: root.join("src/main.go"),
                language: "go".into(),
                line: 20,
                column: 1,
                operator: "lt_to_lte".into(),
                description: "< to <=".into(),
                original: "<".into(),
                replacement: "<=".into(),
                byte_range: 0..1,
            },
        ];

        let filtered = filter_by_coverage(mutations, &coverage, root);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].line, 10);
    }

    #[test]
    fn diff_coverage_report_lists_uncovered_changed_lines() {
        let root = Path::new("/project");
        let mut coverage = CoverageStats {
            covered_lines: CoverageMap::new(),
            total_lines: CoverageMap::new(),
        };
        coverage
            .covered_lines
            .entry(PathBuf::from("src/main.go"))
            .or_default()
            .insert(10);
        coverage
            .total_lines
            .entry(PathBuf::from("src/main.go"))
            .or_default()
            .extend([10, 11]);

        let changed_files = vec![crate::ChangedFile {
            path: root.join("src/main.go"),
            hunks: vec![crate::LineRange { start: 10, end: 11 }],
        }];

        let report = diff_coverage_report(&coverage, &changed_files, root);
        assert_eq!(report.diff_coverage.covered, 1);
        assert_eq!(report.diff_coverage.total, 2);
        assert_eq!(report.uncovered_changed_lines.len(), 1);
        assert_eq!(
            report.uncovered_changed_lines[0].file,
            PathBuf::from("src/main.go")
        );
        assert_eq!(report.uncovered_changed_lines[0].lines, vec![11]);
        assert!(!report.fail_on_uncovered_diff);
    }

}
