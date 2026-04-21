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
}

#[derive(Debug, Clone, Deserialize)]
pub struct LanguageTestConfig {
    pub command: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct TestConfig {
    #[serde(default = "default_test_command")]
    pub command: Vec<String>,
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
                let content = std::fs::read_to_string(&p)?;
                let config: Config = toml::from_str(&content)
                    .map_err(|e| anyhow::anyhow!("Invalid togi.toml at {}: {e}", p.display()))?;
                Ok(config)
            }
            None => Ok(Config::default()),
        }
    }

    /// If no test command was explicitly set in togi.toml, auto-detect from project files.
    pub fn resolve_test_command(&mut self, project_root: &Path) {
        if self.test.command.is_empty() {
            self.test.command = detect_test_command(project_root);
        }
    }

    /// Write a template togi.toml to the given path.
    pub fn write_template(path: &Path) -> anyhow::Result<()> {
        let template = r#"# togi.toml — mutation testing configuration

[test]
command = ["cargo", "test"]
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
}
