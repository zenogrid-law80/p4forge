use crate::{
    AppState,
    auth::{Session, random_token, token_hash},
    error::ApiError,
    store,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, Request, State},
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, Utc};
use p4forge_domain::{Job, PipelineDefinition, Workspace, hash_bytes, validate_name};
use p4forge_perforce::SourceSnapshot;
use p4forge_protocol::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool, Postgres, Transaction, types::Json as DbJson};
use uuid::Uuid;

#[derive(Clone, FromRow, Serialize)]
pub struct Agent {
    pub id: Uuid,
    pub name: String,
    pub pool: String,
    pub os: String,
    pub arch: String,
    pub active: bool,
    pub draining: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub binary_sha256: Option<String>,
    pub agent_version: Option<String>,
    pub self_patch_ready: bool,
    pub perforce_managed: bool,
    pub docker_state: String,
    pub docker_engine_os: Option<String>,
    pub docker_checked_at: Option<DateTime<Utc>>,
}
const AGENT_COLUMNS: &str = "id,name,pool,os,arch,active,draining,last_seen_at,binary_sha256,agent_version,self_patch_ready,perforce_managed,docker_state,docker_engine_os,docker_checked_at";

pub async fn docker_status(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
    Json(input): Json<DockerCapability>,
) -> Result<Json<Value>, ApiError> {
    if !input.valid() {
        return Err(ApiError::bad_request(
            "Docker engine OS must accompany an available result",
        ));
    }
    let updated = sqlx::query("UPDATE agents SET docker_state=$2,docker_engine_os=$3,docker_checked_at=now() WHERE id=$1 AND active")
        .bind(agent.id)
        .bind(input.state.as_str())
        .bind(input.engine_os.map(DockerEngineOs::as_str))
        .execute(&state.pool).await?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::unauthorized("agent revoked"));
    }
    Ok(Json(json!({"ok":true})))
}

pub async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|token| token.len() == 43)
        .ok_or_else(|| ApiError::unauthorized("agent credential required"))?;
    let agent: Agent = sqlx::query_as(&format!(
        "SELECT {AGENT_COLUMNS} FROM agents WHERE token_hash=$1 AND active"
    ))
    .bind(token_hash(token))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::unauthorized("agent credential revoked or invalid"))?;
    request.extensions_mut().insert(agent);
    Ok(next.run(request).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgent {
    pub name: String,
    pub pool: String,
    pub os: String,
    pub arch: String,
}
#[derive(Serialize)]
pub struct CreatedAgent {
    pub agent: Agent,
    pub token: String,
}
pub async fn provision(
    pool: &PgPool,
    input: CreateAgent,
    actor: Option<Uuid>,
) -> Result<CreatedAgent, ApiError> {
    validate_name(&input.name).map_err(ApiError::bad_request)?;
    validate_name(&input.pool).map_err(ApiError::bad_request)?;
    if !matches!(input.os.as_str(), "linux" | "windows" | "macos")
        || !matches!(input.arch.as_str(), "x86_64" | "aarch64")
        || (input.os == "windows" && input.arch != "x86_64")
    {
        return Err(ApiError::bad_request(
            "supported agents: Linux/macOS x86_64/aarch64 or Windows x86_64",
        ));
    }
    let token = random_token();
    let mut tx = pool.begin().await?;
    let agent: Agent = sqlx::query_as(&format!("INSERT INTO agents(id,name,pool,os,arch,token_hash) VALUES($1,$2,$3,$4,$5,$6) RETURNING {AGENT_COLUMNS}"))
        .bind(Uuid::new_v4()).bind(input.name).bind(input.pool).bind(input.os).bind(input.arch).bind(token_hash(&token)).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO audit_events(user_id,action,details) VALUES($1,'agent.provisioned',$2)")
        .bind(actor).bind(json!({"agent_id":agent.id,"name":agent.name,"os":agent.os,"arch":agent.arch,"pool":agent.pool})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(CreatedAgent { agent, token })
}
pub async fn create(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Json(input): Json<CreateAgent>,
) -> Result<Json<CreatedAgent>, ApiError> {
    store::require_admin(&session.user)?;
    Ok(Json(
        provision(&state.pool, input, Some(session.user.id)).await?,
    ))
}
pub async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Vec<Agent>>, ApiError> {
    store::require_admin(&session.user)?;
    Ok(Json(
        sqlx::query_as(&format!("SELECT {AGENT_COLUMNS} FROM agents ORDER BY name"))
            .fetch_all(&state.pool)
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentControl {
    pub draining: bool,
    pub active: bool,
}
pub async fn control(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<AgentControl>,
) -> Result<Json<Agent>, ApiError> {
    store::require_admin(&session.user)?;
    let mut tx = state.pool.begin().await?;
    // Revocation is permanent: issue a new credential to provision a replacement agent.
    let agent: Agent = sqlx::query_as(&format!(
        "UPDATE agents SET draining=($2 OR EXISTS(SELECT 1 FROM agent_updates WHERE agent_id=$1 AND status IN ('requested','staged'))),active=active AND $3 WHERE id=$1 RETURNING {AGENT_COLUMNS}"
    ))
    .bind(id)
    .bind(input.draining)
    .bind(input.active)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    store::audit(
        &mut tx,
        session.user.id,
        None,
        "agent.control",
        json!({"agent_id":id,"active":agent.active,"draining":agent.draining}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(agent))
}

#[derive(FromRow)]
struct Attempt {
    id: Uuid,
    job_id: Uuid,
    status: String,
    lease_expires_at: DateTime<Utc>,
    cancel_requested: bool,
    last_log_sequence: i64,
    log_bytes: i64,
    completion_hash: Option<String>,
}
async fn assignment(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Assignment, ApiError> {
    let (attempt_id,job_id,run_id,fence,job,source): (Uuid,Uuid,Uuid,i64,DbJson<Job>,DbJson<SourceSnapshot>) = sqlx::query_as(
        "SELECT a.id,j.id,j.run_id,a.fence,j.definition,r.source_snapshot FROM job_attempts a JOIN job_runs j ON j.id=a.job_id JOIN runs r ON r.id=j.run_id WHERE a.id=$1")
        .bind(id).fetch_one(&mut **tx).await?;
    let definition: DbJson<PipelineDefinition> =
        sqlx::query_scalar("SELECT definition FROM runs WHERE id=$1")
            .bind(run_id)
            .fetch_one(&mut **tx)
            .await?;
    let mut workspace_jobs = Vec::new();
    if definition.workspace == Workspace::Pipeline {
        let rows: Vec<(Uuid, DbJson<Job>)> =
            sqlx::query_as("SELECT id,definition FROM job_runs WHERE run_id=$1 ORDER BY position")
                .bind(run_id)
                .fetch_all(&mut **tx)
                .await?;
        for name in definition.validate().map_err(ApiError::internal)?.order {
            let (id, job) = rows
                .iter()
                .find(|(_, job)| job.name == name)
                .ok_or_else(ApiError::not_found)?;
            workspace_jobs.push(AssignedJob {
                id: *id,
                job: job.0.clone(),
            });
        }
    }
    Ok(Assignment {
        attempt_id,
        job_id,
        run_id,
        fence,
        lease_seconds: LEASE_SECONDS,
        job: job.0,
        source: source.0,
        workspace_jobs,
        publish_generated: definition.publish_generated.clone(),
        artifacts: definition.artifacts.clone(),
        artifact_inputs: crate::artifact_inputs::load(tx, run_id).await?,
    })
}
pub async fn claim(
    State(state): State<AppState>,
    Extension(identity): Extension<Agent>,
    Json(input): Json<ClaimRequest>,
) -> Result<Json<ClaimResponse>, ApiError> {
    if input.request_id.is_nil() {
        return Err(ApiError::bad_request("request_id required"));
    }
    let mut tx = state.pool.begin().await?;
    let agent: Agent = sqlx::query_as(&format!(
        "SELECT {AGENT_COLUMNS} FROM agents WHERE id=$1 AND active FOR UPDATE"
    ))
    .bind(identity.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::unauthorized("agent revoked"))?;
    if agent.os != input.os || agent.arch != input.arch {
        return Err(ApiError::conflict(
            "agent platform does not match its approved capabilities",
        ));
    }
    sqlx::query("UPDATE agents SET last_seen_at=now(),last_protocol_version=$2,protocol_reported_at=now() WHERE id=$1")
        .bind(agent.id)
        .bind(i64::from(input.protocol_version))
        .execute(&mut *tx)
        .await?;
    let previous: Option<Attempt> =
        sqlx::query_as("SELECT * FROM job_attempts WHERE agent_id=$1 AND claim_request_id=$2")
            .bind(agent.id)
            .bind(input.request_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(previous) = previous {
        if previous.status != "running" || previous.lease_expires_at <= Utc::now() {
            return Err(ApiError::conflict("claim is no longer active"));
        }
        let mut value = assignment(&mut tx, previous.id).await?;
        if input.protocol_version < 8 && value.job.runner.docker.is_some() {
            return Err(ApiError::conflict(
                "Docker requirements need Agent protocol 8",
            ));
        }
        if input.protocol_version < 7 && !value.artifact_inputs.is_empty() {
            return Err(ApiError::conflict(
                "artifact inputs require Agent protocol 7",
            ));
        }
        value.lease_seconds = (previous.lease_expires_at - Utc::now())
            .num_seconds()
            .max(0) as u64;
        tx.commit().await?;
        return Ok(Json(ClaimResponse {
            assignment: Some(value),
        }));
    }
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM job_attempts WHERE agent_id=$1 AND status='running')",
    )
    .bind(agent.id)
    .fetch_one(&mut *tx)
    .await?;
    if agent.draining || busy {
        tx.commit().await?;
        return Ok(Json(ClaimResponse { assignment: None }));
    }
    // Only a fresh, successful probe can satisfy a Docker requirement. Runs
    // without one remain usable by existing native and specialized agents.
    let docker_engine = agent
        .docker_checked_at
        .filter(|checked| {
            let age = Utc::now().signed_duration_since(*checked);
            age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(150)
        })
        .filter(|_| agent.docker_state == "available")
        .and(agent.docker_engine_os.as_deref());
    // Every change to a run's dispatchable jobs first locks this run row.
    let run: Option<Uuid> = sqlx::query_scalar(
        "SELECT r.id FROM runs r JOIN pipelines p ON p.id=r.pipeline_id WHERE r.status IN ('queued','running') AND NOT r.cancel_requested
         AND (SELECT count(*) FROM job_runs j WHERE j.run_id=r.id AND j.status='running') < (r.definition->>'max_parallel_jobs')::integer
         AND ($5 >= 2 OR COALESCE(r.definition->>'workspace','job') <> 'pipeline')
         AND ($5 >= 3 OR NOT (r.definition ? 'publish_generated'))
         AND ($5 >= 4 OR COALESCE(r.source_snapshot->'root'->>'kind','stream') <> 'view')
         AND ($5 >= 7 OR jsonb_array_length(COALESCE(r.definition->'artifact_inputs','[]'::jsonb))=0)
         AND (NOT (r.definition ? 'deployment') OR EXISTS(SELECT 1 FROM run_deployments d JOIN deployment_environments e ON e.id=d.environment_id JOIN users u ON u.id=d.approved_by WHERE d.run_id=r.id AND d.status='approved' AND e.active_run_id=r.id AND e.executor_agent_id=$1 AND d.executor_agent_id=$1 AND u.active AND (u.is_admin OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=p.project_id AND m.user_id=u.id AND m.role='admin'))))
         AND ($5 >= 5 OR jsonb_array_length(COALESCE(r.definition->'artifacts','[]'::jsonb))=0)
         AND (NOT $6 OR ($5>=6 AND EXISTS(SELECT 1 FROM agent_perforce_credentials c WHERE c.agent_id=$1 AND c.role='read' AND jsonb_strip_nulls(c.connection)=jsonb_strip_nulls(r.source_snapshot->'connection') AND c.server_id=r.source_snapshot->>'server_id' AND c.server_state='ready' AND c.agent_state='ready' AND c.server_checked_at>now()-interval '150 seconds' AND c.agent_checked_at>now()-interval '150 seconds' AND LEAST(c.server_expires_at,c.agent_expires_at)>now()+interval '5 minutes')
         AND (NOT (r.definition ? 'publish_generated') OR EXISTS(SELECT 1 FROM agent_perforce_credentials c WHERE c.agent_id=$1 AND c.role='publish' AND c.connection->>'user'=r.definition->'publish_generated'->>'user' AND c.connection->>'port'=r.source_snapshot->'connection'->>'port' AND c.server_id=r.source_snapshot->>'server_id' AND c.server_state='ready' AND c.agent_state='ready' AND c.server_checked_at>now()-interval '150 seconds' AND c.agent_checked_at>now()-interval '150 seconds' AND LEAST(c.server_expires_at,c.agent_expires_at)>now()+interval '5 minutes'))))
         AND EXISTS(SELECT 1 FROM job_runs j WHERE j.run_id=r.id AND j.status='queued' AND j.definition->'runner'->>'pool'=$2 AND j.definition->'runner'->>'os'=$3 AND j.definition->'runner'->>'arch'=$4 AND (j.definition->'runner'->>'docker' IS NULL OR ($5>=8 AND j.definition->'runner'->>'docker'=$7)))
         ORDER BY r.created_at,r.id LIMIT 1 FOR UPDATE OF r SKIP LOCKED")
        .bind(agent.id).bind(&agent.pool).bind(&agent.os).bind(&agent.arch).bind(i64::from(input.protocol_version)).bind(agent.perforce_managed).bind(docker_engine).fetch_optional(&mut *tx).await?;
    let Some(run_id) = run else {
        tx.commit().await?;
        return Ok(Json(ClaimResponse { assignment: None }));
    };
    // A concurrent claim can commit between the candidate query's snapshot and
    // its row lock. Re-read capacity under the acquired run lock before dispatch.
    let available:bool=sqlx::query_scalar("SELECT NOT cancel_requested AND status IN ('queued','running') AND (SELECT count(*) FROM job_runs WHERE run_id=$1 AND status='running') < (definition->>'max_parallel_jobs')::integer FROM runs WHERE id=$1")
        .bind(run_id).fetch_one(&mut *tx).await?;
    if !available || !crate::deployments::start(&mut tx, run_id, agent.id).await? {
        tx.commit().await?;
        return Ok(Json(ClaimResponse { assignment: None }));
    }
    let job_id: Uuid = sqlx::query_scalar("SELECT id FROM job_runs WHERE run_id=$1 AND status='queued' AND definition->'runner'->>'pool'=$2 AND definition->'runner'->>'os'=$3 AND definition->'runner'->>'arch'=$4 AND (definition->'runner'->>'docker' IS NULL OR ($5>=8 AND definition->'runner'->>'docker'=$6)) ORDER BY position LIMIT 1 FOR UPDATE")
        .bind(run_id).bind(&agent.pool).bind(&agent.os).bind(&agent.arch).bind(i64::from(input.protocol_version)).bind(docker_engine).fetch_one(&mut *tx).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO job_attempts(id,job_id,agent_id,claim_request_id,status,lease_expires_at) VALUES($1,$2,$3,$4,'running',clock_timestamp()+interval '60 seconds')")
        .bind(id).bind(job_id).bind(agent.id).bind(input.request_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE job_runs SET status='running' WHERE id=$1")
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE runs SET status='running' WHERE id=$1")
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    let value = assignment(&mut tx, id).await?;
    if !value.workspace_jobs.is_empty() {
        sqlx::query(
            "UPDATE job_runs SET status='assigned',workspace_attempt_id=$2 WHERE run_id=$1",
        )
        .bind(run_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(ClaimResponse {
        assignment: Some(value),
    }))
}

/// Report an individual job while the single pipeline workspace lease stays live.
pub async fn step(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
    Path(id): Path<Uuid>,
    Json(input): Json<StepRequest>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(input.status.as_str(), "running" | "succeeded" | "failed")
        || input.reason.len() > 1024
        || input.reason.contains('\0')
        || (input.status == "running" && (input.exit_code.is_some() || !input.reason.is_empty()))
        || (input.status == "succeeded" && input.exit_code != Some(0))
    {
        return Err(ApiError::bad_request("invalid workspace job event"));
    }
    let hash = hash_bytes(&serde_json::to_vec(&input).map_err(ApiError::internal)?);
    let mut tx = state.pool.begin().await?;
    let (_, attempt) = lock_attempt(&mut tx, agent.id, id, input.fence).await?;
    live(&attempt)?;
    let (status,start_hash,completion_hash): (String,Option<String>,Option<String>) = sqlx::query_as("SELECT status,step_start_hash,step_completion_hash FROM job_runs WHERE id=$1 AND workspace_attempt_id=$2")
        .bind(input.job_id).bind(id).fetch_optional(&mut *tx).await?.ok_or_else(ApiError::not_found)?;
    let previous = if input.status == "running" {
        start_hash
    } else {
        completion_hash
    };
    if let Some(previous) = previous {
        if previous == hash {
            return Ok(Json(json!({"ok":true})));
        }
        return Err(ApiError::conflict("workspace job event changed"));
    }
    if input.status == "running" {
        if status != "assigned" || attempt.cancel_requested {
            return Err(ApiError::conflict("workspace job cannot start"));
        }
        let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM job_runs WHERE workspace_attempt_id=$1 AND status='running') OR EXISTS(SELECT 1 FROM job_dependencies d JOIN job_runs n ON n.id=d.needs_id WHERE d.job_id=$2 AND n.status<>'succeeded') OR EXISTS(SELECT 1 FROM job_runs WHERE workspace_attempt_id=$1 AND status IN ('failed','canceled','lost'))")
            .bind(id).bind(input.job_id).fetch_one(&mut *tx).await?;
        if blocked {
            return Err(ApiError::conflict(
                "workspace stage/dependency barrier is not complete",
            ));
        }
        sqlx::query("UPDATE job_runs SET status='running',step_started_at=now(),step_start_hash=$2 WHERE id=$1")
            .bind(input.job_id).bind(hash).execute(&mut *tx).await?;
    } else {
        if status != "running" {
            return Err(ApiError::conflict("workspace job is not running"));
        }
        sqlx::query("UPDATE job_runs SET status=$2,step_finished_at=now(),step_exit_code=$3,step_reason=$4,step_completion_hash=$5 WHERE id=$1")
            .bind(input.job_id).bind(&input.status).bind(input.exit_code).bind(&input.reason).bind(hash).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}

async fn lock_attempt(
    tx: &mut Transaction<'_, Postgres>,
    agent: Uuid,
    id: Uuid,
    fence: i64,
) -> Result<(Uuid, Attempt), ApiError> {
    let run: Uuid=sqlx::query_scalar("SELECT j.run_id FROM job_attempts a JOIN job_runs j ON j.id=a.job_id WHERE a.id=$1 AND a.agent_id=$2 AND a.fence=$3")
        .bind(id).bind(agent).bind(fence).fetch_optional(&mut **tx).await?.ok_or_else(ApiError::not_found)?;
    sqlx::query("SELECT id FROM runs WHERE id=$1 FOR UPDATE")
        .bind(run)
        .execute(&mut **tx)
        .await?;
    let attempt: Attempt = sqlx::query_as("SELECT * FROM job_attempts WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    Ok((run, attempt))
}
fn live(attempt: &Attempt) -> Result<(), ApiError> {
    if attempt.status != "running" || attempt.lease_expires_at <= Utc::now() {
        return Err(ApiError::conflict("attempt lease expired or finished"));
    }
    Ok(())
}
pub async fn heartbeat(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
    Path(id): Path<Uuid>,
    Json(input): Json<AttemptRequest>,
) -> Result<Json<HeartbeatResponse>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let (_, attempt) = lock_attempt(&mut tx, agent.id, id, input.fence).await?;
    live(&attempt)?;
    sqlx::query("UPDATE job_attempts SET lease_expires_at=clock_timestamp()+interval '60 seconds' WHERE id=$1").bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE agents SET last_seen_at=now() WHERE id=$1")
        .bind(agent.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(HeartbeatResponse {
        cancel_requested: attempt.cancel_requested,
        lease_seconds: LEASE_SECONDS,
    }))
}
pub async fn append_log(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
    Path(id): Path<Uuid>,
    Json(input): Json<LogChunk>,
) -> Result<Json<Value>, ApiError> {
    if input.sequence < 0
        || input.sequence >= 4096
        || input.content.is_empty()
        || input.content.len() > MAX_LOG_CHUNK
        || input.content.contains('\0')
        || !matches!(input.stream.as_str(), "stdout" | "stderr" | "system")
    {
        return Err(ApiError::bad_request("invalid log chunk"));
    }
    let mut tx = state.pool.begin().await?;
    let (_, attempt) = lock_attempt(&mut tx, agent.id, id, input.fence).await?;
    live(&attempt)?;
    if input.sequence <= attempt.last_log_sequence {
        let previous: Option<(String, String)> = sqlx::query_as(
            "SELECT stream,content FROM attempt_logs WHERE attempt_id=$1 AND sequence=$2",
        )
        .bind(id)
        .bind(input.sequence)
        .fetch_optional(&mut *tx)
        .await?;
        if previous != Some((input.stream, input.content)) {
            return Err(ApiError::conflict(
                "log sequence was used for different content",
            ));
        }
    } else {
        if input.sequence != attempt.last_log_sequence + 1 {
            return Err(ApiError::conflict("log sequence gap"));
        }
        if attempt.log_bytes + input.content.len() as i64 > MAX_LOG_BYTES {
            return Err(ApiError(
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                "attempt log limit reached".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO attempt_logs(attempt_id,sequence,stream,content) VALUES($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(input.sequence)
        .bind(&input.stream)
        .bind(&input.content)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE job_attempts SET last_log_sequence=$2,log_bytes=log_bytes+$3 WHERE id=$1",
        )
        .bind(id)
        .bind(input.sequence)
        .bind(input.content.len() as i64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
pub async fn complete(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
    Path(id): Path<Uuid>,
    Json(input): Json<CompleteRequest>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(input.status.as_str(), "succeeded" | "failed" | "canceled")
        || input.reason.len() > 1024
        || input.reason.contains('\0')
        || (input.status == "succeeded" && input.exit_code != Some(0))
    {
        return Err(ApiError::bad_request("invalid completion"));
    }
    let hash = hash_bytes(&serde_json::to_vec(&input).map_err(ApiError::internal)?);
    let mut tx = state.pool.begin().await?;
    let (run, attempt) = lock_attempt(&mut tx, agent.id, id, input.fence).await?;
    if let Some(previous) = attempt.completion_hash {
        if previous == hash {
            return Ok(Json(json!({"ok":true,"status":attempt.status})));
        }
        return Err(ApiError::conflict("completion changed"));
    }
    live(&attempt)?;
    if input.last_log_sequence != attempt.last_log_sequence {
        return Err(ApiError::conflict("log upload is incomplete"));
    }
    let status = if attempt.cancel_requested {
        "canceled"
    } else {
        input.status.as_str()
    };
    let shared: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM job_runs WHERE workspace_attempt_id=$1)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if status == "succeeded" {
        crate::artifacts::require_complete(&mut tx, run, id).await?;
    }
    if shared && status == "succeeded" {
        let missing: bool = sqlx::query_scalar("SELECT (definition ? 'publish_generated') AND NOT EXISTS(SELECT 1 FROM run_publications WHERE run_id=$1 AND status IN ('submitted','unchanged')) FROM runs WHERE id=$1")
            .bind(run).fetch_one(&mut *tx).await?;
        if missing {
            return Err(ApiError::conflict("publication has not been verified"));
        }
        let unfinished: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM job_runs WHERE workspace_attempt_id=$1 AND status<>'succeeded')")
            .bind(id).fetch_one(&mut *tx).await?;
        if unfinished {
            return Err(ApiError::conflict("workspace jobs have not all succeeded"));
        }
    }
    sqlx::query("UPDATE job_attempts SET status=$2,exit_code=$3,reason=$4,completion_hash=$5,finished_at=now() WHERE id=$1")
        .bind(id).bind(status).bind(input.exit_code).bind(input.reason).bind(hash).execute(&mut *tx).await?;
    if shared {
        finish_workspace(&mut tx, id, status).await?;
    } else {
        sqlx::query("UPDATE job_runs SET status=$2 WHERE id=$1")
            .bind(attempt.job_id)
            .bind(status)
            .execute(&mut *tx)
            .await?;
    }
    settle(&mut tx, run).await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true,"status":status})))
}

async fn finish_workspace(
    tx: &mut Transaction<'_, Postgres>,
    attempt: Uuid,
    status: &str,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE job_runs SET status=CASE WHEN status='running' THEN $2 WHEN $2='canceled' THEN 'canceled' ELSE 'skipped' END,step_finished_at=now(),step_reason=CASE WHEN step_reason='' THEN 'Pipeline workspace stopped: ' || $2 ELSE step_reason END WHERE workspace_attempt_id=$1 AND status IN ('assigned','running')")
        .bind(attempt).bind(status).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn settle(tx: &mut Transaction<'_, Postgres>, run: Uuid) -> Result<(), ApiError> {
    loop {
        let changed=sqlx::query("UPDATE job_runs j SET status='skipped' WHERE j.run_id=$1 AND j.status='blocked' AND EXISTS(SELECT 1 FROM job_dependencies d JOIN job_runs n ON n.id=d.needs_id WHERE d.job_id=j.id AND n.status IN ('failed','canceled','skipped','lost'))")
            .bind(run).execute(&mut **tx).await?.rows_affected();
        if changed == 0 {
            break;
        }
    }
    sqlx::query("UPDATE job_runs j SET status='queued' WHERE j.run_id=$1 AND j.status='blocked' AND NOT EXISTS(SELECT 1 FROM job_dependencies d JOIN job_runs n ON n.id=d.needs_id WHERE d.job_id=j.id AND n.status<>'succeeded')")
        .bind(run).execute(&mut **tx).await?;
    sqlx::query("UPDATE runs r SET status=CASE WHEN EXISTS(SELECT 1 FROM job_runs WHERE run_id=r.id AND status='lost') THEN 'failed' WHEN r.cancel_requested THEN 'canceled' WHEN EXISTS(SELECT 1 FROM job_runs WHERE run_id=r.id AND status<>'succeeded') OR EXISTS(SELECT 1 FROM job_attempts a JOIN job_runs j ON j.id=a.job_id WHERE j.run_id=r.id AND a.status IN ('failed','lost','canceled')) THEN 'failed' ELSE 'succeeded' END,finished_at=now() WHERE r.id=$1 AND r.status IN ('queued','running') AND NOT EXISTS(SELECT 1 FROM job_runs WHERE run_id=r.id AND status IN ('queued','blocked','assigned','running','finalizing')) AND NOT EXISTS(SELECT 1 FROM job_attempts a JOIN job_runs j ON j.id=a.job_id WHERE j.run_id=r.id AND a.status='running')")
        .bind(run).execute(&mut **tx).await?;
    crate::deployments::settle(tx, run).await?;
    Ok(())
}
pub async fn reap(pool: &PgPool) -> Result<(), ApiError> {
    let mut tx = pool.begin().await?;
    let runs:Vec<Uuid>=sqlx::query_scalar("SELECT r.id FROM runs r WHERE r.status IN ('queued','running') AND EXISTS(SELECT 1 FROM job_runs j JOIN job_attempts a ON a.job_id=j.id JOIN agents g ON g.id=a.agent_id WHERE j.run_id=r.id AND a.status='running' AND (a.lease_expires_at<=clock_timestamp() OR NOT g.active)) ORDER BY r.id LIMIT 100 FOR UPDATE SKIP LOCKED")
        .fetch_all(&mut *tx).await?;
    for run in runs {
        let jobs:Vec<Uuid>=sqlx::query_scalar("UPDATE job_attempts a SET status='lost',finished_at=now(),reason='Agent lease expired or credential revoked; not automatically retried' FROM job_runs j,agents g WHERE a.job_id=j.id AND a.agent_id=g.id AND j.run_id=$1 AND a.status='running' AND (a.lease_expires_at<=clock_timestamp() OR NOT g.active) RETURNING a.job_id")
            .bind(run).fetch_all(&mut *tx).await?;
        for job in jobs {
            let shared: Option<Uuid> =
                sqlx::query_scalar("SELECT workspace_attempt_id FROM job_runs WHERE id=$1")
                    .bind(job)
                    .fetch_one(&mut *tx)
                    .await?;
            if let Some(attempt) = shared {
                finish_workspace(&mut tx, attempt, "lost").await?;
            } else {
                sqlx::query("UPDATE job_runs SET status='lost' WHERE id=$1")
                    .bind(job)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        settle(&mut tx, run).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn jobs(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let _ = store::run_detail(State(state.clone()), Extension(session), Path(id)).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',j.id,'name',j.name,'stage',j.definition->>'stage','status',j.status,'needs',COALESCE((SELECT jsonb_agg(n.name ORDER BY n.position) FROM job_dependencies d JOIN job_runs n ON n.id=d.needs_id WHERE d.job_id=j.id),'[]'::jsonb),'attempt_id',a.id,'agent_name',g.name,'shared_workspace',j.workspace_attempt_id IS NOT NULL,'started_at',CASE WHEN j.workspace_attempt_id IS NULL THEN a.started_at ELSE j.step_started_at END,'finished_at',CASE WHEN j.workspace_attempt_id IS NULL THEN a.finished_at ELSE j.step_finished_at END,'exit_code',CASE WHEN j.workspace_attempt_id IS NULL THEN a.exit_code ELSE j.step_exit_code END,'reason',CASE WHEN j.workspace_attempt_id IS NULL THEN a.reason ELSE j.step_reason END,'cancel_requested',a.cancel_requested) FROM job_runs j LEFT JOIN job_attempts a ON a.id=j.workspace_attempt_id OR (j.workspace_attempt_id IS NULL AND a.job_id=j.id) LEFT JOIN agents g ON g.id=a.agent_id WHERE j.run_id=$1 ORDER BY j.position")
        .bind(id).fetch_all(&state.pool).await?;
    Ok(Json(json!(rows)))
}
#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(default = "before_first")]
    after: i64,
}
fn before_first() -> i64 {
    -1
}
pub async fn logs(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Query(query): Query<LogQuery>,
) -> Result<Json<Value>, ApiError> {
    let run: Uuid = sqlx::query_scalar(
        "SELECT j.run_id FROM job_attempts a JOIN job_runs j ON j.id=a.job_id WHERE a.id=$1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let _ = store::run_detail(State(state.clone()), Extension(session), Path(run)).await?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('sequence',sequence,'stream',stream,'content',content) FROM attempt_logs WHERE attempt_id=$1 AND sequence>$2 ORDER BY sequence LIMIT 16")
        .bind(id).bind(query.after).fetch_all(&state.pool).await?;
    Ok(Json(json!(rows)))
}

/// A receipt is accepted only from the live supervisor; stdout is never parsed as authority.
pub async fn publication(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
    Path(id): Path<Uuid>,
    Json(input): Json<p4forge_protocol::PublicationRequest>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(
        input.status.as_str(),
        "prepared" | "submitted" | "unchanged"
    ) || input.changelist <= 0
    {
        return Err(ApiError::bad_request("invalid publication receipt"));
    }
    let mut tx = state.pool.begin().await?;
    let (run, attempt) = lock_attempt(&mut tx, agent.id, id, input.fence).await?;
    live(&attempt)?;
    if attempt.cancel_requested {
        return Err(ApiError::conflict("publication canceled"));
    }
    let a = assignment(&mut tx, id).await?;
    let spec = a
        .publish_generated
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("no publication configured"))?;
    let incomplete: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM job_runs WHERE run_id=$1 AND status<>'succeeded')",
    )
    .bind(run)
    .fetch_one(&mut *tx)
    .await?;
    if incomplete {
        return Err(ApiError::conflict(
            "build jobs must succeed before publication",
        ));
    }
    let previous: Option<(String, i64)> =
        sqlx::query_as("SELECT status,reserved_changelist FROM run_publications WHERE run_id=$1")
            .bind(run)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((status, cl)) = &previous {
        if *cl == input.changelist && *status == input.status {
            let actual: i64 =
                sqlx::query_scalar("SELECT changelist FROM run_publications WHERE run_id=$1")
                    .bind(run)
                    .fetch_one(&mut *tx)
                    .await?;
            tx.commit().await?;
            return Ok(Json(json!({"ok":true,"changelist":actual})));
        }
        if status != "prepared" || input.status != "submitted" || *cl != input.changelist {
            return Err(ApiError::conflict(
                "publication receipt differs from reserved changelist",
            ));
        }
    } else if input.status == "submitted" {
        return Err(ApiError::conflict("reserve changelist before submit"));
    }
    let mut actual_changelist = input.changelist;
    if input.status == "unchanged" {
        if input.changelist != a.source.changelist {
            return Err(ApiError::bad_request(
                "unchanged publication must retain input CL",
            ));
        }
    } else {
        if input.changelist <= a.source.changelist {
            return Err(ApiError::bad_request("published CL must follow input CL"));
        }
        // Pending changelists are private to the publisher; submitted receipts are independently checked.
        if input.status == "submitted" {
            let fields = state
                .perforce
                .tagged(
                    &a.source.connection,
                    &["describe", "-s", "-O", &input.changelist.to_string()],
                )
                .await
                .map_err(|_| ApiError::unavailable("cannot verify submitted changelist"))?;
            actual_changelist = fields
                .get("change")
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|cl| *cl >= input.changelist)
                .ok_or_else(|| ApiError::conflict("invalid submitted change number"))?;
            let stream = match &a.source.root {
                p4forge_perforce::Root::Stream { stream } => stream,
                _ => return Err(ApiError::bad_request("publication requires a stream")),
            };
            let marker = format!(
                "P4Forge run={} attempt={} base={}",
                run, id, a.source.changelist
            );
            let name = format!("p4forge_publish_{}", id.simple());
            let files: Vec<_> = fields
                .iter()
                .filter(|(k, _)| {
                    k.strip_prefix("depotFile").is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                    })
                })
                .map(|(_, v)| v)
                .collect();
            if fields.get("status").map(String::as_str) != Some("submitted")
                || fields.get("user") != Some(&spec.user)
                || fields.get("client") != Some(&name)
                || fields.get("desc").map(|s| s.trim()) != Some(marker.as_str())
                || files.is_empty()
                || !files.iter().all(|f| {
                    spec.paths
                        .iter()
                        .any(|p| f.starts_with(&format!("{stream}/{p}/")))
                })
            {
                return Err(ApiError::conflict(
                    "submitted changelist does not match publication identity and allowed paths",
                ));
            }
        }
    }
    sqlx::query("INSERT INTO run_publications(run_id,attempt_id,changelist,status,verified_at,reserved_changelist) VALUES($1,$2,$3,$4,CASE WHEN $4='prepared' THEN NULL ELSE now() END,$5) ON CONFLICT(run_id) DO UPDATE SET status=EXCLUDED.status,verified_at=EXCLUDED.verified_at,changelist=EXCLUDED.changelist")
        .bind(run).bind(id).bind(actual_changelist).bind(&input.status).bind(input.changelist).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true,"changelist":actual_changelist})))
}

/// Artifact I/O uses the same run/attempt lock order as completion and cancellation.
pub(crate) async fn artifact_attempt(
    tx: &mut Transaction<'_, Postgres>,
    agent: Uuid,
    id: Uuid,
    fence: i64,
) -> Result<Uuid, ApiError> {
    let (run, attempt) = lock_attempt(tx, agent, id, fence).await?;
    live(&attempt)?;
    if attempt.cancel_requested {
        return Err(ApiError::conflict("artifact upload canceled"));
    }
    let unfinished: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM job_runs WHERE run_id=$1 AND status<>'succeeded')",
    )
    .bind(run)
    .fetch_one(&mut **tx)
    .await?;
    if unfinished {
        return Err(ApiError::conflict(
            "build jobs must succeed before artifact upload",
        ));
    }
    Ok(run)
}

// Input downloads use the existing attempt fence and lease before the jobs start.
pub(crate) async fn input_attempt(
    tx: &mut Transaction<'_, Postgres>,
    agent: Uuid,
    id: Uuid,
    fence: i64,
) -> Result<Uuid, ApiError> {
    let (run, attempt) = lock_attempt(tx, agent, id, fence).await?;
    live(&attempt)?;
    let canceled: bool = sqlx::query_scalar(
        "SELECT cancel_requested OR status NOT IN ('queued','running') FROM runs WHERE id=$1",
    )
    .bind(run)
    .fetch_one(&mut **tx)
    .await?;
    if canceled || attempt.cancel_requested {
        return Err(ApiError::conflict("artifact input download canceled"));
    }
    Ok(run)
}
