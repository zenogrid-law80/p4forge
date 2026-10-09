use p4forge_domain::{ArtifactSpec, GeneratedPublication, Job};
use p4forge_perforce::SourceSnapshot;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const LEASE_SECONDS: u64 = 60;
pub const HEARTBEAT_SECONDS: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerState {
    Available,
    CliMissing,
    EngineUnavailable,
    CheckFailed,
}
impl DockerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::CliMissing => "cli_missing",
            Self::EngineUnavailable => "engine_unavailable",
            Self::CheckFailed => "check_failed",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerEngineOs {
    Linux,
    Windows,
}
impl DockerEngineOs {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerCapability {
    pub state: DockerState,
    pub engine_os: Option<DockerEngineOs>,
}
impl DockerCapability {
    pub fn valid(&self) -> bool {
        (self.state == DockerState::Available) == self.engine_os.is_some()
    }
}
// Secret-bearing messages intentionally do not implement Debug.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketGrant {
    pub id: Uuid,
    pub generation: Uuid,
    pub role: String,
    pub connection: p4forge_perforce::Connection,
    pub server_id: String,
    pub tickets: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketHealth {
    pub id: Uuid,
    pub generation: Uuid,
    pub ready: bool,
    pub remaining_seconds: u64,
}
pub const MAX_LOG_CHUNK: usize = 16 * 1024;
pub const MAX_LOG_BYTES: i64 = 4 * 1024 * 1024;

pub const MAX_AGENT_BINARY: usize = 64 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateOffer {
    pub id: Uuid,
    pub release_id: Uuid,
    pub version: String,
    pub sha256: String,
    pub size: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateReceipt {
    pub id: Uuid,
    pub status: String,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePoll {
    pub sha256: String,
    pub version: String,
    pub ready: bool,
    pub pending_id: Option<Uuid>,
    pub receipt: Option<UpdateReceipt>,
}
#[derive(Serialize, Deserialize)]
pub struct UpdateResponse {
    pub offer: Option<UpdateOffer>,
    pub healthy_id: Option<Uuid>,
    pub receipt_accepted: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    pub request_id: Uuid,
    pub os: String,
    pub arch: String,
    #[serde(default)]
    pub protocol_version: u32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ClaimResponse {
    pub assignment: Option<Assignment>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub attempt_id: Uuid,
    pub job_id: Uuid,
    pub run_id: Uuid,
    pub fence: i64,
    pub lease_seconds: u64,
    pub job: Job,
    pub source: SourceSnapshot,
    /// Ordered jobs reserved together on one agent and one checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_jobs: Vec<AssignedJob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_generated: Option<GeneratedPublication>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inputs: Vec<ArtifactInput>,
}

pub const ARTIFACT_CHUNK: usize = 256 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub fence: i64,
    pub files: Vec<ArtifactEntry>,
}
#[derive(Serialize, Deserialize)]
pub struct ArtifactUpload {
    pub id: Uuid,
    pub name: String,
    pub uploaded_size: i64,
    pub status: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactChunk {
    pub fence: i64,
    pub offset: u64,
    pub data: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRequest {
    pub fence: i64,
    pub status: String,
    pub changelist: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct AssignedJob {
    pub id: Uuid,
    pub job: Job,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepRequest {
    pub fence: i64,
    pub job_id: Uuid,
    pub status: String,
    pub exit_code: Option<i32>,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRequest {
    pub fence: i64,
}
#[derive(Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub cancel_requested: bool,
    pub lease_seconds: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogChunk {
    pub fence: i64,
    pub sequence: i64,
    pub stream: String,
    pub content: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteRequest {
    pub fence: i64,
    pub status: String,
    pub exit_code: Option<i32>,
    pub reason: String,
    pub last_log_sequence: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactInput {
    pub artifact_id: Uuid,
    pub source_run_id: Uuid,
    pub name: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRead {
    pub fence: i64,
    pub offset: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactData {
    pub offset: u64,
    pub data: String,
}
