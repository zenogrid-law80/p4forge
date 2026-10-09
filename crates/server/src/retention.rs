//! Persist deletion intent before touching disk so interrupted cleanup can safely resume.
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
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone, FromRow, Serialize)]
pub struct Policy {
    pub project_id: Uuid,
    pub generation: i64,
    pub enabled: bool,
    pub retention_days: i32,
    pub keep_successful_runs: i32,
    pub updated_at: Option<DateTime<Utc>>,
    pub updated_by: Option<Uuid>,
    pub last_sweep_at: Option<DateTime<Utc>>,
}
impl Policy {
    fn default_for(project: Uuid) -> Self {
        Self {
            project_id: project,
            generation: 0,
            enabled: false,
            retention_days: 30,
            keep_successful_runs: 5,
            updated_at: None,
            updated_by: None,
            last_sweep_at: None,
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyInput {
    pub generation: i64,
    pub enabled: bool,
    pub retention_days: i32,
    pub keep_successful_runs: i32,
}
impl PolicyInput {
    fn validate(&self) -> Result<(), ApiError> {
        if self.generation < 0
            || !(1..=3650).contains(&self.retention_days)
            || !(1..=1000).contains(&self.keep_successful_runs)
        {
            return Err(ApiError::bad_request(
                "retention days must be 1..3650 and protected successful runs 1..1000",
            ));
        }
        Ok(())
    }
}
#[derive(FromRow, Serialize)]
pub struct Candidate {
    pub id: Uuid,
    pub run_id: Uuid,
    pub attempt_id: Uuid,
    pub pipeline_name: String,
    pub name: String,
    pub uploaded_size: i64,
    pub reason: String,
    pub total_count: i64,
    pub total_bytes: i64,
}
// Rank successful runs with verified artifacts, including tombstones. Runs without
// artifacts must not evict the last usable build. Terminal times, not creation times,
// define the retention window. A live attempt anywhere in the run always protects it.
const ELIGIBLE: &str = r#"
WITH ranked AS (
    SELECT r.id,row_number() OVER(PARTITION BY r.pipeline_id ORDER BY r.finished_at DESC,r.id DESC) AS position
    FROM runs r JOIN pipelines p ON p.id=r.pipeline_id
    WHERE p.project_id=$1 AND r.status='succeeded' AND r.finished_at IS NOT NULL
      AND EXISTS(SELECT 1 FROM artifacts a JOIN job_attempts t ON t.id=a.attempt_id
                 WHERE a.run_id=r.id AND a.status='verified' AND t.status='succeeded')
), eligible AS (
    SELECT a.id,a.run_id,a.attempt_id,p.name AS pipeline_name,a.name,a.uploaded_size,
      CASE WHEN r.status='succeeded' AND t.status='succeeded' AND a.status='verified' THEN 'expired' ELSE 'incomplete' END AS reason
    FROM artifacts a JOIN runs r ON r.id=a.run_id JOIN pipelines p ON p.id=r.pipeline_id
    JOIN job_attempts t ON t.id=a.attempt_id LEFT JOIN ranked ON ranked.id=r.id
    WHERE p.project_id=$1 AND a.purge_requested_at IS NULL AND a.deleted_at IS NULL
      AND r.status IN ('succeeded','failed','canceled') AND r.finished_at IS NOT NULL
      AND t.status IN ('succeeded','failed','canceled','lost') AND t.finished_at IS NOT NULL
      AND NOT EXISTS(SELECT 1 FROM job_attempts live JOIN job_runs j ON j.id=live.job_id WHERE j.run_id=r.id AND live.status='running')
      AND (
        (r.status='succeeded' AND t.status='succeeded' AND a.status='verified'
          AND ranked.position>$3 AND r.finished_at<now()-($2::integer*interval '1 day'))
        OR ((r.status<>'succeeded' OR t.status<>'succeeded' OR a.status<>'verified')
          AND GREATEST(r.finished_at,t.finished_at,a.created_at)<now()-interval '24 hours')
      )
      AND NOT EXISTS (
        SELECT 1 FROM run_artifact_inputs b JOIN runs consumer ON consumer.id=b.run_id
        WHERE b.artifact_id=a.id AND (consumer.status IN ('queued','running')
          OR EXISTS(SELECT 1 FROM deployment_environments e WHERE consumer.id IN (e.current_run_id,e.previous_run_id,e.active_run_id))
          OR EXISTS(SELECT 1 FROM job_attempts live JOIN job_runs j ON j.id=live.job_id WHERE j.run_id=consumer.id AND live.status='running'))
      )
      AND NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(COALESCE(r.definition->'on_success','[]'::jsonb)) target
        JOIN pipeline_revisions v ON v.pipeline_id=(target->>'pipeline_id')::uuid AND v.version=(target->>'version')::bigint
        LEFT JOIN run_followups f ON f.parent_run_id=r.id AND f.pipeline_id=v.pipeline_id
        WHERE COALESCE(v.definition->'artifact_inputs','[]'::jsonb) ? a.name
          AND (f.id IS NULL OR f.status='pending') AND NOT r.cancel_requested
      )
      AND ($4::uuid IS NULL OR a.id=$4)
)
SELECT *,count(*) OVER() AS total_count,COALESCE(sum(uploaded_size) OVER(),0)::bigint AS total_bytes FROM eligible
ORDER BY run_id,id LIMIT 100
"#;
async fn candidates(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    days: i32,
    keep: i32,
    id: Option<Uuid>,
) -> Result<Vec<Candidate>, ApiError> {
    Ok(sqlx::query_as(ELIGIBLE)
        .bind(project)
        .bind(days)
        .bind(keep)
        .bind(id)
        .fetch_all(&mut **tx)
        .await?)
}
async fn policy(state: &AppState, project: Uuid) -> Result<Policy, ApiError> {
    Ok(
        sqlx::query_as("SELECT * FROM artifact_retention_policies WHERE project_id=$1")
            .bind(project)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| Policy::default_for(project)),
    )
}
pub async fn get(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Viewer).await?;
    let policy = policy(&state, project).await?;
    let summary:Value=sqlx::query_scalar(r#"SELECT jsonb_build_object(
      'stored_files',count(*) FILTER(WHERE a.deleted_at IS NULL AND a.purge_requested_at IS NULL),
      'stored_bytes',COALESCE(sum(a.uploaded_size) FILTER(WHERE a.deleted_at IS NULL AND a.purge_requested_at IS NULL),0),
      'pending_files',count(*) FILTER(WHERE a.purge_requested_at IS NOT NULL AND a.deleted_at IS NULL),
      'failed_deletions',count(*) FILTER(WHERE a.deleted_at IS NULL AND a.purge_error IS NOT NULL),
      'deleted_files',count(*) FILTER(WHERE a.deleted_at IS NOT NULL))
      FROM artifacts a JOIN runs r ON r.id=a.run_id JOIN pipelines p ON p.id=r.pipeline_id WHERE p.project_id=$1"#).bind(project).fetch_one(&state.pool).await?;
    Ok(Json(json!({"policy":policy,"summary":summary})))
}
pub async fn preview(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Json(input): Json<PolicyInput>,
) -> Result<Json<Value>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Admin).await?;
    input.validate()?;
    let mut tx = state.pool.begin().await?;
    let rows = if input.enabled {
        candidates(
            &mut tx,
            project,
            input.retention_days,
            input.keep_successful_runs,
            None,
        )
        .await?
    } else {
        Vec::new()
    };
    Ok(Json(
        json!({"count":rows.first().map_or(0,|r|r.total_count),"bytes":rows.first().map_or(0,|r|r.total_bytes),"files":rows}),
    ))
}
pub async fn save(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Json(input): Json<PolicyInput>,
) -> Result<Json<Policy>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Admin).await?;
    input.validate()?;
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO artifact_retention_policies(project_id) VALUES($1) ON CONFLICT DO NOTHING",
    )
    .bind(project)
    .execute(&mut *tx)
    .await?;
    let previous: Policy =
        sqlx::query_as("SELECT * FROM artifact_retention_policies WHERE project_id=$1 FOR UPDATE")
            .bind(project)
            .fetch_one(&mut *tx)
            .await?;
    if previous.generation != input.generation {
        return Err(ApiError::conflict(
            "retention policy changed; refresh before saving",
        ));
    }
    let policy:Policy=sqlx::query_as("UPDATE artifact_retention_policies SET enabled=$2,retention_days=$3,keep_successful_runs=$4,generation=generation+1,updated_at=now(),updated_by=$5 WHERE project_id=$1 RETURNING *")
        .bind(project).bind(input.enabled).bind(input.retention_days).bind(input.keep_successful_runs).bind(session.user.id).fetch_one(&mut *tx).await?;
    store::audit(
        &mut tx,
        session.user.id,
        Some(project),
        "artifact.retention_policy",
        json!({"before":previous,"after":policy}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(policy))
}

/// Policy lock serializes saves and marking. Run -> attempt -> artifact follows upload lock order.
async fn schedule(state: &AppState, project: Uuid) -> Result<(), ApiError> {
    let mut tx = state.pool.begin().await?;
    let policy:Option<Policy>=sqlx::query_as("SELECT * FROM artifact_retention_policies WHERE project_id=$1 AND enabled FOR UPDATE SKIP LOCKED").bind(project).fetch_optional(&mut *tx).await?;
    let Some(policy) = policy else { return Ok(()) };
    let rows = candidates(
        &mut tx,
        project,
        policy.retention_days,
        policy.keep_successful_runs,
        None,
    )
    .await?;
    for row in rows {
        sqlx::query("SELECT id FROM runs WHERE id=$1 FOR UPDATE")
            .bind(row.run_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT id FROM job_attempts WHERE id=$1 FOR UPDATE")
            .bind(row.attempt_id)
            .execute(&mut *tx)
            .await?;
        // Re-read eligibility after acquiring the run lock: never use a stale preview as authority.
        if candidates(
            &mut tx,
            project,
            policy.retention_days,
            policy.keep_successful_runs,
            Some(row.id),
        )
        .await?
        .is_empty()
        {
            continue;
        }
        let changed=sqlx::query("UPDATE artifacts SET purge_requested_at=now(),purge_reason=$2,purge_generation=$3 WHERE id=$1 AND purge_requested_at IS NULL AND deleted_at IS NULL")
            .bind(row.id).bind(&row.reason).bind(policy.generation).execute(&mut *tx).await?.rows_affected();
        if changed == 1 {
            sqlx::query("INSERT INTO audit_events(user_id,project_id,action,details) VALUES($1,$2,'artifact.retention_scheduled',$3)")
                .bind(policy.updated_by).bind(project).bind(json!({"artifact_id":row.id,"run_id":row.run_id,"name":row.name,"reason":row.reason,"policy_generation":policy.generation})).execute(&mut *tx).await?;
        }
    }
    sqlx::query("UPDATE artifact_retention_policies SET last_sweep_at=now() WHERE project_id=$1")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Filesystem removal is idempotent. Missing files are expected after a crash between
/// unlink and SQL commit. Failed removes retain intent and retry on the next sweep.
async fn purge(state: &AppState) -> Result<u64, ApiError> {
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM artifacts WHERE purge_requested_at IS NOT NULL AND deleted_at IS NULL ORDER BY purge_last_attempt_at NULLS FIRST,id LIMIT 100").fetch_all(&state.pool).await?;
    let mut removed = 0;
    for id in ids {
        let mut tx = state.pool.begin().await?;
        let row:Option<(Uuid,String,String)>=sqlx::query_as("SELECT run_id,name,purge_reason FROM artifacts WHERE id=$1 AND purge_requested_at IS NOT NULL AND deleted_at IS NULL FOR UPDATE SKIP LOCKED").bind(id).fetch_optional(&mut *tx).await?;
        let Some((run, name, reason)) = row else {
            continue;
        };
        match remove_blob(state, id).await {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(artifact_id=%id,%error,"artifact removal will be retried");
                sqlx::query("UPDATE artifacts SET purge_last_attempt_at=now(),purge_error='storage removal failed; retry pending' WHERE id=$1").bind(id).execute(&mut *tx).await?;
                tx.commit().await?;
                continue;
            }
        }
        sqlx::query("UPDATE artifacts SET deleted_at=now(),purge_last_attempt_at=now(),purge_error=NULL WHERE id=$1").bind(id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO audit_events(project_id,action,details) SELECT p.project_id,'artifact.deleted',$2 FROM runs r JOIN pipelines p ON p.id=r.pipeline_id WHERE r.id=$1")
            .bind(run).bind(json!({"artifact_id":id,"run_id":run,"name":name,"reason":reason})).execute(&mut *tx).await?;
        tx.commit().await?;
        removed += 1;
    }
    Ok(removed)
}

async fn remove_blob(state: &AppState, id: Uuid) -> std::io::Result<()> {
    // Do not mistake an unavailable storage directory for an already removed blob.
    if !tokio::fs::metadata(&state.artifact_dir).await?.is_dir() {
        return Err(std::io::Error::other("artifact storage is not a directory"));
    }
    match tokio::fs::remove_file(crate::artifacts::blob(state, id)).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    tokio::fs::File::open(&state.artifact_dir)
        .await?
        .sync_all()
        .await?;
    Ok(())
}

pub async fn tick(state: &AppState) -> Result<u64, ApiError> {
    let projects:Vec<Uuid>=sqlx::query_scalar("SELECT project_id FROM artifact_retention_policies WHERE enabled ORDER BY last_sweep_at NULLS FIRST,project_id LIMIT 32").fetch_all(&state.pool).await?;
    for project in projects {
        if let Err(error) = schedule(state, project).await {
            tracing::error!(project_id=%project,error=%error.1,"artifact retention scheduling failed");
        }
    }
    // A policy disable stops NEW intents. Already committed removals must finish,
    // including after a restart, because their files may already have been unlinked.
    purge(state).await
}
