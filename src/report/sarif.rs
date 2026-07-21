use crate::{Mutation, MutationReport, MutationResult};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;

const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";
const SARIF_VERSION: &str = "2.1.0";
const INFORMATION_URI: &str = "https://github.com/Darkroom4364/togi";

#[derive(Serialize)]
struct SarifReport {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<SarifRun>,
}

#[derive(Serialize)]
struct SarifRun {
    tool: SarifTool,
    invocations: Vec<SarifInvocation>,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(Serialize)]
struct SarifDriver {
    name: &'static str,
    version: &'static str,
    #[serde(rename = "informationUri")]
    information_uri: &'static str,
    rules: Vec<SarifRule>,
}

#[derive(Serialize)]
struct SarifRule {
    id: String,
    #[serde(rename = "shortDescription")]
    short_description: SarifMessage,
}

#[derive(Serialize)]
struct SarifMessage {
    text: String,
}

#[derive(Serialize)]
struct SarifInvocation {
    #[serde(rename = "executionSuccessful")]
    execution_successful: bool,
    properties: SarifInvocationProperties,
}

#[derive(Serialize)]
struct SarifInvocationProperties {
    killed: usize,
    survived: usize,
    timeout: usize,
    build_errors: usize,
    #[serde(skip_serializing_if = "super::is_zero")]
    uncovered: usize,
    mutation_score: f64,
}

#[derive(Serialize)]
struct SarifResult {
    #[serde(rename = "ruleId")]
    rule_id: String,
    level: &'static str,
    message: SarifMessage,
    locations: Vec<SarifLocation>,
}

#[derive(Serialize)]
struct SarifLocation {
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysicalLocation,
}

#[derive(Serialize)]
struct SarifPhysicalLocation {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifactLocation,
    region: SarifRegion,
}

#[derive(Serialize)]
struct SarifArtifactLocation {
    uri: String,
}

#[derive(Serialize)]
struct SarifRegion {
    #[serde(rename = "startLine")]
    start_line: usize,
    #[serde(rename = "startColumn")]
    start_column: usize,
    #[serde(rename = "byteOffset")]
    byte_offset: usize,
    #[serde(rename = "byteLength")]
    byte_length: usize,
}

/// SARIF artifact URIs always use forward slashes, even on Windows.
fn artifact_uri(mutation: &Mutation) -> String {
    mutation.file.display().to_string().replace('\\', "/")
}

fn result_for(mutation: &Mutation) -> SarifResult {
    SarifResult {
        rule_id: mutation.operator.clone(),
        level: "warning",
        message: SarifMessage {
            text: format!(
                "Survived mutation: {} ({} → {})",
                mutation.description, mutation.original, mutation.replacement
            ),
        },
        locations: vec![SarifLocation {
            physical_location: SarifPhysicalLocation {
                artifact_location: SarifArtifactLocation {
                    uri: artifact_uri(mutation),
                },
                region: SarifRegion {
                    start_line: mutation.line,
                    start_column: mutation.column,
                    byte_offset: mutation.byte_range.start,
                    byte_length: mutation.byte_range.end - mutation.byte_range.start,
                },
            },
        }],
    }
}

/// Rule descriptions from the operator registry, keyed by operator id.
fn registry_descriptions() -> BTreeMap<String, String> {
    crate::operators::all_operators()
        .iter()
        .map(|op| (op.id().to_string(), op.description().to_string()))
        .collect()
}

pub fn print_report(report: &MutationReport) -> Result<()> {
    let sarif = to_sarif_string(report)?;
    println!("{}", sarif);
    Ok(())
}

/// Serialize report to a SARIF 2.1.0 string (for testing and programmatic use).
///
/// Emits one result per surviving mutant; killed, timeout, build-error, and
/// uncovered mutants are not findings. Uncovered mutants are deliberately not
/// code-scanning results (a zero-coverage line is not an actionable surviving
/// mutant); they only appear in the invocation totals.
pub fn to_sarif_string(report: &MutationReport) -> Result<String> {
    let registry = registry_descriptions();
    let surviving: Vec<&Mutation> = report
        .results
        .iter()
        .filter(|(_, r)| *r == MutationResult::Survived)
        .map(|(m, _)| m)
        .collect();

    let mut rules: Vec<SarifRule> = Vec::new();
    for mutation in &surviving {
        if rules.iter().any(|rule| rule.id == mutation.operator) {
            continue;
        }
        let description = registry
            .get(&mutation.operator)
            .cloned()
            .unwrap_or_else(|| mutation.description.clone());
        rules.push(SarifRule {
            id: mutation.operator.clone(),
            short_description: SarifMessage { text: description },
        });
    }

    let sarif_report = SarifReport {
        schema: SARIF_SCHEMA,
        version: SARIF_VERSION,
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "togi",
                    version: env!("CARGO_PKG_VERSION"),
                    information_uri: INFORMATION_URI,
                    rules,
                },
            },
            invocations: vec![SarifInvocation {
                execution_successful: true,
                properties: SarifInvocationProperties {
                    killed: report.killed,
                    survived: report.survived,
                    timeout: report.timeout,
                    build_errors: report.build_errors,
                    uncovered: report.uncovered_count(),
                    mutation_score: super::mutation_score(report),
                },
            }],
            results: surviving
                .iter()
                .map(|mutation| result_for(mutation))
                .collect(),
        }],
    };

    Ok(serde_json::to_string_pretty(&sarif_report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::sample_report;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::time::Duration;

    fn mutation(id: u32, file: &str, line: usize, operator: &str, description: &str) -> Mutation {
        Mutation {
            id,
            file: PathBuf::from(file),
            language: String::new(),
            line,
            column: 3,
            operator: operator.into(),
            description: description.into(),
            original: "==".into(),
            replacement: "!=".into(),
            byte_range: 10..12,
        }
    }

    fn report_with(results: Vec<(Mutation, MutationResult)>) -> MutationReport {
        let killed = results
            .iter()
            .filter(|(_, r)| *r == MutationResult::Killed)
            .count();
        let survived = results
            .iter()
            .filter(|(_, r)| *r == MutationResult::Survived)
            .count();
        let timeout = results
            .iter()
            .filter(|(_, r)| *r == MutationResult::Timeout)
            .count();
        let build_errors = results
            .iter()
            .filter(|(_, r)| *r == MutationResult::BuildError)
            .count();
        MutationReport {
            planned_total: results.len(),
            early_stop_reason: None,
            total: results.len(),
            killed,
            survived,
            timeout,
            build_errors,
            duration: Duration::from_secs(0),
            test_command: None,
            build_command: vec![],
            results,
            build_error_diagnostics: vec![],
            schemata: None,
            baseline_timing: None,
        }
    }

    fn parse(report: &MutationReport) -> Value {
        let sarif_str = to_sarif_string(report).expect("report should serialize");
        serde_json::from_str(&sarif_str).expect("output should be valid JSON")
    }

    #[test]
    fn sarif_output_is_valid_json_with_envelope() {
        let value = parse(&sample_report());
        assert!(value.is_object());
        assert_eq!(
            value["$schema"],
            "https://json.schemastore.org/sarif-2.1.0.json"
        );
        assert_eq!(value["version"], "2.1.0");
        assert_eq!(value["runs"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn sarif_driver_has_name_version_and_rules() {
        let value = parse(&sample_report());
        let driver = &value["runs"][0]["tool"]["driver"];
        assert_eq!(driver["name"], "togi");
        assert_eq!(driver["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            driver["informationUri"],
            "https://github.com/Darkroom4364/togi"
        );
        // Only the surviving mutant's operator produces a rule.
        let rules = driver["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["id"], "eq_to_neq");
        // Description comes from the operator registry, not the mutation.
        assert_eq!(rules[0]["shortDescription"]["text"], "Replace == with !=");
    }

    #[test]
    fn sarif_only_surviving_mutants_produce_results() {
        let report = report_with(vec![
            (
                mutation(0, "src/a.rs", 1, "op_a", "killed one"),
                MutationResult::Killed,
            ),
            (
                mutation(1, "src/b.rs", 2, "op_b", "survived one"),
                MutationResult::Survived,
            ),
            (
                mutation(2, "src/c.rs", 3, "op_c", "timed out"),
                MutationResult::Timeout,
            ),
            (
                mutation(3, "src/d.rs", 4, "op_d", "build error"),
                MutationResult::BuildError,
            ),
        ]);
        let value = parse(&report);
        let results = value["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["ruleId"], "op_b");
    }

    #[test]
    fn sarif_result_has_level_message_and_location() {
        let value = parse(&sample_report());
        let result = &value["runs"][0]["results"][0];
        assert_eq!(result["level"], "warning");
        assert_eq!(
            result["message"]["text"],
            "Survived mutation: changed == to != (== → !=)"
        );
        let location = &result["locations"][0]["physicalLocation"];
        assert_eq!(location["artifactLocation"]["uri"], "src/handler.rs");
        assert_eq!(location["region"]["startLine"], 15);
        assert_eq!(location["region"]["startColumn"], 5);
        assert_eq!(location["region"]["byteOffset"], 0);
        assert_eq!(location["region"]["byteLength"], 2);
    }

    #[test]
    fn sarif_invocation_properties_carry_totals_and_score() {
        let value = parse(&sample_report());
        let invocation = &value["runs"][0]["invocations"][0];
        assert_eq!(invocation["executionSuccessful"], true);
        let properties = &invocation["properties"];
        assert_eq!(properties["killed"], 1);
        assert_eq!(properties["survived"], 1);
        assert_eq!(properties["timeout"], 0);
        assert_eq!(properties["build_errors"], 0);
        assert_eq!(properties["mutation_score"], 50.0);
    }

    #[test]
    fn sarif_rules_deduplicate_and_fall_back_to_mutation_description() {
        let report = report_with(vec![
            (
                mutation(0, "src/a.rs", 1, "custom_op", "Custom operator description"),
                MutationResult::Survived,
            ),
            (
                mutation(1, "src/b.rs", 2, "custom_op", "Custom operator description"),
                MutationResult::Survived,
            ),
        ]);
        let value = parse(&report);
        assert_eq!(value["runs"][0]["results"].as_array().unwrap().len(), 2);
        let rules = value["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["id"], "custom_op");
        // Unknown operators keep the mutation's own description.
        assert_eq!(
            rules[0]["shortDescription"]["text"],
            "Custom operator description"
        );
    }

    #[test]
    fn sarif_empty_report_has_no_results_or_rules() {
        let value = parse(&report_with(vec![]));
        let run = &value["runs"][0];
        assert_eq!(run["results"], serde_json::json!([]));
        assert_eq!(run["tool"]["driver"]["rules"], serde_json::json!([]));
        assert_eq!(run["invocations"][0]["properties"]["mutation_score"], 100.0);
    }

    #[test]
    fn sarif_uncovered_mutants_produce_no_results_but_appear_in_totals() {
        let report = report_with(vec![
            (
                mutation(0, "src/a.rs", 1, "op_a", "covered and killed"),
                MutationResult::Killed,
            ),
            (
                mutation(1, "src/dead.rs", 9, "op_b", "zero coverage"),
                MutationResult::Uncovered,
            ),
        ]);
        let value = parse(&report);
        // Uncovered mutants are not code-scanning findings.
        assert_eq!(value["runs"][0]["results"], serde_json::json!([]));
        let properties = &value["runs"][0]["invocations"][0]["properties"];
        assert_eq!(properties["uncovered"], 1);
        // Score excludes uncovered mutants: 1 killed of 1 tested → 100%.
        assert_eq!(properties["mutation_score"], 100.0);
    }

    #[test]
    fn sarif_omits_uncovered_property_when_zero() {
        let value = parse(&sample_report());
        let properties = &value["runs"][0]["invocations"][0]["properties"];
        assert!(properties.get("uncovered").is_none());
    }

    #[test]
    fn sarif_artifact_uris_use_forward_slashes() {
        let report = report_with(vec![(
            mutation(0, "src\\win\\a.rs", 7, "op", "desc"),
            MutationResult::Survived,
        )]);
        let value = parse(&report);
        assert_eq!(
            value["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
                ["uri"],
            "src/win/a.rs"
        );
    }

    #[test]
    fn sarif_small_report_matches_expected_structure() {
        let report = report_with(vec![
            (
                mutation(0, "src/a.rs", 3, "lt_to_lte", "changed < to <="),
                MutationResult::Killed,
            ),
            (
                mutation(1, "src/b.rs", 9, "eq_to_neq", "changed == to !="),
                MutationResult::Survived,
            ),
        ]);
        let value = parse(&report);
        let expected = serde_json::json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "runs": [{
                "tool": {
                    "driver": {
                        "name": "togi",
                        "version": env!("CARGO_PKG_VERSION"),
                        "informationUri": "https://github.com/Darkroom4364/togi",
                        "rules": [{
                            "id": "eq_to_neq",
                            "shortDescription": { "text": "Replace == with !=" }
                        }]
                    }
                },
                "invocations": [{
                    "executionSuccessful": true,
                    "properties": {
                        "killed": 1,
                        "survived": 1,
                        "timeout": 0,
                        "build_errors": 0,
                        "mutation_score": 50.0
                    }
                }],
                "results": [{
                    "ruleId": "eq_to_neq",
                    "level": "warning",
                    "message": {
                        "text": "Survived mutation: changed == to != (== → !=)"
                    },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": { "uri": "src/b.rs" },
                            "region": {
                                "startLine": 9,
                                "startColumn": 3,
                                "byteOffset": 10,
                                "byteLength": 2
                            }
                        }
                    }]
                }]
            }]
        });
        assert_eq!(value, expected);
    }
}
