use anyhow::Context;
use clap::ValueEnum;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub path: PathBuf,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub test: Option<ProjectTestConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectTestConfig {
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageTestConfig {
    pub command: Vec<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Debug)]
pub struct TestConfig {
    pub profile: Option<ResourceProfile>,
    pub command: Vec<String>,
    pub build_command: Vec<String>,
    pub sandbox_command: Vec<String>,
    pub timeout: u64,
    pub calibrate_timeout: bool,
    pub timeout_multiplier: f64,
    pub timeout_slack: u64,
    pub jobs: usize,
    pub languages: HashMap<String, LanguageTestConfig>,
    jobs_explicit: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTestConfig {
    #[serde(default)]
    profile: Option<ResourceProfile>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    build_command: Vec<String>,
    #[serde(default)]
    sandbox_command: Vec<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    calibrate_timeout: Option<bool>,
    #[serde(default)]
    timeout_multiplier: Option<f64>,
    #[serde(default)]
    timeout_slack: Option<u64>,
    #[serde(default)]
    jobs: Option<usize>,
    #[serde(default)]
    languages: HashMap<String, LanguageTestConfig>,
}

impl<'de> Deserialize<'de> for TestConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawTestConfig::deserialize(deserializer)?;
        Ok(Self {
            profile: raw.profile,
            command: raw.command,
            build_command: raw.build_command,
            sandbox_command: raw.sandbox_command,
            timeout: raw.timeout.unwrap_or_else(default_timeout),
            calibrate_timeout: raw.calibrate_timeout.unwrap_or(false),
            timeout_multiplier: raw.timeout_multiplier.unwrap_or(4.0),
            timeout_slack: raw.timeout_slack.unwrap_or(2),
            jobs: raw.jobs.unwrap_or_else(default_jobs),
            languages: raw.languages,
            jobs_explicit: raw.jobs.is_some(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum ResourceProfile {
    Cool,
    Balanced,
    Ci,
}

impl ResourceProfile {
    pub fn default_jobs(self) -> usize {
        std::thread::available_parallelism()
            .map(|n| self.jobs_for_available_parallelism(n.get()))
            .unwrap_or(1)
    }

    pub fn jobs_for_available_parallelism(self, available: usize) -> usize {
        match self {
            Self::Cool => 1,
            Self::Balanced => default_jobs_for_available_parallelism(available),
            Self::Ci => available.max(1),
        }
    }

    pub fn default_fail_fast(self) -> bool {
        matches!(self, Self::Cool)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum CoverageMode {
    Auto,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffConfig {
    #[serde(default = "default_base")]
    pub base: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationConfig {
    #[serde(default = "default_max_per_run")]
    pub max_per_run: usize,
    #[serde(default)]
    pub coverage: Option<CoverageMode>,
    pub coverage_file: Option<PathBuf>,
    #[serde(default)]
    pub coverage_command: Vec<String>,
    pub test_selection_file: Option<PathBuf>,
    #[serde(default)]
    pub confirm_survivors: bool,
    #[serde(default)]
    pub min_line_coverage: Option<f64>,
    #[serde(default)]
    pub min_diff_coverage: Option<f64>,
    #[serde(default)]
    pub fail_on_uncovered_diff: bool,
    #[serde(default = "default_max_per_file")]
    pub max_per_file: usize,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default = "default_true")]
    pub skip_noisy_files: bool,
    #[serde(default = "default_true")]
    pub respect_workspace_ignores: bool,
    #[serde(default = "default_true")]
    pub incremental_history: bool,
    #[serde(default)]
    pub operators: Vec<String>,
    #[serde(default)]
    pub schemata: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JavaScriptRunner {
    Bun,
    Npm,
    Pnpm,
    Yarn,
}

impl JavaScriptRunner {
    fn detect(project_root: &Path) -> Option<Self> {
        if !project_root.join("package.json").exists() {
            return None;
        }
        Some(if project_root.join("pnpm-lock.yaml").exists() {
            Self::Pnpm
        } else if project_root.join("yarn.lock").exists() {
            Self::Yarn
        } else if project_root.join("bun.lockb").exists() || project_root.join("bun.lock").exists()
        {
            Self::Bun
        } else {
            Self::Npm
        })
    }

    fn binary(self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
        }
    }

    fn test_command(self) -> Vec<String> {
        vec![self.binary().into(), "test".into()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DetectedTestRoute {
    marker: &'static str,
    command: Vec<String>,
    languages: &'static [&'static str],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProjectInspection {
    has_cargo_toml: bool,
    has_go_mod: bool,
    python_manifest: Option<&'static str>,
    javascript_runner: Option<JavaScriptRunner>,
    has_pom_xml: bool,
    has_gradle: bool,
    gradle_manifest: Option<&'static str>,
    has_gemfile: bool,
    has_cmake: bool,
    has_dotnet_project: bool,
    has_tsconfig: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinCoverageAdapter {
    Go,
}

impl ProjectInspection {
    fn scan(project_root: &Path) -> Self {
        let python_manifest = if project_root.join("pyproject.toml").exists() {
            Some("pyproject.toml")
        } else if project_root.join("setup.py").exists() {
            Some("setup.py")
        } else if project_root.join("setup.cfg").exists() {
            Some("setup.cfg")
        } else {
            None
        };
        let gradle_manifest = if project_root.join("build.gradle").exists() {
            Some("build.gradle")
        } else if project_root.join("build.gradle.kts").exists() {
            Some("build.gradle.kts")
        } else {
            None
        };

        Self {
            has_cargo_toml: project_root.join("Cargo.toml").exists(),
            has_go_mod: project_root.join("go.mod").exists(),
            python_manifest,
            javascript_runner: JavaScriptRunner::detect(project_root),
            has_pom_xml: project_root.join("pom.xml").exists(),
            has_gradle: gradle_manifest.is_some(),
            gradle_manifest,
            has_gemfile: project_root.join("Gemfile").exists(),
            has_cmake: project_root.join("CMakeLists.txt").exists(),
            has_dotnet_project: has_file_with_ext(project_root, "sln")
                || has_file_with_ext(project_root, "csproj"),
            has_tsconfig: project_root.join("tsconfig.json").exists(),
        }
    }

    fn detected_test_routes(&self) -> Vec<DetectedTestRoute> {
        let mut detected = Vec::new();
        if self.has_cargo_toml {
            detected.push(DetectedTestRoute {
                marker: "Cargo.toml",
                command: vec!["cargo".into(), "test".into()],
                languages: &["rust"],
            });
        }
        if self.has_go_mod {
            detected.push(DetectedTestRoute {
                marker: "go.mod",
                command: vec!["go".into(), "test".into(), "./...".into()],
                languages: &["go"],
            });
        }
        if let Some(marker) = self.python_manifest {
            detected.push(DetectedTestRoute {
                marker,
                command: vec!["pytest".into()],
                languages: &["python"],
            });
        }
        if let Some(runner) = self.javascript_runner {
            detected.push(DetectedTestRoute {
                marker: "package.json",
                command: runner.test_command(),
                languages: &["typescript"],
            });
        }
        if self.has_pom_xml {
            detected.push(DetectedTestRoute {
                marker: "pom.xml",
                command: vec!["mvn".into(), "test".into()],
                languages: &["java"],
            });
        }
        if let Some(marker) = self.gradle_manifest {
            detected.push(DetectedTestRoute {
                marker,
                command: vec!["./gradlew".into(), "test".into()],
                languages: &["java"],
            });
        }
        if self.has_gemfile {
            detected.push(DetectedTestRoute {
                marker: "Gemfile",
                command: vec!["bundle".into(), "exec".into(), "rspec".into()],
                languages: &["ruby"],
            });
        }
        if self.has_cmake {
            detected.push(DetectedTestRoute {
                marker: "CMakeLists.txt",
                command: vec!["ctest".into()],
                languages: &["c", "cpp"],
            });
        }
        if self.has_dotnet_project {
            detected.push(DetectedTestRoute {
                marker: ".sln/.csproj",
                command: vec!["dotnet".into(), "test".into()],
                languages: &["c_sharp"],
            });
        }
        detected
    }

    fn detect_build_command(&self) -> Vec<String> {
        if self.has_cargo_toml {
            return vec!["cargo".into(), "check".into()];
        }
        if self.has_go_mod {
            return vec!["go".into(), "build".into(), "./...".into()];
        }
        if self.has_tsconfig {
            return vec!["npx".into(), "tsc".into(), "--noEmit".into()];
        }
        if self.has_pom_xml {
            return vec!["mvn".into(), "compile".into(), "-q".into()];
        }
        if self.has_gradle {
            return vec!["./gradlew".into(), "compileJava".into()];
        }
        if self.has_dotnet_project {
            return vec!["dotnet".into(), "build".into(), "--no-restore".into()];
        }
        vec![]
    }

    fn detect_builtin_coverage_adapter(&self) -> Option<BuiltinCoverageAdapter> {
        if self.has_go_mod {
            return Some(BuiltinCoverageAdapter::Go);
        }
        None
    }
}

pub fn detect_test_command(project_root: &Path) -> Vec<String> {
    let detected = ProjectInspection::scan(project_root).detected_test_routes();
    select_test_command(&detected)
}

fn select_test_command(detected: &[DetectedTestRoute]) -> Vec<String> {
    if detected.len() > 1 {
        let names: Vec<&str> = detected.iter().map(|route| route.marker).collect();
        eprintln!(
            "warning: multiple build systems detected ({}). Using `{}`. \
             Set [test] command in togi.toml to override.",
            names.join(", "),
            detected[0].command.join(" ")
        );
    }

    select_primary_test_command(detected)
}

fn select_primary_test_command(detected: &[DetectedTestRoute]) -> Vec<String> {
    if let Some(route) = detected.first() {
        route.command.clone()
    } else {
        eprintln!(
            "warning: no known build system found. Falling back to `make test`. \
             Set [test] command in togi.toml to override."
        );
        vec!["make".into(), "test".into()]
    }
}

fn validate_init_language_routes(routes: &[DetectedTestRoute]) -> anyhow::Result<()> {
    let mut routes_by_language = HashMap::new();

    for route in routes {
        for &language in route.languages {
            if let Some(previous) = routes_by_language.insert(language, route) {
                if previous.command != route.command {
                    anyhow::bail!(
                        "multiple test commands for {language} detected ({} and {}); \
                         configure an explicit [test] command or [projects.*.test] routes",
                        previous.marker,
                        route.marker
                    );
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
pub struct AmbiguousTestCommand {
    candidates: Vec<&'static str>,
}

impl AmbiguousTestCommand {
    pub fn error(&self) -> anyhow::Error {
        anyhow::anyhow!(
            "multiple test runtimes detected ({}); set [test] command in togi.toml or pass --test-cmd <command>",
            self.candidates.join(", ")
        )
    }

    pub fn error_for_path(&self, path: &Path) -> anyhow::Error {
        anyhow::anyhow!(
            "multiple test runtimes detected ({}); {} has no explicit project or language test command, so set [test] command in togi.toml or pass --test-cmd <command>",
            self.candidates.join(", "),
            path.display()
        )
    }
}

#[derive(Debug)]
pub enum TestCommandResolution {
    Resolved,
    Ambiguous(AmbiguousTestCommand),
}

// Mirrors runner::normalized_cache_path so ambiguity preflight chooses the
// same project route that runner would select.
fn normalized_test_command_path(project_root: &Path, path: &Path) -> String {
    let relative = if path.is_absolute() {
        path.canonicalize()
            .ok()
            .and_then(|path| {
                project_root
                    .canonicalize()
                    .ok()
                    .and_then(|root| path.strip_prefix(root).ok().map(PathBuf::from))
            })
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    };

    relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Returns failfast args to append to a test command, based on the test runner.
pub fn failfast_args(command: &[String]) -> Vec<String> {
    match command.first().map(|s| s.as_str()) {
        Some("go") => vec!["-failfast".into()],
        Some("pytest") => vec!["-x".into()],
        Some("npx") => vec!["--bail".into()],
        Some("npm") | Some("pnpm") | Some("yarn") | Some("bun") => {
            vec!["--".into(), "--bail".into()]
        }
        Some("mvn") => vec!["--fail-fast".into()],
        Some("./gradlew") | Some("gradlew") => vec!["--fail-fast".into()],
        Some("bundle") => vec!["--fail-fast".into()],
        Some("dotnet") => vec!["--".into(), "--fail-fast".into()],
        _ => vec![],
    }
}

pub fn detect_build_command(project_root: &Path) -> Vec<String> {
    ProjectInspection::scan(project_root).detect_build_command()
}

pub fn detect_builtin_coverage_adapter(project_root: &Path) -> Option<BuiltinCoverageAdapter> {
    ProjectInspection::scan(project_root).detect_builtin_coverage_adapter()
}

fn default_timeout() -> u64 {
    30
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| default_jobs_for_available_parallelism(n.get()))
        .unwrap_or(1)
}

fn default_jobs_for_available_parallelism(available: usize) -> usize {
    if available <= 2 { 1 } else { 2 }
}

fn default_base() -> String {
    crate::diff::DEFAULT_DIFF_BASE.into()
}

fn default_max_per_run() -> usize {
    20
}

fn default_max_per_file() -> usize {
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

    pub fn jobs_was_explicit(&self) -> bool {
        self.jobs_explicit
    }
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            profile: None,
            command: default_test_command(),
            build_command: vec![],
            sandbox_command: vec![],
            timeout: default_timeout(),
            calibrate_timeout: false,
            timeout_multiplier: 4.0,
            timeout_slack: 2,
            jobs: default_jobs(),
            languages: HashMap::new(),
            jobs_explicit: false,
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
            max_per_file: default_max_per_file(),
            coverage: None,
            coverage_file: None,
            coverage_command: vec![],
            test_selection_file: None,
            confirm_survivors: false,
            min_line_coverage: None,
            min_diff_coverage: None,
            fail_on_uncovered_diff: false,
            exclude_paths: vec![],
            skip_noisy_files: true,
            respect_workspace_ignores: true,
            incremental_history: true,
            operators: vec![],
            schemata: true,
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

    /// Returns whether a nonempty configured route could require path-aware command resolution.
    pub fn has_configured_test_command_routes(&self) -> bool {
        self.test
            .languages
            .values()
            .any(|language| !language.command.is_empty())
            || self.projects.values().any(|project| {
                project
                    .test
                    .as_ref()
                    .and_then(|test| test.command.as_ref())
                    .is_some_and(|command| !command.is_empty())
            })
    }

    /// Mirrors runner project/language precedence before it falls back to the global command.
    pub fn has_configured_test_command_for_path(
        &self,
        project_root: &Path,
        file_path: &Path,
        language: &str,
    ) -> bool {
        let file_path = normalized_test_command_path(project_root, file_path);
        let file_parts: Vec<&str> = file_path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        let project = self
            .projects
            .values()
            .filter_map(|project| {
                let project_path = normalized_test_command_path(project_root, &project.path);
                let project_parts: Vec<&str> = project_path
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .collect();
                (!project_parts.is_empty()
                    && project_parts.len() <= file_parts.len()
                    && file_parts
                        .iter()
                        .zip(&project_parts)
                        .all(|(file, project)| file == project))
                .then_some((project_parts.len(), project))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, project)| project);

        if let Some(command) = project
            .and_then(|project| project.test.as_ref())
            .and_then(|test| test.command.as_ref())
        {
            return !command.is_empty();
        }

        self.test
            .languages
            .get(language)
            .is_some_and(|language| !language.command.is_empty())
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

    /// Resolve a global test command, leaving it unset when detection is ambiguous.
    pub fn resolve_test_command(&mut self, project_root: &Path) -> TestCommandResolution {
        if !self.test.command.is_empty() {
            return TestCommandResolution::Resolved;
        }

        let detected = ProjectInspection::scan(project_root).detected_test_routes();
        if detected.len() > 1 {
            return TestCommandResolution::Ambiguous(AmbiguousTestCommand {
                candidates: detected.iter().map(|route| route.marker).collect(),
            });
        }

        self.test.command = if let Some(route) = detected.into_iter().next() {
            route.command
        } else {
            eprintln!(
                "warning: no known build system found. Falling back to `make test`. \
                 Set [test] command in togi.toml to override."
            );
            vec!["make".into(), "test".into()]
        };
        TestCommandResolution::Resolved
    }

    /// If no build command was explicitly set in togi.toml, auto-detect from project files.
    pub fn resolve_build_command(&mut self, project_root: &Path) {
        if self.test.build_command.is_empty() {
            self.test.build_command = detect_build_command(project_root);
        }
    }

    /// Write a template togi.toml to the given path.
    pub fn write_template(path: &Path) -> anyhow::Result<()> {
        let project_root = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let inspection = ProjectInspection::scan(project_root);
        let routes = inspection.detected_test_routes();
        validate_init_language_routes(&routes)?;
        let test_cmd = select_primary_test_command(&routes);
        let build_cmd = inspection.detect_build_command();
        let base_toml = toml::Value::String(crate::diff::init_diff_base(project_root)).to_string();

        let test_cmd_toml: Vec<String> = test_cmd.iter().map(|s| format!("\"{}\"", s)).collect();

        let mut template = format!(
            "# togi.toml — mutation testing configuration\n\
             \n\
             [test]\n\
             command = [{}]\n",
            test_cmd_toml.join(", ")
        );

        if !build_cmd.is_empty() {
            let build_cmd_toml: Vec<String> =
                build_cmd.iter().map(|s| format!("\"{}\"", s)).collect();
            template.push_str(&format!(
                "# build_command = [{}]  # uncomment to pre-filter mutations that don't compile\n",
                build_cmd_toml.join(", ")
            ));
        }

        template.push_str(
            "# sandbox_command = [\"bwrap\", \"--ro-bind\", \"/\", \"/\", \"--dev\", \"/dev\", \"--proc\", \"/proc\", \"--\"]\n\
             # Optional wrapper that runs every build and test command inside your own sandbox tool.\n\
             # Leave unset to run directly on the host or CI runner.\n",
        );

        template.push_str(
            "# coverage = \"auto\"  # collect coverage through a built-in adapter when supported\n\
             # coverage_file = \"coverage/lcov.info\"  # enable LCOV filtering and coverage gates\n\
             # coverage_command = [\"./scripts/collect-coverage.sh\"]  # generate LCOV before mutation filtering\n\
             # min_line_coverage = 80.0  # fail if overall LCOV line coverage drops below this\n\
             # min_diff_coverage = 90.0  # fail if changed-line coverage drops below this\n\
             # fail_on_uncovered_diff = false  # fail if any changed line is uncovered\n",
        );

        template.push_str(&format!(
            "# profile = \"cool\"  # cool, balanced, or ci; explicit jobs still win\n\
             timeout = 30\n\
             # calibrate_timeout = false  # derive timeout from one unmutated baseline run\n\
             # timeout_multiplier = 4.0\n\
             # timeout_slack = 2\n\
             # jobs = {}  # conservative local default; raise in CI for throughput\n\
             \n",
            default_jobs()
        ));

        let mut lang_sections: Vec<String> = Vec::new();
        for route in routes.iter().skip(1) {
            let command_toml: Vec<String> = route
                .command
                .iter()
                .map(|command| toml::Value::String(command.clone()).to_string())
                .collect();
            for language in route.languages {
                lang_sections.push(format!(
                    "[test.languages.{language}]\ncommand = [{}]\n",
                    command_toml.join(", ")
                ));
            }
        }

        if !lang_sections.is_empty() {
            template.push_str("# Per-language test commands (auto-detected)\n");
            for section in &lang_sections {
                template.push('\n');
                template.push_str(section);
            }
            template.push('\n');
        }

        template.push_str(&format!(
            "[diff]\n\
             base = {base_toml}\n\
             \n\
             [mutations]\n\
             max_per_run = 20\n\
             # max_per_file = 20  # cap mutations per source file (0 = unlimited)\n\
             # schemata = true  # use supported mutant schemata as the fast path\n\
             # incremental_history = true  # reuse safe results from previous runs\n\
             # respect_workspace_ignores = true  # honor .ignore/.gitignore in mutation workspaces\n",
        ));

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
profile = "ci"
command = ["make", "test"]
timeout = 60
calibrate_timeout = true
timeout_multiplier = 3.5
timeout_slack = 4
jobs = 8

[diff]
base = "origin/develop"

[mutations]
max_per_run = 50
schemata = true
confirm_survivors = true
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.test.profile, Some(ResourceProfile::Ci));
        assert_eq!(config.test.command, vec!["make", "test"]);
        assert_eq!(config.test.timeout, 60);
        assert!(config.test.calibrate_timeout);
        assert_eq!(config.test.timeout_multiplier, 3.5);
        assert_eq!(config.test.timeout_slack, 4);
        assert_eq!(config.test.jobs, 8);
        assert!(config.test.jobs_was_explicit());
        assert!(config.test.sandbox_command.is_empty());
        assert_eq!(config.diff.base, "origin/develop");
        assert_eq!(config.mutations.max_per_run, 50);
        assert!(config.mutations.schemata);
        assert!(config.mutations.confirm_survivors);
    }

    #[test]
    fn rejects_unknown_nested_config_fields() {
        let cases = [
            r#"
[test]
commnad = ["make", "test"]
"#,
            r#"
[test.languages.python]
command = ["pytest"]
timeuot = 10
"#,
            r#"
[mutations]
max_per_rnu = 10
"#,
            r#"
[projects.api]
path = "services/api"

[projects.api.test]
commnad = ["cargo", "test"]
"#,
        ];

        for case in cases {
            assert!(toml::from_str::<Config>(case).is_err(), "{case}");
        }
    }

    #[test]
    fn example_config_matches_schema() {
        let content = include_str!("../togi.toml.example");
        let config: Config = toml::from_str(content).unwrap();
        assert_eq!(config.test.command, vec!["go", "test", "./..."]);
        assert!(config.test.build_command.is_empty());
        assert!(config.test.sandbox_command.is_empty());
        assert_eq!(config.test.timeout, 30);
        assert_eq!(config.test.jobs, 2);
        assert_eq!(config.test.languages["python"].command, vec!["pytest"]);
        assert_eq!(config.diff.base, "origin/main");
        assert_eq!(
            config.mutations.coverage_file,
            Some("coverage/lcov.info".into())
        );
        assert_eq!(
            config.mutations.test_selection_file,
            Some("coverage/test-selection.json".into())
        );
        assert!(!config.mutations.confirm_survivors);
        assert!(config.mutations.min_line_coverage.is_none());
        assert!(config.mutations.min_diff_coverage.is_none());
        assert!(!config.mutations.fail_on_uncovered_diff);
        assert!(config.mutations.skip_noisy_files);
        assert!(config.mutations.respect_workspace_ignores);
        assert!(config.mutations.incremental_history);
        assert!(config.mutations.schemata);
        let project = &config.projects["api"];
        assert_eq!(project.path, PathBuf::from("services/api"));
        assert_eq!(project.test.as_ref().unwrap().timeout, Some(60));
    }

    #[test]
    fn defaults_when_empty() {
        let config: Config = toml::from_str("").unwrap();
        // Command is empty sentinel when not explicitly configured
        assert_eq!(config.test.profile, None);
        assert!(config.test.command.is_empty());
        assert!(config.test.build_command.is_empty());
        assert!(config.test.sandbox_command.is_empty());
        assert!(config.test.languages.is_empty());
        assert_eq!(config.test.timeout, 30);
        assert!(!config.test.calibrate_timeout);
        assert_eq!(config.test.timeout_multiplier, 4.0);
        assert_eq!(config.test.timeout_slack, 2);
        assert!(config.test.jobs >= 1);
        assert!(!config.test.jobs_was_explicit());
        assert_eq!(config.diff.base, "origin/main");
        assert_eq!(config.mutations.max_per_run, 20);
        assert_eq!(config.mutations.max_per_file, 20);
        assert!(config.mutations.operators.is_empty());
        assert!(config.mutations.coverage_file.is_none());
        assert!(config.mutations.test_selection_file.is_none());
        assert!(!config.mutations.confirm_survivors);
        assert!(config.mutations.min_line_coverage.is_none());
        assert!(config.mutations.min_diff_coverage.is_none());
        assert!(!config.mutations.fail_on_uncovered_diff);
        assert!(config.mutations.exclude_paths.is_empty());
        assert!(config.mutations.skip_noisy_files);
        assert!(config.mutations.respect_workspace_ignores);
        assert!(config.mutations.incremental_history);
        assert!(config.mutations.schemata);
        assert!(config.projects.is_empty());
    }

    #[test]
    fn default_jobs_are_conservative_for_local_runs() {
        assert_eq!(default_jobs_for_available_parallelism(0), 1);
        assert_eq!(default_jobs_for_available_parallelism(1), 1);
        assert_eq!(default_jobs_for_available_parallelism(2), 1);
        assert_eq!(default_jobs_for_available_parallelism(3), 2);
        assert_eq!(default_jobs_for_available_parallelism(16), 2);
    }

    #[test]
    fn profile_jobs_are_predictable() {
        assert_eq!(ResourceProfile::Cool.jobs_for_available_parallelism(16), 1);
        assert_eq!(
            ResourceProfile::Balanced.jobs_for_available_parallelism(16),
            2
        );
        assert_eq!(ResourceProfile::Ci.jobs_for_available_parallelism(16), 16);
        assert_eq!(ResourceProfile::Ci.jobs_for_available_parallelism(0), 1);
    }

    #[test]
    fn parse_profile_without_explicit_jobs() {
        let toml_str = r#"
[test]
profile = "cool"
"#;
        let config: Config = toml::from_str(toml_str).expect("profile config should parse");
        assert_eq!(config.test.profile, Some(ResourceProfile::Cool));
        assert!(!config.test.jobs_was_explicit());
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
    fn parse_sandbox_command() {
        let toml_str = r#"
[test]
command = ["cargo", "test"]
sandbox_command = ["bwrap", "--ro-bind", "/", "/", "--"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.test.sandbox_command,
            vec!["bwrap", "--ro-bind", "/", "/", "--"]
        );
    }

    #[test]
    fn write_template_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("togi.toml");
        Config::write_template(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[test]"));
        assert!(content.contains("command = "));
        assert!(content.contains("sandbox_command = "));
        assert!(content.contains("[diff]"));
        assert!(content.contains("[mutations]"));
    }

    #[test]
    fn write_template_detects_cargo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let path = dir.path().join("togi.toml");
        Config::write_template(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("command = [\"cargo\", \"test\"]"));
    }

    #[test]
    fn write_template_detects_pnpm() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        let path = dir.path().join("togi.toml");
        Config::write_template(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("command = [\"pnpm\", \"test\"]"));
    }

    #[test]
    fn write_template_polyglot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        let path = dir.path().join("togi.toml");
        Config::write_template(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        // pytest wins as primary, so JS gets a language section
        assert!(content.contains("[test.languages.typescript]"));
    }

    #[test]
    fn write_template_routes_all_supported_markers() {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "Cargo.toml",
            "go.mod",
            "pyproject.toml",
            "package.json",
            "pnpm-lock.yaml",
            "pom.xml",
            "Gemfile",
            "CMakeLists.txt",
            "example.csproj",
        ] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        let path = dir.path().join("togi.toml");
        Config::write_template(&path).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        let config: Config = toml::from_str(&content).unwrap();

        let primary: Vec<String> = vec!["cargo".to_owned(), "test".to_owned()];
        assert_eq!(config.test.command, primary);
        for (language, expected) in [
            ("go", vec!["go", "test", "./..."]),
            ("python", vec!["pytest"]),
            ("typescript", vec!["pnpm", "test"]),
            ("java", vec!["mvn", "test"]),
            ("ruby", vec!["bundle", "exec", "rspec"]),
            ("c", vec!["ctest"]),
            ("cpp", vec!["ctest"]),
            ("c_sharp", vec!["dotnet", "test"]),
        ] {
            let expected: Vec<String> = expected.into_iter().map(str::to_owned).collect();
            assert_eq!(
                config.test.languages[language].command, expected,
                "unexpected command for {language}"
            );
        }

        let mut previous = 0;
        for language in [
            "go",
            "python",
            "typescript",
            "java",
            "ruby",
            "c",
            "cpp",
            "c_sharp",
        ] {
            let section = format!("[test.languages.{language}]");
            let position = content.find(&section).unwrap();
            assert!(position > previous, "{section} is out of order");
            previous = position;
        }
    }

    #[test]
    fn write_template_rejects_conflicting_java_routes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pom.xml"), "").unwrap();
        std::fs::write(dir.path().join("build.gradle"), "").unwrap();
        let path = dir.path().join("togi.toml");

        let err = Config::write_template(&path).unwrap_err();
        assert!(err.to_string().contains("pom.xml"));
        assert!(err.to_string().contains("build.gradle"));
        assert!(!path.exists());
        assert!(err.to_string().contains("[projects.*.test]"));
    }

    #[test]
    fn write_template_escapes_quoted_origin_head_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["init"]);
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "initial"]);
        run(&["update-ref", "refs/remotes/origin/trunk\"quoted", "HEAD"]);
        run(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk\"quoted",
        ]);

        let path = root.join("togi.toml");
        Config::write_template(&path).unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        let config: Config = toml::from_str(&content).unwrap();
        assert_eq!(config.diff.base, "origin/trunk\"quoted");
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
    fn detect_pnpm() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "").unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["pnpm", "test"]);
    }

    #[test]
    fn detect_yarn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "").unwrap();
        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["yarn", "test"]);
    }

    #[test]
    fn detect_bun() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "").unwrap();
        std::fs::write(dir.path().join("bun.lockb"), "").unwrap();
        assert_eq!(detect_test_command(dir.path()), vec!["bun", "test"]);
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
    fn parse_coverage_collection_options() {
        let toml_str = r#"
[mutations]
coverage = "auto"
coverage_command = ["./scripts/collect-coverage.sh"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mutations.coverage, Some(CoverageMode::Auto));
        assert_eq!(
            config.mutations.coverage_command,
            vec!["./scripts/collect-coverage.sh"]
        );
    }

    #[test]
    fn parse_coverage_gate_options() {
        let toml_str = r#"
[mutations]
min_line_coverage = 80.0
min_diff_coverage = 90.0
fail_on_uncovered_diff = true
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mutations.min_line_coverage, Some(80.0));
        assert_eq!(config.mutations.min_diff_coverage, Some(90.0));
        assert!(config.mutations.fail_on_uncovered_diff);
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
    fn ambiguous_detection_names_actual_manifests() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("setup.cfg"), "").unwrap();
        std::fs::write(dir.path().join("build.gradle.kts"), "").unwrap();
        let mut config = Config::default();

        let TestCommandResolution::Ambiguous(ambiguity) = config.resolve_test_command(dir.path())
        else {
            panic!("expected ambiguous test command detection");
        };

        assert!(config.test.command.is_empty());

        assert_eq!(
            ambiguity.error().to_string(),
            "multiple test runtimes detected (setup.cfg, build.gradle.kts); set [test] command in togi.toml or pass --test-cmd <command>"
        );
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
    fn detect_builtin_go_coverage_adapter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "").unwrap();
        assert_eq!(
            detect_builtin_coverage_adapter(dir.path()),
            Some(BuiltinCoverageAdapter::Go)
        );
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
    fn parse_per_language_timeout() {
        let toml_str = r#"
[test]
command = ["cargo", "test"]

[test.languages.java]
command = ["mvn", "test"]
timeout = 120
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.test.languages["java"].timeout, Some(120));
    }

    #[test]
    fn parse_per_language_timeout_absent() {
        let toml_str = r#"
[test]
command = ["cargo", "test"]

[test.languages.go]
command = ["go", "test", "./..."]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.test.languages["go"].timeout, None);
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
respect_workspace_ignores = false
incremental_history = false
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.mutations.exclude_paths,
            vec!["vendor/**", "*.generated.ts"]
        );
        assert!(!config.mutations.skip_noisy_files);
        assert!(!config.mutations.respect_workspace_ignores);
        assert!(!config.mutations.incremental_history);
    }

    #[test]
    fn parse_max_per_file() {
        let toml_str = r#"
[mutations]
max_per_file = 50
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mutations.max_per_file, 50);
    }

    #[test]
    fn max_per_file_zero_means_unlimited() {
        let toml_str = r#"
[mutations]
max_per_file = 0
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mutations.max_per_file, 0);
    }
}
