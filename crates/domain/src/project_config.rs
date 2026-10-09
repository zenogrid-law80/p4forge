//! DB-editable project configurations awaiting runner/source capabilities.
use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{MAX_CONFIG_BYTES, validate_name, validate_relative_directory};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfiguration {
    pub schema_version: u32,
    pub pipelines: Vec<PipelineConfiguration>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfiguration {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root_id: Option<uuid::Uuid>,
    pub category: String,
    pub runner_os: String,
    pub runner_arch: String,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_view: Option<String>,
    pub changes: Vec<String>,
    pub working_directory: String,
    pub stages: Vec<String>,
    /// Jobs in this configuration share a workspace; the current isolated-job
    /// executor must not silently interpret this as an executable definition.
    pub workspace: String,
    pub max_parallel_jobs: usize,
    pub jobs: Vec<ConfigurationJob>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationJob {
    pub name: String,
    pub stage: String,
    #[serde(default)]
    pub needs: Vec<String>,
    pub script: Vec<String>,
    pub timeout_seconds: u64,
}

impl ProjectConfiguration {
    pub fn parse(source: &str) -> Result<Self> {
        ensure!(
            source.len() <= MAX_CONFIG_BYTES,
            "configuration exceeds 256 KiB"
        );
        let value: Self = toml::from_str(source)?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported project schema_version"
        );
        ensure!(
            (1..=32).contains(&self.pipelines.len()),
            "expected 1..32 pipelines"
        );
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_CONFIG_BYTES,
            "configuration exceeds 256 KiB"
        );
        let mut names = BTreeSet::new();
        for pipeline in &self.pipelines {
            pipeline.validate()?;
            ensure!(
                names.insert(pipeline.name.clone()),
                "duplicate pipeline name"
            );
        }
        validate_dependencies(self.pipelines.iter().map(|p| (&p.name, &p.needs)))?;
        Ok(())
    }
}

impl PipelineConfiguration {
    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(&ProjectConfiguration {
            schema_version: 1,
            pipelines: vec![self.clone()],
        })?)
    }

    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        ensure!(
            self.source_root_id.is_none_or(|id| !id.is_nil()),
            "source_root_id must not be nil"
        );
        validate_name(&self.category)?;
        ensure!(
            matches!(self.runner_os.as_str(), "linux" | "windows" | "macos"),
            "unknown runner_os"
        );
        ensure!(
            matches!(self.runner_arch.as_str(), "x86_64" | "aarch64"),
            "unknown runner_arch"
        );
        ensure!(self.workspace == "pipeline", "expected pipeline workspace");
        ensure!(
            (1..=32).contains(&self.max_parallel_jobs),
            "max_parallel_jobs must be 1..32"
        );
        validate_relative_directory(&self.working_directory)?;
        if let Some(view) = &self.checkout_view {
            validate_name(view)?;
        }
        ensure!(
            (1..=256).contains(&self.changes.len()),
            "expected 1..256 change paths"
        );
        for pattern in &self.changes {
            let path = pattern.strip_suffix("/**").unwrap_or(pattern);
            validate_relative_directory(path)?;
            ensure!(
                !path.contains(['*', '?', '[', ']']) && !path.chars().any(char::is_control),
                "changes require exact paths or directory/**"
            );
        }
        ensure!(
            (1..=64).contains(&self.stages.len()),
            "expected 1..64 stages"
        );
        let mut stages = BTreeSet::new();
        for stage in &self.stages {
            validate_name(stage)?;
            ensure!(stages.insert(stage), "duplicate stage");
        }
        ensure!((1..=64).contains(&self.jobs.len()), "expected 1..64 jobs");
        let mut names = BTreeSet::new();
        for job in &self.jobs {
            validate_name(&job.name)?;
            ensure!(names.insert(&job.name), "duplicate job");
            ensure!(stages.contains(&job.stage), "job has unknown stage");
            ensure!(
                (1..=86400).contains(&job.timeout_seconds),
                "invalid timeout"
            );
            ensure!(
                (1..=100).contains(&job.script.len()),
                "expected 1..100 commands"
            );
            ensure!(
                job.script
                    .iter()
                    .all(|s| !s.trim().is_empty() && !s.contains('\0')),
                "empty or invalid script"
            );
        }
        validate_dependencies(self.jobs.iter().map(|j| (&j.name, &j.needs)))?;
        for job in &self.jobs {
            let stage = self.stages.iter().position(|s| s == &job.stage).unwrap();
            for need in &job.needs {
                let upstream = self.jobs.iter().find(|j| &j.name == need).unwrap();
                ensure!(
                    self.stages
                        .iter()
                        .position(|s| s == &upstream.stage)
                        .unwrap()
                        <= stage,
                    "job depends on a later stage"
                );
            }
        }
        Ok(())
    }

    pub fn readiness(&self) -> Vec<String> {
        let mut result = Vec::new();
        if self.source_root_id.is_none() {
            result.push("이 소스가 제출된 Perforce stream/depot 연결 필요".into());
        }
        result.push("프로젝트 구성을 실행 버전으로 변환하는 기능 필요".into());
        if self.max_parallel_jobs != 1 {
            result.push("현재 공용 workspace 실행은 max_parallel_jobs = 1 필요".into());
        }
        if !self.needs.is_empty() {
            result.push("파이프라인 간 의존성과 생성 파일 제출 후 소스 revision 전달 필요".into());
        }
        if self.checkout_view.is_some() {
            result.push("checkout_view를 실제 Perforce client view에 연결 필요".into());
        }
        let commands = self
            .jobs
            .iter()
            .flat_map(|j| &j.script)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if commands.contains("p4-publish") {
            result.push("Perforce 게시 도구 및 생성 경로에 한정된 쓰기 계정 필요".into());
        }
        if commands.contains("docker ") {
            result.push("전용 빌드 호스트의 Docker/BuildKit 환경 필요".into());
        }
        if commands.contains("ECR_") {
            result.push("ECR 게시용 자격 증명 연결 필요".into());
        }
        result.push("changes 필터는 저장되며 자동 실행 트리거는 미구현".into());
        result
    }
}

fn validate_dependencies<'a>(
    nodes: impl Iterator<Item = (&'a String, &'a Vec<String>)>,
) -> Result<()> {
    let nodes = nodes.collect::<Vec<_>>();
    let names = nodes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    for (name, needs) in &nodes {
        let mut seen = BTreeSet::new();
        for need in *needs {
            ensure!(
                need != *name && names.contains(need.as_str()) && seen.insert(need),
                "unknown, duplicate or self dependency: {need}"
            );
        }
    }
    let mut done = BTreeSet::new();
    while done.len() < nodes.len() {
        let next = nodes.iter().find(|(name, needs)| {
            !done.contains(name.as_str()) && needs.iter().all(|n| done.contains(n.as_str()))
        });
        let Some((name, _)) = next else {
            anyhow::bail!("cyclic dependencies")
        };
        done.insert(name.as_str());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const ROOT: &str = include_str!("../../../examples/root.p4forge.toml");

    #[test]
    fn root_configuration_preserves_all_workflows_and_p4_conversion() {
        let config = ProjectConfiguration::parse(ROOT).unwrap();
        assert_eq!(config.pipelines.len(), 7);
        assert_eq!(
            config.pipelines.iter().map(|p| p.jobs.len()).sum::<usize>(),
            12
        );
        let server = config
            .pipelines
            .iter()
            .find(|p| p.name == "server")
            .unwrap();
        assert_eq!(server.needs, ["data-table-generate"]);
        assert_eq!(server.stages, ["check", "test", "build", "publish"]);
        assert_eq!(server.checkout_view.as_deref(), Some("ServerBuildView"));
        assert!(!ROOT.contains("LORE_") && !ROOT.contains(".lore-ci.toml"));
        assert!(ROOT.contains("P4FORGE_CHANGELIST") && ROOT.contains("p4-publish.py"));
        assert_eq!(
            ProjectConfiguration::parse(&toml::to_string_pretty(&config).unwrap()).unwrap(),
            config
        );
    }

    #[test]
    fn rejects_lost_fields_and_invalid_dependencies_or_paths() {
        assert!(
            ProjectConfiguration::parse(
                &ROOT.replace("schema_version = 1", "schema_version = 1\nunknown = true")
            )
            .is_err()
        );
        let mut config = ProjectConfiguration::parse(ROOT).unwrap();
        config.pipelines[0].needs = vec!["server".into()];
        assert!(config.validate().is_err());
        config.pipelines[0].needs.clear();
        config.pipelines[0].changes = vec!["../outside/**".into()];
        assert!(config.validate().is_err());
    }
}
