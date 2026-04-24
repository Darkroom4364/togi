use anyhow::Context;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub test: TestConfig,
    #[serde(default)]
    pub diff: DiffConfig,
    #[serde(default)]
    pub mutations: MutationConfig,
    #[serde(default)]
    pub projects: HashMap<String, ProjectConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    pub path: PathBuf,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub test: Option<ProjectTestConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectTestConfig {
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LanguageTestConfig {
    pub command: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct TestConfig {
    #[serde(default = "default_test_command")]
    pub command: Vec<String>,
    #[serde(default)]
    pub build_command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default = "default_jobs")]
    pub jobs: usize,
    #[serde(default)]
    pub languages: HashMap<String, LanguageTestConfig>,
}

#[derive(Debug, Deserialize)]
pub struct DiffConfig {
    #[serde(default = "default_base")]
    pub base: String,
}

#[derive(Debug, Deserialize)]
pub struct MutationConfig {
    #[serde(default = "default_max_per_run")]
    pub max_per_run: usize,
    pub coverage_file: Option<PathBuf>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default = "default_true")]
    pub skip_noisy_files: bool,
}

fn default_true() -> bool {
    true
}

fn default_test_command() -> Vec<String> {
    vec![]
}

/// Auto-detect the test command based on project files in the given root.
fn has_file_with_ext(dir: &Path, ext: &str) -> bool {
    dir.read_dir()
        .map(|entries| {
            entries.filter_map(Result::ok).any(|e| {
                e.path()
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(ext))
            })
        })
        .unwrap_or(false)
}

pub fn detect_test_command(project_root: &Path) -> Vec<String> {
    let mut detected: Vec<(&str, Vec<String>)> = Vec::new();

    if project_root.join("Cargo.toml").exists() {
        detected.push(("Cargo.toml", vec!["cargo".into(), "test".into()]));
    }
    if project_root.join("go.mod").exists() {
        detected.push(("go.mod", vec!["go".into(), "test".into(), "./...".into()]));
    }
    if project_root.join("pyproject.toml").exists()
        || project_root.join("setup.py").exists()
        || project_root.join("setup.cfg").exists()
    {
        detected.push(("pyproject.toml/setup.py", vec!["pytest".into()]));
    }
    if project_root.join("package.json").exists() {
        detected.push(("package.json", vec!["npm".into(), "test".into()]));
    }
    if project_root.join("pom.xml").exists() {
        detected.push(("pom.xml", vec!["mvn".into(), "test".into()]));
    }
    if project_root.join("build.gradle").exists() || project_root.join("build.gradle.kts").exists()
    {
        detected.push(("build.gradle", vec!["./gradlew".into(), "test".into()]));
    }
    if project_root.join("Gemfile").exists() {
        detected.push((
            "Gemfile",
            vec!["bundle".into(), "exec".into(), "rspec".into()],
        ));
    }
    if project_root.join("CMakeLists.txt").exists() {
        detected.push(("CMakeLists.txt", vec!["ctest".into()]));
    }
    if has_file_with_ext(project_root, "sln") || has_file_with_ext(project_root, "csproj") {
        detected.push((".sln/.csproj", vec!["dotnet".into(), "test".into()]));
    }

    if detected.len() > 1 {
        let names: Vec<&str> = detected.iter().map(|(name, _)| *name).collect();
        eprintln!(
            "warning: multiple build systems detected ({}). Using `{}`. \
             Set [test] command in togi.toml to override.",
            names.join(", "),
            detected[0].1.join(" ")
        );
    }

    if let Some((_, cmd)) = detected.into_iter().next() {
        cmd
    } else {
        eprintln!(
            "warning: no known build system found. Falling back to `make test`. \
             Set [test] command in togi.toml to override."
        );
        vec!["make".into(), "test".into()]
    }
}

/// Returns failfast args to append to a test command, based on the test runner.
pub fn failfast_args(command: &[String]) -> Vec<String> {
    match command.first().map(|s| s.as_str()) {
        Some("go") => vec!["-failfast".into()],
        Some("pytest") => vec!["-x".into()],
        Some("npx") => vec!["--bail".into()],
        Some("npm") => vec!["--".into(), "--bail".into()],
        Some("mvn") => vec!["--fail-fast".into()],
        Some("./gradlew") | Some("gradlew") => vec!["--fail-fast".into()],
        Some("bundle") => vec!["--fail-fast".into()],
        Some("dotnet") => vec!["--".into(), "--fail-fast".into()],
        _ => vec![],
    }
}

pub fn detect_build_command(project_root: &Path) -> Vec<String> {
    if project_root.join("Cargo.toml").exists() {
        return vec!["cargo".into(), "check".into()];
    }
    if project_root.join("go.mod").exists() {
        return vec!["go".into(), "build".into(), "./...".into()];
    }
    if project_root.join("tsconfig.json").exists() {
        return vec!["npx".into(), "tsc".into(), "--noEmit".into()];
    }
    if project_root.join("pom.xml").exists() {
        return vec!["mvn".into(), "compile".into(), "-q".into()];
    }
    if project_root.join("build.gradle").exists() || project_root.join("build.gradle.kts").exists()
    {
        return vec!["./gradlew".into(), "compileJava".into()];
    }
    if has_file_with_ext(project_root, "sln") || has_file_with_ext(project_root, "csproj") {
        return vec!["dotnet".into(), "build".into(), "--no-restore".into()];
    }
    vec![]
}

fn default_timeout() -> u64 {
    30
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn default_base() -> String {
    "origin/main".into()
}

fn default_max_per_run() -> usize {
    20
}

impl TestConfig {
    pub fn command_for_language(&self, language: &str) -> &[String] {
        if let Some(lang_config) = self.languages.get(language) {
            &lang_config.command
        } else {
            &self.command
        }
    }
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            command: default_test_command(),
            build_command: vec![],
            timeout: default_timeout(),
            jobs: default_jobs(),
            languages: HashMap::new(),
        }
    }
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            base: default_base(),
        }
    }
}

impl Default for MutationConfig {
    fn default() -> Self {
        Self {
            max_per_run: default_max_per_run(),
            coverage_file: None,
            exclude_paths: vec![],
            skip_noisy_files: true,
        }
    }
}

impl Config {
    /// Load config from an explicit path, or search upward from the current directory.
    /// Returns defaults if no config file is found.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Config> {
        let config_path = match path {
            Some(p) => Some(p.to_path_buf()),
            None => find_config(),
        };

        match config_path {
            Some(p) => {
                let content = std::fs::read_to_string(&p)
                    .with_context(|| format!("could not read {}", p.display()))?;
                let config: Config = toml::from_str(&content)
                    .with_context(|| format!("invalid togi.toml at {}", p.display()))?;
                Ok(config)
            }
            None => Ok(Config::default()),
        }
    }

    /// Find the project config whose `path` is a prefix of `file_path`.
    /// Longest match wins when projects are nested.
    pub fn project_for_path(&self, file_path: &Path) -> Option<(&str, &ProjectConfig)> {
        self.projects
            .iter()
            .filter(|(_, proj)| file_path.starts_with(&proj.path))
            .max_by_key(|(_, proj)| proj.path.components().count())
            .map(|(name, proj)| (name.as_str(), proj))
    }

    /// Warn if any configured language keys don't match known language names.
    pub fn warn_unknown_languages(&self, known: &[&str]) {
        for key in self.test.languages.keys() {
            if !known.contains(&key.as_str()) {
                eprintln!(
                    "warning: unknown language '{}' in [test.languages]. \
                     Known languages: {}",
                    key,
                    known.join(", ")
                );
            }
        }
    }

    /// If no test command was explicitly set in togi.toml, auto-detect from project files.
    pub fn resolve_test_command(&mut self, project_root: &Path) {
        if self.test.command.is_empty() {
            self.test.command = detect_test_command(project_root);
        }
    }

    /// If no build command was explicitly set in togi.toml, auto-detect from project files.
    pub fn resolve_build_command(&mut self, project_root: &Path) {
        if self.test.build_command.is_empty() {
            self.test.build_command = detect_build_command(project_root);
        }
    }

    /// Write a template togi.toml to the given path.
    pub fn write_template(path: &Path) -> anyhow::Result<()> {
        let template = r#"# togi.toml — mutation testing configuration

[test]
command = ["cargo", "test"]
# build_command = ["cargo", "check"]  # auto-detected; compile check before running tests
timeout = 30
# jobs = 4  # defaults to number of CPUs

# Per-language test commands (key = language name, e.g. typescript, python, go)
# [test.languages.typescript]
# command = ["npx", "jest"]

[diff]
base = "origin/main"

[mutations]
max_per_run = 20
"#;
        std::fs::write(path, template)?;
        Ok(())
    }
}

/// Walk from the current directory upward looking for togi.toml.
fn find_config() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("togi.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_toml() {
        let toml_str = r#"
[test]
command = ["make", "test"]
timeout = 60
jobs = 8

[diff]
base = "origin/develop"

[mutations]
max_per_run = 50
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.test.command, vec!["make", "test"]);
        assert_eq!(config.test.timeout, 60);
        assert_eq!(config.test.jobs, 8);
        assert_eq!(config.diff.base, "origin/develop");
        assert_eq!(config.mutations.max_per_run, 50);
    }

    #[test]
    fn defaults_when_empty() {
        let config: Config = toml::from_str("").unwrap();
        // Command is empty sentinel when not explicitly configured
        assert!(config.test.command.is_empty());
        assert_eq!(config.test.timeout, 30);
        assert!(config.test.jobs >= 1);
        assert_eq!(config.diff.base, "origin/main");
        assert_eq!(config.mutations.max_per_run, 20);
    }

    #[test]
    fn partial_config() {
        let toml_str = r#"
[test]
timeout = 120
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.test.timeout, 120);
        // Command is empty when not explicitly set
        assert!(config.test.command.is_empty());
        assert_eq!(config.diff.base, "origin/main");
    }

    #[test]
    fn write_template_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("togi.toml");
        Config::write_template(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[test]"));
        assert!(content.contains("command = [\"cargo\", \"test\"]"));
    }

    #[test]
    fn detect_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["cargo", "test"]);
    }

    #[test]
    fn detect_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["go", "test", "./..."]);
    }

    #[test]
    fn detect_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["npm", "test"]);
    }

    #[test]
    fn detect_fallback_make_test() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["make", "test"]);
    }

    #[test]
    fn parse_coverage_file_option() {
        let toml_str = r#"
[mutations]
coverage_file = "coverage.lcov"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.mutations.coverage_file,
            Some(PathBuf::from("coverage.lcov"))
        );
    }

    #[test]
    fn has_file_with_ext_returns_false_when_no_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.txt"), "").unwrap();
        assert!(!has_file_with_ext(dir.path(), "rs"));
    }

    #[test]
    fn has_file_with_ext_returns_false_for_nonexistent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("nonexistent");
        assert!(!has_file_with_ext(&bad, "rs"));
    }

    #[test]
    fn detect_gradle_kts_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("build.gradle.kts"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["./gradlew", "test"]);
    }

    #[test]
    fn detect_gemfile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Gemfile"), "").unwrap();
        assert_eq!(
            detect_test_command(dir.path()),
            vec!["bundle", "exec", "rspec"]
        );
    }

    #[test]
    fn coverage_file_defaults_to_none() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.mutations.coverage_file.is_none());
    }

    #[test]
    fn detect_cmake() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CMakeLists.txt"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["ctest"]);
    }

    #[test]
    fn detect_csproj_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MyApp.csproj"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["dotnet", "test"]);
    }

    #[test]
    fn detect_sln_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MyApp.sln"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["dotnet", "test"]);
    }

    #[test]
    fn detect_pom_xml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pom.xml"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["mvn", "test"]);
    }

    #[test]
    fn default_max_per_run_is_20() {
        assert_eq!(default_max_per_run(), 20);
    }

    #[test]
    fn default_timeout_is_30() {
        assert_eq!(default_timeout(), 30);
    }

    #[test]
    fn parse_per_language_commands() {
        let toml_str = r#"
[test]
command = ["cargo", "test"]

[test.languages.typescript]
command = ["npx", "jest"]

[test.languages.python]
command = ["pytest"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.test.languages["typescript"].command,
            vec!["npx", "jest"]
        );
        assert_eq!(config.test.languages["python"].command, vec!["pytest"]);
    }

    #[test]
    fn command_for_language_resolution() {
        let toml_str = r#"
[test]
command = ["cargo", "test"]

[test.languages.typescript]
command = ["npx", "jest"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.test.command_for_language("typescript"),
            &["npx", "jest"]
        );
        assert_eq!(config.test.command_for_language("go"), &["cargo", "test"]);
    }

    #[test]
    fn empty_languages_backward_compat() {
        let toml_str = r#"
[test]
command = ["make", "test"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.test.languages.is_empty());
        assert_eq!(
            config.test.command_for_language("anything"),
            &["make", "test"]
        );
    }

    #[test]
    fn parse_projects_config() {
        let toml_str = r#"
[projects.api]
path = "services/api"
language = "rust"

[projects.api.test]
command = ["cargo", "test"]
timeout = 60

[projects.web]
path = "services/web"

[projects.web.test]
command = ["npm", "test"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.projects.len(), 2);

        let api = &config.projects["api"];
        assert_eq!(api.path, PathBuf::from("services/api"));
        assert_eq!(api.language.as_deref(), Some("rust"));
        let api_test = api.test.as_ref().unwrap();
        assert_eq!(
            api_test.command.as_deref(),
            Some(vec!["cargo".into(), "test".into()].as_slice())
        );
        assert_eq!(api_test.timeout, Some(60));

        let web = &config.projects["web"];
        assert_eq!(web.path, PathBuf::from("services/web"));
        assert!(web.language.is_none());
    }

    #[test]
    fn project_for_path_matches_prefix() {
        let toml_str = r#"
[projects.api]
path = "services/api"

[projects.web]
path = "services/web"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (name, _) = config
            .project_for_path(Path::new("services/api/src/main.rs"))
            .unwrap();
        assert_eq!(name, "api");

        let (name, _) = config
            .project_for_path(Path::new("services/web/index.js"))
            .unwrap();
        assert_eq!(name, "web");

        assert!(
            config
                .project_for_path(Path::new("other/file.rs"))
                .is_none()
        );
    }

    #[test]
    fn project_for_path_longest_match_wins() {
        let toml_str = r#"
[projects.services]
path = "services"

[projects.api]
path = "services/api"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let (name, _) = config
            .project_for_path(Path::new("services/api/src/lib.rs"))
            .unwrap();
        assert_eq!(name, "api");

        let (name, _) = config
            .project_for_path(Path::new("services/web/app.js"))
            .unwrap();
        assert_eq!(name, "services");
    }

    #[test]
    fn no_projects_by_default() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.projects.is_empty());
    }

    #[test]
    fn project_without_test_override() {
        let toml_str = r#"
[projects.lib]
path = "lib"
language = "go"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let lib = &config.projects["lib"];
        assert!(lib.test.is_none());
        assert_eq!(lib.language.as_deref(), Some("go"));
    }

    #[test]
    fn parse_exclude_paths_and_skip_noisy_files() {
        let toml_str = r#"
[mutations]
exclude_paths = ["vendor/**", "*.generated.ts"]
skip_noisy_files = false
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.mutations.exclude_paths,
            vec!["vendor/**", "*.generated.ts"]
        );
        assert!(!config.mutations.skip_noisy_files);
    }

    #[test]
    fn skip_noisy_files_defaults_to_true() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.mutations.skip_noisy_files);
        assert!(config.mutations.exclude_paths.is_empty());
    }
}
