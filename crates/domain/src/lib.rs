use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub mod project_config;

pub const MAX_CONFIG_BYTES: usize = 256 * 1024;

/// Machine-readable location without changing the validator's public error message.
#[derive(Debug, Clone, Serialize)]
pub struct DefinitionDiagnostic {
    pub field: String,
    pub job_index: Option<usize>,
    pub message: String,
}
impl std::fmt::Display for DefinitionDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for DefinitionDiagnostic {}
fn located(error: impl std::fmt::Display, field: &str, job_index: Option<usize>) -> anyhow::Error {
    DefinitionDiagnostic {
        field: field.into(),
        job_index,
        message: error.to_string(),
    }
    .into()
}
macro_rules! ensure_at {
    ($field:expr, $job:expr, $condition:expr, $($message:tt)*) => {
        if !$condition { return Err(located(format!($($message)*), $field, $job)); }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineDefinition {
    pub schema_version: u32,
    pub source_root_id: Uuid,
    #[serde(default = "default_parallel")]
    pub max_parallel_jobs: usize,
    #[serde(default, skip_serializing_if = "is_job_workspace")]
    pub workspace: Workspace,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_generated: Option<GeneratedPublication>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_success: Vec<PipelineTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<Deployment>,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    pub environment_id: Uuid,
    pub health_check: String,
}

pub const MAX_ARTIFACT_FILE: u64 = 64 * 1024 * 1024;
pub const MAX_ARTIFACT_TOTAL: u64 = 256 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSpec {
    pub name: String,
    pub path: String,
}
impl ArtifactSpec {
    pub fn validate(&self) -> Result<()> {
        let stem = self
            .name
            .split('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        ensure!(
            !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit()),
            "reserved Windows device filename"
        );
        ensure!(
            !self.name.is_empty()
                && self.name.len() <= 120
                && !self.name.starts_with('.')
                && !self.name.ends_with('.')
                && self
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')),
            "artifact name must be a plain ASCII filename"
        );
        ensure!(
            self.path.len() <= 512
                && self.path.split('/').all(|p| !p.is_empty()
                    && p != "."
                    && p != ".."
                    && !p.ends_with('.')
                    && !p.ends_with(' ')
                    && !p
                        .chars()
                        .any(|c| c.is_control() || "\\:*?\"<>|".contains(c))),
            "artifact path must name a file inside the checkout without wildcards"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedPublication {
    pub user: String,
    /// Explicit checkout-relative directories, including their contents.
    pub paths: Vec<String>,
}
impl GeneratedPublication {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.user)?;
        ensure!(
            !self.paths.is_empty() && self.paths.len() <= 16,
            "publication needs 1..16 paths"
        );
        for path in &self.paths {
            ensure!(
                path.len() <= 512
                    && path.split('/').all(|part| !part.is_empty()
                        && part != "."
                        && part != ".."
                        && !part.contains("...")
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))),
                "publication paths must be plain relative directories"
            );
            ensure!(
                self.paths
                    .iter()
                    .filter(|other| *other == path || path.starts_with(&format!("{other}/")))
                    .count()
                    == 1,
                "publication paths overlap"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineTarget {
    pub pipeline_id: Uuid,
    pub version: i64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    #[default]
    Job,
    Pipeline,
}
fn is_job_workspace(value: &Workspace) -> bool {
    *value == Workspace::Job
}

fn default_parallel() -> usize {
    4
}
fn default_timeout() -> u64 {
    3600
}
fn default_directory() -> String {
    ".".into()
}
fn default_arch() -> String {
    "x86_64".into()
}
fn default_pool() -> String {
    "default".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub name: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub needs: Vec<String>,
    pub runner: RunnerRequirements,
    pub shell: Shell,
    pub script: Vec<String>,
    #[serde(default = "default_directory")]
    pub working_directory: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerRequirements {
    pub os: OperatingSystem,
    #[serde(default = "default_arch")]
    pub arch: String,
    #[serde(default = "default_pool")]
    pub pool: String,
    /// Require a recently verified Docker engine of this type on the runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<DockerEngine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerEngine {
    Linux,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    Linux,
    Windows,
    Macos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shell {
    Sh,
    PowerShell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidatedGraph {
    pub order: Vec<String>,
    pub edges: Vec<(String, String)>,
}

impl PipelineDefinition {
    pub fn parse(source: &str) -> Result<Self> {
        ensure!(
            source.len() <= MAX_CONFIG_BYTES,
            "configuration exceeds 256 KiB"
        );
        let definition: Self = toml::from_str(source).map_err(|error: toml::de::Error| {
            anyhow::anyhow!(
                "invalid TOML at byte {}: {}",
                error.span().map_or(0, |span| span.start),
                error.message()
            )
        })?;
        definition.validate()?;
        Ok(definition)
    }

    pub fn validate(&self) -> Result<ValidatedGraph> {
        if let Some(deployment) = &self.deployment {
            ensure_at!(
                "deployment",
                None,
                !deployment.environment_id.is_nil(),
                "deployment environment is required"
            );
            ensure_at!(
                "deployment",
                None,
                self.workspace == Workspace::Pipeline && !self.artifact_inputs.is_empty(),
                "deployment requires a pipeline workspace and fixed artifact inputs"
            );
            ensure_at!(
                "deployment",
                None,
                self.publish_generated.is_none() && self.on_success.is_empty(),
                "deployment cannot publish Perforce changes or start followups"
            );
            let health = self
                .jobs
                .iter()
                .find(|j| j.name == deployment.health_check)
                .ok_or_else(|| {
                    located(
                        "deployment health_check must name a job",
                        "deployment",
                        None,
                    )
                })?;
            ensure_at!(
                "deployment",
                None,
                self.stages.len() >= 2
                    && self.jobs.len() >= 2
                    && self.stages.last() == Some(&health.stage)
                    && self.jobs.iter().filter(|j| j.stage == health.stage).count() == 1,
                "health_check must be the only job in the final stage after deployment"
            );
        }
        ensure_at!(
            "artifact_inputs",
            None,
            self.artifact_inputs.len() <= 16,
            "at most 16 artifact inputs per run"
        );
        let mut input_names = BTreeSet::new();
        for name in &self.artifact_inputs {
            ArtifactSpec {
                name: name.clone(),
                path: name.clone(),
            }
            .validate()
            .map_err(|e| located(e, "artifact_inputs", None))?;
            ensure_at!(
                "artifact_inputs",
                None,
                self.workspace == Workspace::Pipeline,
                "artifact inputs require one pipeline workspace"
            );
            ensure_at!(
                "artifact_inputs",
                None,
                input_names.insert(name.to_ascii_lowercase()),
                "duplicate artifact input filename"
            );
        }
        ensure_at!(
            "artifacts",
            None,
            self.artifacts.len() <= 16,
            "at most 16 artifacts per run"
        );
        let mut artifact_names = BTreeSet::new();
        for artifact in &self.artifacts {
            artifact
                .validate()
                .map_err(|e| located(e, "artifacts", None))?;
            ensure_at!(
                "artifacts",
                None,
                self.workspace == Workspace::Pipeline,
                "artifacts require one pipeline workspace"
            );
            ensure_at!(
                "artifacts",
                None,
                artifact_names.insert(artifact.name.to_ascii_lowercase()),
                "duplicate artifact filename"
            );
        }
        if let Some(publication) = &self.publish_generated {
            publication
                .validate()
                .map_err(|e| located(e, "publish_generated", None))?;
            ensure_at!(
                "publish_generated",
                None,
                self.workspace == Workspace::Pipeline,
                "publication requires one pipeline workspace"
            );
            ensure_at!(
                "publish_generated",
                None,
                self.jobs
                    .iter()
                    .all(|job| job.runner.os != OperatingSystem::Windows),
                "native publication currently supports Linux and macOS"
            );
        }
        ensure_at!(
            "on_success",
            None,
            self.on_success.len() <= 16,
            "at most 16 downstream pipelines"
        );
        let mut targets = BTreeSet::new();
        for target in &self.on_success {
            ensure_at!(
                "on_success",
                None,
                !target.pipeline_id.is_nil()
                    && target.version > 0
                    && targets.insert(target.pipeline_id),
                "downstream pipelines need unique IDs and fixed positive versions"
            );
        }
        ensure_at!(
            "schema_version",
            None,
            self.schema_version == 1,
            "unsupported schema_version; expected 1"
        );
        ensure_at!(
            "source_root_id",
            None,
            !self.source_root_id.is_nil(),
            "source_root_id must not be nil"
        );
        ensure_at!(
            "max_parallel_jobs",
            None,
            (1..=32).contains(&self.max_parallel_jobs),
            "max_parallel_jobs must be 1..32"
        );
        ensure_at!(
            "jobs",
            None,
            (1..=64).contains(&self.jobs.len()),
            "a pipeline must have 1..64 jobs"
        );
        ensure_at!(
            "jobs",
            None,
            serde_json::to_vec(self)?.len() <= MAX_CONFIG_BYTES,
            "configuration exceeds 256 KiB"
        );
        let mut names = BTreeSet::new();
        for (job_index, job) in self.jobs.iter().enumerate() {
            validate_name(&job.name).map_err(|e| located(e, "name", Some(job_index)))?;
            ensure_at!(
                "name",
                Some(job_index),
                names.insert(job.name.as_str()),
                "duplicate job: {}",
                job.name
            );
            ensure_at!(
                "stage",
                Some(job_index),
                job.stage.len() <= 80 && !job.stage.chars().any(char::is_control),
                "invalid stage"
            );
            ensure_at!(
                "timeout_seconds",
                Some(job_index),
                (1..=86400).contains(&job.timeout_seconds),
                "timeout_seconds must be 1..86400"
            );
            ensure_at!(
                "script",
                Some(job_index),
                (1..=100).contains(&job.script.len()),
                "job {} needs 1..100 commands",
                job.name
            );
            ensure_at!(
                "script",
                Some(job_index),
                job.script
                    .iter()
                    .all(|s| !s.trim().is_empty() && !s.contains('\0')),
                "empty or invalid script in {}",
                job.name
            );
            validate_relative_directory(&job.working_directory)
                .map_err(|e| located(e, "working_directory", Some(job_index)))?;
            validate_name(&job.runner.pool)
                .map_err(|e| located(e, "runner.pool", Some(job_index)))?;
            ensure_at!(
                "runner.arch",
                Some(job_index),
                matches!(job.runner.arch.as_str(), "x86_64" | "aarch64"),
                "unsupported runner architecture"
            );
            ensure_at!(
                "shell",
                Some(job_index),
                (job.runner.os == OperatingSystem::Windows) == (job.shell == Shell::PowerShell),
                "Windows jobs require power_shell; Linux jobs require sh"
            );
            ensure_at!(
                "environment",
                Some(job_index),
                job.environment.len() <= 64,
                "too many environment variables"
            );
            for (key, value) in &job.environment {
                let reserved = key.to_ascii_uppercase();
                ensure_at!(
                    "environment",
                    Some(job_index),
                    valid_env_key(key)
                        && !reserved.starts_with("P4")
                        && !matches!(
                            reserved.as_str(),
                            "PATH"
                                | "HOME"
                                | "LD_PRELOAD"
                                | "BASH_ENV"
                                | "ENV"
                                | "SYSTEMROOT"
                                | "USERPROFILE"
                                | "APPDATA"
                                | "LOCALAPPDATA"
                                | "COMSPEC"
                                | "TEMP"
                                | "TMP"
                                | "TMPDIR"
                        ),
                    "reserved or invalid environment key: {key}"
                );
                ensure_at!(
                    "environment",
                    Some(job_index),
                    value.len() <= 4096 && !value.contains('\0'),
                    "invalid environment value"
                );
            }
        }
        let mut edges = Vec::new();
        let mut dependencies = self
            .jobs
            .iter()
            .map(|job| (job.name.clone(), job.needs.clone()))
            .collect::<BTreeMap<_, _>>();
        if self.workspace == Workspace::Pipeline {
            ensure_at!(
                "max_parallel_jobs",
                None,
                self.max_parallel_jobs == 1,
                "shared workspace requires max_parallel_jobs = 1"
            );
            ensure_at!(
                "stages",
                None,
                !self.stages.is_empty() && self.stages.len() <= 64,
                "shared workspace requires 1..64 ordered stages"
            );
            let mut stages = BTreeSet::new();
            for stage in &self.stages {
                validate_name(stage).map_err(|e| located(e, "stages", None))?;
                ensure_at!("stages", None, stages.insert(stage), "duplicate stage");
            }
            let runner = &self.jobs[0].runner;
            for (job_index, job) in self.jobs.iter().enumerate() {
                ensure_at!(
                    "runner",
                    Some(job_index),
                    &job.runner == runner,
                    "shared workspace jobs require the same runner"
                );
                let index = self
                    .stages
                    .iter()
                    .position(|stage| stage == &job.stage)
                    .ok_or_else(|| located("job has unknown stage", "stage", Some(job_index)))?;
                for (earlier_job_index, earlier) in self.jobs.iter().enumerate() {
                    let earlier_index = self
                        .stages
                        .iter()
                        .position(|stage| stage == &earlier.stage)
                        .ok_or_else(|| {
                            located("job has unknown stage", "stage", Some(earlier_job_index))
                        })?;
                    if earlier_index < index && !dependencies[&job.name].contains(&earlier.name) {
                        dependencies
                            .get_mut(&job.name)
                            .unwrap()
                            .push(earlier.name.clone());
                    }
                }
            }
        } else {
            ensure_at!(
                "stages",
                None,
                self.stages.is_empty(),
                "ordered stages require pipeline workspace"
            );
        }
        for (job_index, job) in self.jobs.iter().enumerate() {
            let mut unique = BTreeSet::new();
            for dependency in &dependencies[&job.name] {
                ensure_at!(
                    "needs",
                    Some(job_index),
                    dependency != &job.name,
                    "job {} depends on itself",
                    job.name
                );
                ensure_at!(
                    "needs",
                    Some(job_index),
                    names.contains(dependency.as_str()),
                    "unknown dependency: {dependency}"
                );
                ensure_at!(
                    "needs",
                    Some(job_index),
                    unique.insert(dependency),
                    "duplicate dependency in {}",
                    job.name
                );
                edges.push((dependency.clone(), job.name.clone()));
            }
        }
        let mut emitted = BTreeSet::new();
        let mut order = Vec::new();
        while emitted.len() < self.jobs.len() {
            let Some(job) = self.jobs.iter().find(|job| {
                !emitted.contains(&job.name)
                    && dependencies[&job.name]
                        .iter()
                        .all(|name| emitted.contains(name))
            }) else {
                return Err(located("cyclic job dependencies", "needs", None));
            };
            emitted.insert(job.name.clone());
            order.push(job.name.clone());
        }
        Ok(ValidatedGraph { order, edges })
    }

    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        Ok(hash_bytes(&serde_json::to_vec(self)?))
    }

    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        toml::to_string_pretty(self).context("serialize TOML")
    }
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn validate_name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 80
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "names must use 1..80 letters, digits, hyphens or underscores"
    );
    Ok(())
}

pub fn validate_relative_directory(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 512
            && !value.starts_with('/')
            && !value.contains(['\\', ':', '\0'])
            && !value.split('/').any(|part| part == ".."),
        "working_directory must stay inside the checkout"
    );
    Ok(())
}

fn valid_env_key(value: &str) -> bool {
    value.len() <= 80
        && value
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> PipelineDefinition {
        PipelineDefinition::parse(include_str!("../../../examples/ci.toml")).unwrap()
    }

    #[test]
    fn accepts_dag_and_toml_roundtrip() {
        let input = definition();
        let graph = input.validate().unwrap();
        assert_eq!(graph.order, ["build", "test"]);
        assert_eq!(
            PipelineDefinition::parse(&input.to_toml().unwrap()).unwrap(),
            input
        );
        assert_eq!(input.hash().unwrap().len(), 64);
    }

    #[test]
    fn diagnostics_locate_existing_validation_failures_without_changing_messages() {
        let base = definition();
        let check =
            |value: PipelineDefinition, field: &str, index: Option<usize>, message: &str| {
                let error = value.validate().unwrap_err();
                let diagnostic = error.downcast_ref::<DefinitionDiagnostic>().unwrap();
                assert_eq!(diagnostic.field, field);
                assert_eq!(diagnostic.job_index, index);
                assert!(error.to_string().contains(message), "{error}");
                assert!(
                    value.to_toml().is_err(),
                    "invalid definitions must remain unsavable"
                );
            };
        let mut value = base.clone();
        value.jobs[1].timeout_seconds = 0;
        check(value, "timeout_seconds", Some(1), "timeout_seconds must");
        let mut value = base.clone();
        value.jobs[1].name = value.jobs[0].name.clone();
        check(value, "name", Some(1), "duplicate job");
        let mut value = base.clone();
        value.jobs[1].needs = vec!["missing".into()];
        check(value, "needs", Some(1), "unknown dependency");
        let mut value = base.clone();
        value.jobs[0].needs = vec!["test".into()];
        check(value, "needs", None, "cyclic");
        let mut value = base.clone();
        value.jobs[0].working_directory = "../outside".into();
        check(value, "working_directory", Some(0), "inside the checkout");
        let mut value = base.clone();
        value.workspace = Workspace::Pipeline;
        value.max_parallel_jobs = 1;
        value.stages = vec!["build".into(), "test".into()];
        value.jobs[1].stage = "unknown".into();
        check(value, "stage", Some(1), "unknown stage");
    }

    #[test]
    fn rejects_cycles_unknown_duplicate_and_self_dependencies() {
        for needs in [
            vec!["test".into()],
            vec!["missing".into()],
            vec!["build".into()],
        ] {
            let mut input = definition();
            input.jobs[0].needs = needs;
            assert!(input.validate().is_err());
        }
        let mut input = definition();
        input.jobs[1].needs.push("build".into());
        assert!(input.validate().is_err());
    }

    #[test]
    fn rejects_unsafe_paths_versions_and_unknown_fields() {
        for directory in [
            "../out",
            "build/../../out",
            "C:\\temp",
            "/tmp",
            "\\server\\path",
        ] {
            let mut input = definition();
            input.jobs[0].working_directory = directory.into();
            assert!(input.validate().is_err());
        }
        let mut input = definition();
        input.schema_version = 2;
        assert!(input.validate().is_err());
        assert!(
            PipelineDefinition::parse(&format!(
                "unexpected = true\n{}",
                include_str!("../../../examples/ci.toml")
            ))
            .is_err()
        );
    }

    #[test]
    fn stages_do_not_add_hidden_dependencies() {
        let mut input = definition();
        input.jobs[0].stage = "late".into();
        input.jobs[1].stage = "early".into();
        assert_eq!(
            input.validate().unwrap().edges,
            [("build".into(), "test".into())]
        );
    }

    #[test]
    fn shared_workspace_stages_order_jobs_and_reject_unsafe_scheduling() {
        let mut input = definition();
        input.workspace = Workspace::Pipeline;
        input.max_parallel_jobs = 1;
        input.stages = vec!["generate".into(), "publish".into()];
        input.jobs[0].stage = "generate".into();
        input.jobs[1].stage = "publish".into();
        input.jobs[1].needs.clear();
        input.jobs.swap(0, 1);
        assert_eq!(input.validate().unwrap().order, ["build", "test"]);
        assert_eq!(
            input.validate().unwrap().edges,
            [("build".into(), "test".into())]
        );
        input.jobs[1].needs.push("test".into());
        assert!(input.validate().is_err());
        input.jobs[1].needs.clear();
        input.max_parallel_jobs = 2;
        assert!(input.validate().is_err());
        input.max_parallel_jobs = 1;
        input.jobs[1].runner.pool = "other".into();
        assert!(input.validate().is_err());
        let mut input = definition();
        input.jobs[0]
            .environment
            .insert("p4tickets".into(), "secret".into());
        assert!(input.validate().is_err());
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn publication_paths_cannot_escape_overlap_or_expand_the_checkout() {
        for paths in [
            vec!["../etc"],
            vec!["a/..."],
            vec!["a", "a/b"],
            vec!["a", "a"],
            vec!["/root"],
            vec!["a@head"],
            vec!["a//b"],
        ] {
            assert!(
                GeneratedPublication {
                    user: "publisher".into(),
                    paths: paths.into_iter().map(str::to_owned).collect()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            GeneratedPublication {
                user: "publisher".into(),
                paths: vec!["GameDesign/Output/Sqlite".into()]
            }
            .validate()
            .is_ok()
        );
    }
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    #[test]
    fn rejects_ambiguous_paths_and_download_names() {
        for path in [
            "",
            "/etc/passwd",
            "../secret",
            "a/../b",
            "a//b",
            "a\\b",
            "file:stream",
            "*.exe",
            "a./b",
            "a /b",
        ] {
            assert!(
                ArtifactSpec {
                    name: "output.exe".into(),
                    path: path.into()
                }
                .validate()
                .is_err(),
                "{path}"
            );
        }
        for name in ["", "../file", "a\"b", "file\r\n", ".env", "a."] {
            assert!(
                ArtifactSpec {
                    name: name.into(),
                    path: "bin/app.exe".into()
                }
                .validate()
                .is_err(),
                "{name}"
            );
        }
        ArtifactSpec {
            name: "app.exe".into(),
            path: "한글/bin/app.exe".into(),
        }
        .validate()
        .unwrap();
    }
}

#[cfg(test)]
mod deployment_tests {
    use super::*;
    #[test]
    fn deployment_requires_frozen_inputs_and_a_final_health_stage() {
        let mut d = PipelineDefinition::parse(include_str!("../../../examples/ci.toml")).unwrap();
        d.deployment = Some(Deployment {
            environment_id: Uuid::new_v4(),
            health_check: "test".into(),
        });
        assert!(d.validate().is_err());
        d.workspace = Workspace::Pipeline;
        d.max_parallel_jobs = 1;
        d.artifact_inputs = vec!["output.bin".into()];
        d.stages = vec!["deploy".into(), "health".into()];
        d.jobs[0].stage = "deploy".into();
        d.jobs[1].stage = "health".into();
        assert!(d.validate().is_ok());
        assert_eq!(PipelineDefinition::parse(&d.to_toml().unwrap()).unwrap(), d);
        d.deployment.as_mut().unwrap().health_check = "build".into();
        assert!(d.validate().is_err());
        d.deployment.as_mut().unwrap().health_check = "missing".into();
        assert!(d.validate().is_err());
        d.deployment.as_mut().unwrap().health_check = "test".into();
        d.jobs[0].stage = "health".into();
        assert!(d.validate().is_err());
        d.jobs[0].stage = "deploy".into();
        d.on_success.push(PipelineTarget {
            pipeline_id: Uuid::new_v4(),
            version: 1,
        });
        assert!(d.validate().is_err());
    }
}
