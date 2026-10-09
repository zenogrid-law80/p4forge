//! Approval and environment ownership are durable server state, never log messages.
use crate::{
    AppState,
    auth::Session,
    error::ApiError,
    store::{self, Role},
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use p4forge_domain::{PipelineDefinition, hash_bytes, validate_name};
use p4forge_perforce::SourceSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction, types::Json as DbJson};
use uuid::Uuid;

#[derive(FromRow, Serialize)]
pub struct Environment {
    id: Uuid,
    project_id: Uuid,
    pipeline_id: Uuid,
    name: String,
    generation: i64,
    active_run_id: Option<Uuid>,
    current_run_id: Option<Uuid>,
    previous_run_id: Option<Uuid>,
    health: String,
    checked_at: Option<chrono::DateTime<chrono::Utc>>,
    executor_agent_id: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    name: String,
    pipeline_id: Uuid,
}

#[derive(FromRow, Serialize)]
pub struct Executor {
    id: Uuid,
    name: String,
    pool: String,
    os: String,
    arch: String,
    active: bool,
    draining: bool,
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    docker_state: String,
    docker_engine_os: Option<String>,
    docker_checked_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn executors(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<Executor>>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Viewer).await?;
    Ok(Json(
        sqlx::query_as("SELECT id,name,pool,os,arch,active,draining,last_seen_at,docker_state,docker_engine_os,docker_checked_at FROM agents ORDER BY name")
            .fetch_all(&state.pool)
            .await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorBinding {
    generation: i64,
    agent_id: Uuid,
    note: String,
}

pub async fn bind_executor(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<ExecutorBinding>,
) -> Result<Json<Environment>, ApiError> {
    note(&input.note)?;
    let project: Uuid =
        sqlx::query_scalar("SELECT project_id FROM deployment_environments WHERE id=$1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(ApiError::not_found)?;
    store::require_role(&state.pool, &session.user, project, Role::Admin).await?;
    let mut tx = state.pool.begin().await?;
    // Dispatch locks Agent before Run/Environment. Never wait for an Agent under
    // an Environment lock; the binding does not need or acquire a Run lock.
    let agent: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM agents WHERE id=$1 AND active FOR SHARE")
            .bind(input.agent_id)
            .fetch_optional(&mut *tx)
            .await?;
    if agent.is_none() {
        return Err(ApiError::bad_request("select an active global Agent"));
    }
    let env = lock_environment(&mut tx, id).await?;
    authority(&mut tx, session.user.id, project).await?;
    if env.generation != input.generation || env.active_run_id.is_some() {
        return Err(ApiError::conflict(
            "environment changed or is locked; finish/cancel the deployment before changing its executor",
        ));
    }
    let updated: Environment = sqlx::query_as("UPDATE deployment_environments SET executor_agent_id=$2,generation=generation+1 WHERE id=$1 RETURNING *")
        .bind(id).bind(input.agent_id).fetch_one(&mut *tx).await?;
    store::audit(&mut tx,session.user.id,Some(project),"deployment.executor_changed",
        json!({"environment_id":id,"previous_agent_id":env.executor_agent_id,"agent_id":input.agent_id,"generation":updated.generation,"note":input.note})).await?;
    tx.commit().await?;
    Ok(Json(updated))
}

// Re-read and lock current authority through commit, including global-admin revocation.
async fn authority(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
    project: Uuid,
) -> Result<(), ApiError> {
    let admin: Option<bool> =
        sqlx::query_scalar("SELECT is_admin FROM users WHERE id=$1 AND active FOR SHARE")
            .bind(user)
            .fetch_optional(&mut **tx)
            .await?;
    if admin == Some(true) {
        return Ok(());
    }
    if admin == Some(false) {
        let role: Option<String> = sqlx::query_scalar(
            "SELECT role FROM project_members WHERE user_id=$1 AND project_id=$2 FOR SHARE",
        )
        .bind(user)
        .bind(project)
        .fetch_optional(&mut **tx)
        .await?;
        if role.as_deref() == Some("admin") {
            return Ok(());
        }
    }
    Err(ApiError::forbidden(
        "current project administrator approval required",
    ))
}
fn note(value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.len() > 1024 || value.contains('\0') {
        return Err(ApiError::bad_request(
            "a reason of 1..1024 bytes is required",
        ));
    }
    Ok(())
}
async fn lock_environment(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Environment, ApiError> {
    sqlx::query_as("SELECT * FROM deployment_environments WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(ApiError::not_found)
}
#[derive(FromRow, Serialize)]
pub struct EnvironmentSummary {
    #[sqlx(flatten)]
    #[serde(flatten)]
    environment: Environment,
    active_deployment_status: Option<String>,
    pending_approval_count: i64,
    pending_approval_run_id: Option<Uuid>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<EnvironmentSummary>>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Viewer).await?;
    Ok(Json(
        sqlx::query_as(
            "SELECT e.*,d.status AS active_deployment_status,
                    (SELECT count(*) FROM run_deployments pending
                     WHERE pending.environment_id=e.id AND pending.status='awaiting_approval') AS pending_approval_count,
                    (SELECT pending.run_id FROM run_deployments pending JOIN runs r ON r.id=pending.run_id
                     WHERE pending.environment_id=e.id AND pending.status='awaiting_approval'
                     ORDER BY r.created_at DESC,r.id DESC LIMIT 1) AS pending_approval_run_id
             FROM deployment_environments e LEFT JOIN run_deployments d ON d.run_id=e.active_run_id
             WHERE e.project_id=$1 ORDER BY e.name",
        )
            .bind(project)
            .fetch_all(&state.pool)
            .await?,
    ))
}
pub async fn create(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Json(input): Json<Create>,
) -> Result<Json<Environment>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Admin).await?;
    validate_name(&input.name).map_err(ApiError::bad_request)?;
    let mut tx = state.pool.begin().await?;
    authority(&mut tx, session.user.id, project).await?;
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pipelines WHERE id=$1 AND project_id=$2) AND (SELECT count(*) FROM deployment_environments WHERE project_id=$2)<64")
        .bind(input.pipeline_id).bind(project).fetch_one(&mut *tx).await?;
    if !valid {
        return Err(ApiError::bad_request(
            "select a pipeline in this project; at most 64 environments",
        ));
    }
    let env: Environment = sqlx::query_as("INSERT INTO deployment_environments(id,project_id,pipeline_id,name) VALUES($1,$2,$3,$4) RETURNING *")
        .bind(Uuid::new_v4()).bind(project).bind(input.pipeline_id).bind(input.name).fetch_one(&mut *tx).await?;
    store::audit(
        &mut tx,
        session.user.id,
        Some(project),
        "deployment.environment_created",
        json!({"environment_id":env.id,"pipeline_id":env.pipeline_id,"name":env.name}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(env))
}
pub(crate) async fn validate(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    pipeline: Uuid,
    definition: &PipelineDefinition,
) -> Result<(), ApiError> {
    if let Some(deployment) = &definition.deployment {
        let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM deployment_environments WHERE id=$1 AND project_id=$2 AND pipeline_id=$3)")
            .bind(deployment.environment_id).bind(project).bind(pipeline).fetch_one(&mut **tx).await?;
        if !valid {
            return Err(ApiError::bad_request(
                "deployment environment must be bound to this project and pipeline",
            ));
        }
    }
    Ok(())
}
pub(crate) async fn bind(
    tx: &mut Transaction<'_, Postgres>,
    run: Uuid,
    project: Uuid,
    pipeline: Uuid,
    definition: &PipelineDefinition,
    rollback_of: Option<Uuid>,
) -> Result<(), ApiError> {
    validate(tx, project, pipeline, definition).await?;
    if let Some(deployment) = &definition.deployment {
        sqlx::query(
            "INSERT INTO run_deployments(run_id,environment_id,rollback_of) VALUES($1,$2,$3)",
        )
        .bind(run)
        .bind(deployment.environment_id)
        .bind(rollback_of)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}
pub async fn get(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(run): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let detail = store::run_detail(State(state.clone()), Extension(session), Path(run))
        .await?
        .0;
    let deployment: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(d) || jsonb_build_object('environment',to_jsonb(e),'approver_email',u.email) FROM run_deployments d JOIN deployment_environments e ON e.id=d.environment_id LEFT JOIN users u ON u.id=d.approved_by WHERE d.run_id=$1")
        .bind(run).fetch_optional(&state.pool).await?;
    Ok(Json(
        json!({"deployment":deployment,"input_hash":detail.input_hash,"version":detail.version,"changelist":detail.source_snapshot["changelist"]}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    generation: i64,
    input_hash: String,
    approve: bool,
    note: String,
}
pub async fn decide(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(run): Path<Uuid>,
    Json(input): Json<Decision>,
) -> Result<Json<Value>, ApiError> {
    note(&input.note)?;
    let detail = store::run_detail(State(state.clone()), Extension(session.clone()), Path(run))
        .await?
        .0;
    let pipeline =
        store::pipeline_access(&state, &session.user, detail.pipeline_id, Role::Admin).await?;
    let mut tx = state.pool.begin().await?;
    // All lifecycle paths lock Run before Environment. Never reverse this order.
    let (status, canceled, hash): (String, bool, String) = sqlx::query_as(
        "SELECT status,cancel_requested,input_hash FROM runs WHERE id=$1 FOR UPDATE",
    )
    .bind(run)
    .fetch_one(&mut *tx)
    .await?;
    let (environment, decision): (Uuid, String) =
        sqlx::query_as("SELECT environment_id,status FROM run_deployments WHERE run_id=$1")
            .bind(run)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(ApiError::not_found)?;
    let env = lock_environment(&mut tx, environment).await?;
    authority(&mut tx, session.user.id, pipeline.project_id).await?;
    if status != "queued"
        || canceled
        || decision != "awaiting_approval"
        || input.input_hash != hash
        || input.generation != env.generation
    {
        return Err(ApiError::conflict(
            "deployment or environment changed; refresh and review the frozen inputs",
        ));
    }
    if input.approve {
        if env.active_run_id.is_some() {
            return Err(ApiError::conflict(
                "environment is locked by another deployment or unresolved failure",
            ));
        }
        // The exact Agent is part of this review. Pool/platform still apply;
        // another Agent in the same pool must never receive this deployment.
        let eligible: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agents a WHERE a.id=$1 AND a.active AND EXISTS(SELECT 1 FROM job_runs j WHERE j.run_id=$2 AND j.definition->'runner'->>'pool'=a.pool AND j.definition->'runner'->>'os'=a.os AND j.definition->'runner'->>'arch'=a.arch))")
            .bind(env.executor_agent_id).bind(run).fetch_one(&mut *tx).await?;
        if !eligible {
            return Err(ApiError::conflict(
                "configure an active environment executor matching the deployment pool and platform before approval",
            ));
        }
        sqlx::query("UPDATE deployment_environments SET active_run_id=$2,generation=generation+1 WHERE id=$1")
            .bind(environment).bind(run).execute(&mut *tx).await?;
        sqlx::query("UPDATE run_deployments SET status='approved',approved_by=$2,approved_at=now(),note=$3,executor_agent_id=$4 WHERE run_id=$1")
            .bind(run).bind(session.user.id).bind(&input.note).bind(env.executor_agent_id).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE run_deployments SET status='rejected',note=$2,finished_at=now() WHERE run_id=$1").bind(run).bind(&input.note).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET cancel_requested=true WHERE id=$1")
            .bind(run)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE job_runs SET status='canceled' WHERE run_id=$1 AND status IN ('queued','blocked')").bind(run).execute(&mut *tx).await?;
        crate::agents::settle(&mut tx, run).await?;
    }
    store::audit(&mut tx,session.user.id,Some(pipeline.project_id),if input.approve {"deployment.approved"} else {"deployment.rejected"},json!({"run_id":run,"environment_id":environment,"input_hash":hash,"note":input.note,"generation":env.generation,"executor_agent_id":env.executor_agent_id})).await?;
    tx.commit().await?;
    get(State(state), Extension(session), Path(run)).await
}
// Called under the dispatch Run lock. Approval alone is not sufficient after revocation.
pub(crate) async fn start(
    tx: &mut Transaction<'_, Postgres>,
    run: Uuid,
    agent: Uuid,
) -> Result<bool, ApiError> {
    let row: Option<(Uuid, String, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT environment_id,status,approved_by,executor_agent_id FROM run_deployments WHERE run_id=$1",
    )
    .bind(run)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((id, status, approver, executor)) = row else {
        return Ok(true);
    };
    let env = lock_environment(tx, id).await?;
    if env.active_run_id != Some(run)
        || status != "approved"
        || env.executor_agent_id != Some(agent)
        || executor != Some(agent)
    {
        return Ok(false);
    }
    let Some(approver) = approver else {
        return Ok(false);
    };
    if let Err(error) = authority(tx, approver, env.project_id).await {
        if error.0 == axum::http::StatusCode::FORBIDDEN {
            return Ok(false);
        }
        return Err(error);
    }
    sqlx::query("UPDATE run_deployments SET status='running' WHERE run_id=$1")
        .bind(run)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "UPDATE deployment_environments SET health='unknown',generation=generation+1 WHERE id=$1",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

/// Dedicated executors recheck the live attempt and approval immediately before
/// touching their fixed target. This is a current check, not an offline permit.
pub async fn authorize_attempt(
    State(state): State<AppState>,
    Extension(agent): Extension<crate::agents::Agent>,
    Path(attempt): Path<Uuid>,
    Json(input): Json<p4forge_protocol::AttemptRequest>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let run = crate::agents::input_attempt(&mut tx, agent.id, attempt, input.fence).await?;
    let (environment, status, approver, executor): (Uuid, String, Option<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT environment_id,status,approved_by,executor_agent_id FROM run_deployments WHERE run_id=$1",
    )
    .bind(run)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::forbidden("an approved deployment is required"))?;
    let env = lock_environment(&mut tx, environment).await?;
    if status != "running" || env.active_run_id != Some(run) {
        return Err(ApiError::conflict(
            "deployment no longer owns this environment",
        ));
    }
    if env.executor_agent_id != Some(agent.id) || executor != Some(agent.id) {
        return Err(ApiError::forbidden(
            "Agent is not the approved environment executor",
        ));
    }
    authority(
        &mut tx,
        approver.ok_or_else(|| ApiError::forbidden("deployment approval missing"))?,
        env.project_id,
    )
    .await?;
    let (pipeline, hash): (Uuid, String) =
        sqlx::query_as("SELECT pipeline_id,input_hash FROM runs WHERE id=$1")
            .bind(run)
            .fetch_one(&mut *tx)
            .await?;
    if env.pipeline_id != pipeline {
        return Err(ApiError::conflict("deployment pipeline binding changed"));
    }
    let inputs = crate::artifact_inputs::load(&mut tx, run).await?;
    tx.commit().await?;
    Ok(Json(json!({"run_id":run,"project_id":env.project_id,
        "pipeline_id":pipeline,"environment_id":environment,
        "input_hash":hash,"artifact_inputs":inputs})))
}
pub(crate) async fn settle(tx: &mut Transaction<'_, Postgres>, run: Uuid) -> Result<(), ApiError> {
    let row: Option<(Uuid,String,String)> = sqlx::query_as("SELECT d.environment_id,d.status,r.status FROM run_deployments d JOIN runs r ON r.id=d.run_id WHERE d.run_id=$1 AND d.status IN ('awaiting_approval','approved','running') AND r.status IN ('succeeded','failed','canceled')")
        .bind(run).fetch_optional(&mut **tx).await?;
    let Some((id, decision, status)) = row else {
        return Ok(());
    };
    let env = lock_environment(tx, id).await?;
    let attempted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM job_attempts a JOIN job_runs j ON j.id=a.job_id WHERE j.run_id=$1)")
        .bind(run).fetch_one(&mut **tx).await?;
    if status == "succeeded" && decision == "running" && env.active_run_id == Some(run) {
        // Successful workspace completion already requires all steps, including health_check.
        sqlx::query("UPDATE deployment_environments SET previous_run_id=current_run_id,current_run_id=$2,active_run_id=NULL,health='healthy',checked_at=now(),generation=generation+1 WHERE id=$1")
            .bind(id).bind(run).execute(&mut **tx).await?;
        sqlx::query(
            "UPDATE run_deployments SET status='succeeded',finished_at=now() WHERE run_id=$1",
        )
        .bind(run)
        .execute(&mut **tx)
        .await?;
    } else if attempted {
        sqlx::query("UPDATE run_deployments SET status='recovery_required',finished_at=now() WHERE run_id=$1").bind(run).execute(&mut **tx).await?;
        sqlx::query("UPDATE deployment_environments SET health='unknown',generation=generation+1 WHERE id=$1").bind(id).execute(&mut **tx).await?;
        // A lost lease cannot prove the external deployment stopped. Keep ownership.
    } else {
        sqlx::query(
            "UPDATE run_deployments SET status='canceled',finished_at=now() WHERE run_id=$1",
        )
        .bind(run)
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE deployment_environments SET active_run_id=NULL,generation=generation+1 WHERE id=$1 AND active_run_id=$2")
            .bind(id).bind(run).execute(&mut **tx).await?;
    }
    sqlx::query(
        "INSERT INTO audit_events(project_id,action,details) VALUES($1,'deployment.finished',$2)",
    )
    .bind(env.project_id)
    .bind(json!({"run_id":run,"environment_id":id,"run_status":status,"attempted":attempted}))
    .execute(&mut **tx)
    .await?;
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recovery {
    generation: i64,
    run_id: Uuid,
    stopped_confirmed: bool,
    note: String,
}
pub async fn recover(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<Recovery>,
) -> Result<Json<Value>, ApiError> {
    note(&input.note)?;
    if !input.stopped_confirmed {
        return Err(ApiError::bad_request(
            "confirm external deployment processes have stopped before releasing ownership",
        ));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM runs WHERE id=$1 FOR UPDATE")
        .bind(input.run_id)
        .execute(&mut *tx)
        .await?;
    let env = lock_environment(&mut tx, id).await?;
    authority(&mut tx, session.user.id, env.project_id).await?;
    let stopped: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM run_deployments WHERE run_id=$1 AND status='recovery_required') AND NOT EXISTS(SELECT 1 FROM job_attempts a JOIN job_runs j ON j.id=a.job_id WHERE j.run_id=$1 AND a.status='running')")
        .bind(input.run_id).fetch_one(&mut *tx).await?;
    if env.generation != input.generation || env.active_run_id != Some(input.run_id) || !stopped {
        return Err(ApiError::conflict(
            "environment changed or deployment has not terminated",
        ));
    }
    sqlx::query("UPDATE run_deployments SET status='resolved',note=$2 WHERE run_id=$1")
        .bind(input.run_id)
        .bind(&input.note)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE deployment_environments SET active_run_id=NULL,generation=generation+1 WHERE id=$1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    store::audit(
        &mut tx,
        session.user.id,
        Some(env.project_id),
        "deployment.recovery_acknowledged",
        json!({"run_id":input.run_id,"environment_id":id,"note":input.note}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rollback {
    generation: i64,
    target_run_id: Uuid,
    request_id: Uuid,
    note: String,
}
pub async fn rollback(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<Rollback>,
) -> Result<Json<store::Run>, ApiError> {
    note(&input.note)?;
    if input.request_id.is_nil() {
        return Err(ApiError::bad_request("request_id required"));
    }
    let env: Environment = sqlx::query_as("SELECT * FROM deployment_environments WHERE id=$1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let pipeline =
        store::pipeline_access(&state, &session.user, env.pipeline_id, Role::Admin).await?;
    let hash = hash_bytes(
        &serde_json::to_vec(&("deployment.rollback", id, &input)).map_err(ApiError::internal)?,
    );
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("{}:{}", session.user.id, input.request_id))
        .execute(&mut *tx)
        .await?;
    let prior: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,request_hash FROM runs WHERE submitted_by=$1 AND request_id=$2")
            .bind(session.user.id)
            .bind(input.request_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((run, previous_hash)) = prior {
        if previous_hash != hash {
            return Err(ApiError::conflict(
                "request_id was used for different inputs",
            ));
        }
        tx.commit().await?;
        return store::run_detail(State(state), Extension(session), Path(run)).await;
    }
    let target: Option<(i64,DbJson<PipelineDefinition>,DbJson<SourceSnapshot>)> = sqlx::query_as("SELECT r.version,r.definition,r.source_snapshot FROM runs r JOIN run_deployments d ON d.run_id=r.id WHERE r.id=$1 AND r.pipeline_id=$2 AND d.environment_id=$3 AND d.status='succeeded' AND r.status='succeeded'")
        .bind(input.target_run_id).bind(pipeline.id).bind(id).fetch_optional(&mut *tx).await?;
    let (version, definition, snapshot) = target.ok_or_else(|| {
        ApiError::bad_request("select a successful deployment in this environment")
    })?;
    let bindings = crate::artifact_inputs::load(&mut tx, input.target_run_id).await?;
    let source = bindings
        .first()
        .ok_or_else(|| ApiError::conflict("rollback artifact lineage unavailable"))?
        .source_run_id;
    let request = store::RunInput {
        version: Some(version),
        changelist: snapshot.changelist,
        source_run_id: Some(source),
        request_id: input.request_id,
    };
    let run = store::enqueue_run(
        &mut tx,
        &pipeline,
        &session.user,
        &request,
        &definition,
        &snapshot,
        &hash,
        Some(input.target_run_id),
    )
    .await?;
    let copied = crate::artifact_inputs::load(&mut tx, run.id).await?;
    if copied != bindings {
        return Err(ApiError::conflict("rollback artifact identity changed"));
    }
    let current = lock_environment(&mut tx, id).await?;
    authority(&mut tx, session.user.id, env.project_id).await?;
    if current.generation != input.generation
        || current.active_run_id.is_some()
        || ![current.current_run_id, current.previous_run_id].contains(&Some(input.target_run_id))
    {
        return Err(ApiError::conflict(
            "environment changed; release failed ownership and select current or previous successful deployment",
        ));
    }
    store::audit(&mut tx,session.user.id,Some(env.project_id),"deployment.rollback_requested",json!({"run_id":run.id,"target_run_id":input.target_run_id,"environment_id":id,"note":input.note})).await?;
    tx.commit().await?;
    Ok(Json(run))
}
