use crate::{MutationReport, MutationResult};
use anyhow::Result;
use serde::Serialize;

#[derive(Serialize)]
struct JsonReport {
    total: usize,
    tested: usize,
    killed: usize,
    survived: usize,
    timeout: usize,
    build_errors: usize,
    mutation_score: f64,
    duration_ms: u128,
    test_command: Option<Vec<String>>,
    build_command: Vec<String>,
    mutations: Vec<JsonMutation>,
}

#[derive(Serialize)]
struct JsonMutation {
    id: u32,
    file: String,
    line: usize,
    operator: String,
    description: String,
    result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
}

pub fn print_report(report: &MutationReport) -> Result<()> {
    let json = to_json_string(report)?;
    println!("{}", json);
    Ok(())
}

/// Serialize report to a JSON string (for testing and programmatic use).
pub fn to_json_string(report: &MutationReport) -> Result<String> {
    let mutations: Vec<JsonMutation> = report
        .results
        .iter()
        .map(|(m, r)| JsonMutation {
            id: m.id + 1,
            file: m.file.display().to_string(),
            line: m.line,
            operator: m.operator.clone(),
            description: m.description.clone(),
            result: match r {
                MutationResult::Killed => "killed".to_string(),
                MutationResult::Survived => "survived".to_string(),
                MutationResult::Timeout => "timeout".to_string(),
                MutationResult::BuildError => "build_error".to_string(),
            },
            column: Some(m.column),
            original: Some(m.original.clone()),
            replacement: Some(m.replacement.clone()),
            diff: super::mutation_diff(m),
        })
        .collect();

    let tested = report.total.saturating_sub(report.build_errors);
    let json_report = JsonReport {
        total: report.total,
        tested,
        killed: report.killed,
        survived: report.survived,
        timeout: report.timeout,
        build_errors: report.build_errors,
        mutation_score: super::mutation_score(report),
        duration_ms: report.duration.as_millis(),
        test_command: report.test_command.clone(),
        build_command: report.build_command.clone(),
        mutations,
    };

    Ok(serde_json::to_string_pretty(&json_report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mutation;
    use crate::test_helpers::sample_report;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::time::Duration;

    fn assert_object_keys(value: &Value, expected: &[&str]) {
        let object = value.as_object().expect("value should be a JSON object");
        let mut actual: Vec<&str> = object.keys().map(String::as_str).collect();
        let mut expected = expected.to_vec();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn json_output_is_valid_json() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: Value = serde_json::from_str(&json_str).expect("output should be valid JSON");
        assert!(value.is_object());
    }

    #[test]
    fn json_output_has_correct_structure() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(value["total"], 2);
        assert_eq!(value["killed"], 1);
        assert_eq!(value["survived"], 1);
        assert_eq!(value["timeout"], 0);
        assert_eq!(value["build_errors"], 0);
        assert_eq!(value["mutation_score"], 50.0);
        assert_eq!(value["duration_ms"], 1234);
        assert_eq!(value["test_command"], serde_json::json!(["cargo", "test"]));
        assert_eq!(value["build_command"], serde_json::json!([]));

        let mutations = value["mutations"].as_array().unwrap();
        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0]["id"], 1);
        assert_eq!(mutations[0]["file"], "src/auth.rs");
        assert_eq!(mutations[0]["line"], 47);
        assert_eq!(mutations[0]["operator"], "binary/lt_to_lte");
        assert_eq!(mutations[0]["result"], "killed");
        assert_eq!(mutations[1]["result"], "survived");
    }

    #[test]
    fn json_output_schema_is_stable() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: Value = serde_json::from_str(&json_str).unwrap();

        assert_object_keys(
            &value,
            &[
                "total",
                "tested",
                "killed",
                "survived",
                "timeout",
                "build_errors",
                "mutation_score",
                "duration_ms",
                "test_command",
                "build_command",
                "mutations",
            ],
        );

        let mutations = value["mutations"].as_array().unwrap();
        assert_object_keys(
            &mutations[0],
            &[
                "id",
                "file",
                "line",
                "operator",
                "description",
                "result",
                "column",
                "original",
                "replacement",
            ],
        );
        assert_object_keys(
            &mutations[1],
            &[
                "id",
                "file",
                "line",
                "operator",
                "description",
                "result",
                "column",
                "original",
                "replacement",
            ],
        );
    }

    #[test]
    fn json_score_zero_when_all_build_errors() {
        let report = MutationReport {
            results: vec![(
                Mutation {
                    id: 1,
                    file: PathBuf::from("src/a.rs"),
                    language: String::new(),
                    line: 1,
                    column: 1,
                    operator: "op".to_string(),
                    description: "d".to_string(),
                    original: "x".to_string(),
                    replacement: "y".to_string(),
                    byte_range: 0..1,
                },
                MutationResult::BuildError,
            )],
            duration: Duration::from_millis(100),
            test_command: None,
            build_command: vec![],
            total: 1,
            killed: 0,
            survived: 0,
            timeout: 0,
            build_errors: 1,
        };
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(value["mutation_score"], 0.0);
        assert_eq!(value["tested"], 0);
    }

    #[test]
    fn json_score_100_when_empty_report() {
        let report = MutationReport {
            results: vec![],
            duration: Duration::from_millis(0),
            test_command: None,
            build_command: vec![],
            total: 0,
            killed: 0,
            survived: 0,
            timeout: 0,
            build_errors: 0,
        };
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(value["mutation_score"], 100.0);
        assert_eq!(value["total"], 0);
    }

    #[test]
    fn json_output_includes_mutation_details() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let mutations = value["mutations"].as_array().unwrap();

        // All mutations should include exact before/after details for `togi explain`.
        assert_eq!(mutations[0]["column"], 10);
        assert_eq!(mutations[0]["original"], "<");
        assert_eq!(mutations[0]["replacement"], "<=");
        assert!(mutations[0]["diff"].is_null());

        assert_eq!(mutations[1]["column"], 5);
        assert_eq!(mutations[1]["original"], "==");
        assert_eq!(mutations[1]["replacement"], "!=");
        // diff is None because the test file doesn't exist on disk
        assert!(mutations[1]["diff"].is_null());
    }
}
