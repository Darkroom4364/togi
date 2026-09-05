use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::{
    env, fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::ExitCode,
};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const REVIEWED_ACTION_COMMITS: &[&str] = &[
    "0sec-labs/foxguard/action@1fbe7f384d6dda358f2dc7f712dddf25b8e4be76",
    "Darkroom4364/togi@a1503b2ebac4c63d377b015c4825b97cab25ec68",
    "Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32",
    "actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8",
    "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
    "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
    "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
    "actions/setup-dotnet@a98b56852c35b8e3190ac28c8c2271da59106c68",
    "actions/setup-go@b7ad1dad31e06c5925ef5d2fc7ad053ef454303e",
    "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020",
    "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
    "dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8",
    "softprops/action-gh-release@3d0d9888cb7fd7b750713d6e236d1fcb99157228",
];

#[derive(Clone, Copy)]
enum ManifestKind {
    Workflow,
    Action,
}

fn main() -> ExitCode {
    match check_manifests() {
        Ok(()) => ExitCode::SUCCESS,
        Err(diagnostics) => {
            for diagnostic in diagnostics {
                eprintln!("{diagnostic}");
            }
            ExitCode::FAILURE
        }
    }
}

fn check_manifests() -> Result<(), Vec<String>> {
    let (root, manifests) = match manifest_arguments() {
        Ok(arguments) => arguments,
        Err(diagnostic) => return Err(vec![diagnostic]),
    };
    let mut diagnostics = Vec::new();

    for relative_path in manifests {
        check_manifest(&root, &relative_path, &mut diagnostics);
    }

    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

fn manifest_arguments() -> Result<(PathBuf, Vec<PathBuf>), String> {
    let mut arguments = env::args_os().skip(1);
    let root = arguments.next().ok_or_else(|| {
        "check-pinned-actions: expected a Git work-tree root followed by selected manifests"
            .to_owned()
    })?;
    Ok((
        PathBuf::from(root),
        arguments.map(PathBuf::from).collect::<Vec<_>>(),
    ))
}

fn check_manifest(root: &Path, relative_path: &Path, diagnostics: &mut Vec<String>) {
    let display_path = relative_path.display().to_string();
    if !is_safe_relative_path(relative_path) {
        diagnostics.push(format!(
            "{display_path}: $: selected manifest path must be relative to the Git work tree"
        ));
        return;
    }
    let kind = if is_workflow(relative_path) {
        ManifestKind::Workflow
    } else if is_action_manifest(relative_path) {
        ManifestKind::Action
    } else {
        diagnostics.push(format!("{display_path}: $: unsupported manifest path"));
        return;
    };
    let document = match read_document(root, relative_path, &display_path) {
        Ok(document) => document,
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            return;
        }
    };

    match kind {
        ManifestKind::Workflow => check_workflow(&document, &display_path, diagnostics),
        ManifestKind::Action => check_action(&document, &display_path, diagnostics),
    }
}

fn read_document(root: &Path, relative_path: &Path, display_path: &str) -> Result<Value, String> {
    let path = root.join(relative_path);
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("{display_path}: unable to read manifest: {error}"))?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "{display_path}: manifest exceeds the {MAX_MANIFEST_BYTES}-byte size limit"
        ));
    }

    let mut source = Vec::with_capacity(metadata.len().min(MAX_MANIFEST_BYTES + 1) as usize);
    fs::File::open(&path)
        .and_then(|file| file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut source))
        .map_err(|error| format!("{display_path}: unable to read manifest: {error}"))?;
    if source.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(format!(
            "{display_path}: manifest exceeds the {MAX_MANIFEST_BYTES}-byte size limit"
        ));
    }
    let source = String::from_utf8(source)
        .map_err(|error| format!("{display_path}: unable to read manifest: {error}"))?;

    let mut documents = Vec::new();
    for document in serde_yaml::Deserializer::from_str(&source) {
        documents.push(
            Value::deserialize(document)
                .map_err(|error| format!("{display_path}: invalid YAML: {error}"))?,
        );
    }

    match documents.len() {
        1 => Ok(documents.pop().expect("one YAML document")),
        count => Err(format!(
            "{display_path}: expected exactly one YAML document, found {count}"
        )),
    }
}

fn is_workflow(path: &Path) -> bool {
    let path = path.to_string_lossy();
    path.starts_with(".github/workflows/") && (path.ends_with(".yml") || path.ends_with(".yaml"))
}

fn is_action_manifest(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("action.yml" | "action.yaml")
    )
}

fn is_safe_relative_path(path: &Path) -> bool {
    path.is_relative()
        && path
            .components()
            .all(|component| matches!(component, Component::CurDir | Component::Normal(_)))
}

fn check_workflow(document: &Value, file: &str, diagnostics: &mut Vec<String>) {
    let Some(root) = untag(document).as_mapping() else {
        diagnostics.push(format!("{file}: $: expected a workflow mapping"));
        return;
    };
    reject_merge_key(root, file, "$", diagnostics);

    let Some(jobs) = mapping_field(root, "jobs") else {
        return;
    };
    let Some(jobs) = untag(jobs).as_mapping() else {
        diagnostics.push(format!("{file}: jobs: expected a jobs mapping"));
        return;
    };
    reject_merge_key(jobs, file, "jobs", diagnostics);

    for (job_name, job) in jobs {
        let Some(job_name) = untag(job_name).as_str() else {
            diagnostics.push(format!("{file}: jobs: expected a string job key"));
            continue;
        };
        check_job(job, file, &format!("jobs.{job_name}"), diagnostics);
    }
}

fn check_job(job: &Value, file: &str, path: &str, diagnostics: &mut Vec<String>) {
    let Some(job) = untag(job).as_mapping() else {
        diagnostics.push(format!("{file}: {path}: expected a job mapping"));
        return;
    };
    reject_merge_key(job, file, path, diagnostics);

    if let Some(reference) = mapping_field(job, "uses") {
        check_reference(reference, file, &format!("{path}.uses"), diagnostics);
    }
    if let Some(steps) = mapping_field(job, "steps") {
        check_steps(steps, file, &format!("{path}.steps"), diagnostics);
    }
}

fn check_action(document: &Value, file: &str, diagnostics: &mut Vec<String>) {
    let Some(root) = untag(document).as_mapping() else {
        diagnostics.push(format!("{file}: $: expected an action mapping"));
        return;
    };
    reject_merge_key(root, file, "$", diagnostics);

    let Some(runs) = mapping_field(root, "runs") else {
        return;
    };
    let Some(runs) = untag(runs).as_mapping() else {
        diagnostics.push(format!("{file}: runs: expected a runs mapping"));
        return;
    };
    reject_merge_key(runs, file, "runs", diagnostics);

    if mapping_field(runs, "using").and_then(|using| untag(using).as_str()) != Some("composite") {
        return;
    }

    let Some(steps) = mapping_field(runs, "steps") else {
        diagnostics.push(format!(
            "{file}: runs.steps: composite actions must define a steps sequence"
        ));
        return;
    };
    check_steps(steps, file, "runs.steps", diagnostics);
}

fn check_steps(steps: &Value, file: &str, path: &str, diagnostics: &mut Vec<String>) {
    let Some(steps) = untag(steps).as_sequence() else {
        diagnostics.push(format!("{file}: {path}: expected a steps sequence"));
        return;
    };

    for (index, step) in steps.iter().enumerate() {
        let step_path = format!("{path}[{index}]");
        let Some(step) = untag(step).as_mapping() else {
            diagnostics.push(format!(
                "{file}: {step_path}: expected an executable step mapping"
            ));
            continue;
        };
        reject_merge_key(step, file, &step_path, diagnostics);

        if let Some(reference) = mapping_field(step, "uses") {
            check_reference(reference, file, &format!("{step_path}.uses"), diagnostics);
        }
    }
}

fn check_reference(reference: &Value, file: &str, path: &str, diagnostics: &mut Vec<String>) {
    let Some(reference) = untag(reference).as_str() else {
        diagnostics.push(format!(
            "{file}: {path}: uses must be a string action reference"
        ));
        return;
    };

    if reference.starts_with("./") || reference.starts_with("docker://") {
        return;
    }
    if !is_full_sha(reference) {
        diagnostics.push(format!(
            "{file}: {path}: external action must be pinned to a full commit SHA: {reference}"
        ));
    } else if !REVIEWED_ACTION_COMMITS.contains(&reference) {
        diagnostics.push(format!(
            "{file}: {path}: external action must use a reviewed commit SHA: {reference}"
        ));
    }
}

fn reject_merge_key(mapping: &Mapping, file: &str, path: &str, diagnostics: &mut Vec<String>) {
    if mapping_field(mapping, "<<").is_some() {
        diagnostics.push(format!(
            "{file}: {path}: YAML merge keys are unsupported in executable action mappings"
        ));
    }
}

fn mapping_field<'a>(mapping: &'a Mapping, name: &str) -> Option<&'a Value> {
    mapping
        .iter()
        .find_map(|(key, value)| (untag(key).as_str() == Some(name)).then_some(untag(value)))
}

fn untag(value: &Value) -> &Value {
    match value {
        Value::Tagged(tagged) => untag(&tagged.value),
        value => value,
    }
}

fn is_full_sha(reference: &str) -> bool {
    let Some((_, revision)) = reference.rsplit_once('@') else {
        return false;
    };
    revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte <= b'f'))
}
