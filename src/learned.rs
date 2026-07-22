//! Learned mutant selection (`--learned-selection`).
//!
//! Mutants that always die to the same killer test are redundant: mutation
//! subsumption theory says executing one member of such a cluster yields the
//! same signal as executing all of them. Incremental history records the
//! killer test per killed mutant, so a later run can cluster this run's
//! mutants by shared killer test and skip the redundant members.
//!
//! Killer-test equality is only a conservative *proxy* for subsumption, so
//! skipping is strictly opt-in and strictly evidence-based: a mutant is only
//! ever skipped when history holds a `Killed` verdict with a recorded killer
//! test whose source and command hashes still match the current run. Any
//! doubt (no entry, stale hashes, non-killed verdict, no killer test) means
//! the mutant executes normally.

use crate::Mutation;
use std::collections::HashMap;
use std::path::PathBuf;

/// How a run's mutations were partitioned by learned selection.
#[derive(Debug, Default)]
pub struct LearnedPartition {
    /// Mutants to execute: every unclustered mutant plus the canonical
    /// (first in run order) member of each cluster.
    pub to_run: Vec<Mutation>,
    /// Mutants classified as subsumed: non-canonical cluster members,
    /// reported as [`crate::MutationResult::Subsumed`] without execution.
    pub subsumed: Vec<Mutation>,
    /// Number of clusters of size ≥ 2 — i.e. how many canonical mutants the
    /// subsumed ones were folded into.
    pub clusters: usize,
}

/// Partition `mutations` (in run order) into those to execute and those
/// subsumed by a canonical cluster sibling.
///
/// `killer_for` returns the recorded killer test for a mutation when history
/// holds a still-matching `Killed` entry for it, and `None` otherwise. Two
/// mutations cluster when they are in the same file and share a killer test;
/// the first cluster member in run order is canonical and executes.
pub fn partition_subsumed(
    mutations: Vec<Mutation>,
    mut killer_for: impl FnMut(&Mutation) -> Option<String>,
) -> LearnedPartition {
    let mut cluster_sizes: HashMap<(PathBuf, String), usize> = HashMap::new();
    let mut partition = LearnedPartition::default();

    for mutation in mutations {
        let key = killer_for(&mutation).map(|killer| (mutation.file.clone(), killer));
        match key {
            Some(key) => {
                let size = cluster_sizes.entry(key).or_insert(0);
                *size += 1;
                if *size == 1 {
                    // First member of the cluster: the canonical mutant.
                    partition.to_run.push(mutation);
                } else {
                    partition.subsumed.push(mutation);
                }
            }
            None => partition.to_run.push(mutation),
        }
    }

    partition.clusters = cluster_sizes.values().filter(|size| **size >= 2).count();
    partition
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutation(id: u32, file: &str) -> Mutation {
        Mutation {
            id,
            file: PathBuf::from(file),
            language: String::new(),
            line: id as usize,
            column: 1,
            operator: "op".into(),
            description: format!("mutation {id}"),
            original: "x".into(),
            replacement: "y".into(),
            byte_range: 0..1,
        }
    }

    fn killer_map<'a>(
        entries: &'a [(u32, &'a str)],
    ) -> impl FnMut(&Mutation) -> Option<String> + 'a {
        move |mutation| {
            entries
                .iter()
                .find(|(id, _)| *id == mutation.id)
                .map(|(_, killer)| killer.to_string())
        }
    }

    #[test]
    fn shared_killer_in_same_file_clusters_and_keeps_first_as_canonical() {
        let partition = partition_subsumed(
            vec![
                mutation(1, "src/a.rs"),
                mutation(2, "src/a.rs"),
                mutation(3, "src/a.rs"),
            ],
            killer_map(&[(1, "test_add"), (2, "test_add"), (3, "test_add")]),
        );

        assert_eq!(
            partition.to_run.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            partition.subsumed.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(partition.clusters, 1);
    }

    #[test]
    fn missing_killer_runs_normally() {
        let partition = partition_subsumed(
            vec![mutation(1, "src/a.rs"), mutation(2, "src/a.rs")],
            killer_map(&[(1, "test_add")]),
        );

        assert_eq!(partition.to_run.len(), 2);
        assert!(partition.subsumed.is_empty());
        assert_eq!(partition.clusters, 0);
    }

    #[test]
    fn different_killers_do_not_cluster() {
        let partition = partition_subsumed(
            vec![mutation(1, "src/a.rs"), mutation(2, "src/a.rs")],
            killer_map(&[(1, "test_add"), (2, "test_sub")]),
        );

        assert_eq!(partition.to_run.len(), 2);
        assert!(partition.subsumed.is_empty());
        assert_eq!(partition.clusters, 0);
    }

    #[test]
    fn shared_killer_across_files_does_not_cluster() {
        let partition = partition_subsumed(
            vec![mutation(1, "src/a.rs"), mutation(2, "src/b.rs")],
            killer_map(&[(1, "test_add"), (2, "test_add")]),
        );

        assert_eq!(partition.to_run.len(), 2);
        assert!(partition.subsumed.is_empty());
        assert_eq!(partition.clusters, 0);
    }

    #[test]
    fn independent_clusters_partition_in_run_order() {
        let partition = partition_subsumed(
            vec![
                mutation(1, "src/a.rs"),
                mutation(2, "src/a.rs"),
                mutation(3, "src/a.rs"),
                mutation(4, "src/a.rs"),
                mutation(5, "src/a.rs"),
            ],
            killer_map(&[
                (1, "test_add"),
                (2, "test_sub"),
                (3, "test_add"),
                (4, "test_sub"),
                (5, "test_mul"),
            ]),
        );

        assert_eq!(
            partition.to_run.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![1, 2, 5]
        );
        assert_eq!(
            partition.subsumed.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(partition.clusters, 2);
    }
}
