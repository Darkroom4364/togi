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
    mutations: Vec<JsonMutation>,
}

#[derive(Serialize)]
struct JsonMutation {
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
        .map(|(m, r)| {
            let survived = matches!(r, MutationResult::Survived);
            JsonMutation {
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
                column: if survived { Some(m.column) } else { None },
                original: if survived {
                    Some(m.original.clone())
                } else {
                    None
                },
                replacement: if survived {
                    Some(m.replacement.clone())
                } else {
                    None
                },
                diff: if survived {
                    super::mutation_diff(m)
                } else {
                    None
                },
            }
        })
        .collect();

    let tested = report.total - report.build_errors;
    let mutation_score = if tested > 0 {
        (report.killed as f64 / tested as f64) * 100.0
    } else if report.total == 0 {
        100.0
    } else {
        0.0
    };
    let json_report = JsonReport {
        total: report.total,
        tested,
        killed: report.killed,
        survived: report.survived,
        timeout: report.timeout,
        build_errors: report.build_errors,
        mutation_score,
        duration_ms: report.duration.as_millis(),
        mutations,
    };

    Ok(serde_json::to_string_pretty(&json_report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mutation;
    use std::path::PathBuf;
    use std::time::Duration;

    fn sample_report() -> MutationReport {
        MutationReport {
            results: vec![
                (
                    Mutation {
                        id: 1,
                        file: PathBuf::from("src/auth.rs"),
                        language: String::new(),
                        line: 47,
                        column: 10,
                        operator: "binary/lt_to_lte".to_string(),
                        description: "changed < to <=".to_string(),
                        original: "<".to_string(),
                        replacement: "<=".to_string(),
                        byte_range: 0..1,
                    },
                    MutationResult::Killed,
                ),
                (
                    Mutation {
                        id: 2,
                        file: PathBuf::from("src/handler.rs"),
                        language: String::new(),
                        line: 15,
                        column: 5,
                        operator: "binary/eq_to_neq".to_string(),
                        description: "changed == to !=".to_string(),
                        original: "==".to_string(),
                        replacement: "!=".to_string(),
                        byte_range: 0..2,
                    },
                    MutationResult::Survived,
                ),
            ],
            duration: Duration::from_millis(1234),
            total: 2,
            killed: 1,
            survived: 1,
            timeout: 0,
            build_errors: 0,
        }
    }

    #[test]
    fn json_output_is_valid_json() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&json_str).expect("output should be valid JSON");
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

        let mutations = value["mutations"].as_array().unwrap();
        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0]["file"], "src/auth.rs");
        assert_eq!(mutations[0]["line"], 47);
        assert_eq!(mutations[0]["operator"], "binary/lt_to_lte");
        assert_eq!(mutations[0]["result"], "killed");
        assert_eq!(mutations[1]["result"], "survived");
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
    fn json_survived_includes_diff_fields() {
        let report = sample_report();
        let json_str = to_json_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let mutations = value["mutations"].as_array().unwrap();

        // Killed mutation should NOT have diff fields
        assert!(mutations[0]["column"].is_null());
        assert!(mutations[0]["original"].is_null());
        assert!(mutations[0]["replacement"].is_null());
        assert!(mutations[0]["diff"].is_null());

        // Survived mutation should have original/replacement/column
        assert_eq!(mutations[1]["column"], 5);
        assert_eq!(mutations[1]["original"], "==");
        assert_eq!(mutations[1]["replacement"], "!=");
        // diff is None because the test file doesn't exist on disk
        assert!(mutations[1]["diff"].is_null());
    }
}
