//! Frozen, project-scoped artifact dependencies from one successful upstream run.
use crate::{AppState, agents, auth::Session, error::ApiError, store};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use p4forge_domain::{MAX_ARTIFACT_TOTAL, PipelineDefinition};
use p4forge_perforce::SourceSnapshot;
use p4forge_protocol::{ARTIFACT_CHUNK, ArtifactData, ArtifactInput, ArtifactRead};
use sqlx::{Postgres, Transaction, types::Json as DbJson};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;

pub(crate) async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    source: Option<Uuid>,
    definition: &PipelineDefinition,
    snapshot: &SourceSnapshot,
) -> Result<Vec<ArtifactInput>, ApiError> {
    if definition.artifact_inputs.is_empty() {
        if source.is_some() {
            return Err(ApiError::bad_request(
                "pipeline declares no artifact inputs",
            ));
        }
        return Ok(vec![]);
    }
    let source = source
        .ok_or_else(|| ApiError::bad_request("source_run_id is required for artifact inputs"))?;
    // Same lock as retention; it must recheck pins after this transaction commits.
    let parent: Option<(String, bool, DbJson<SourceSnapshot>)> = sqlx::query_as("SELECT r.status,r.cancel_requested,r.source_snapshot FROM runs r JOIN pipelines p ON p.id=r.pipeline_id WHERE r.id=$1 AND p.project_id=$2 FOR UPDATE OF r").bind(source).bind(project).fetch_optional(&mut **tx).await?;
    let (status, canceled, parent_snapshot) = parent.ok_or_else(ApiError::not_found)?;
    if status != "succeeded" || canceled {
        return Err(ApiError::conflict(
            "artifact source run must have succeeded",
        ));
    }
    let published: Option<i64> = sqlx::query_scalar("SELECT changelist FROM run_publications WHERE run_id=$1 AND status IN ('submitted','unchanged')").bind(source).fetch_optional(&mut **tx).await?;
    if snapshot.changelist != published.unwrap_or(parent_snapshot.changelist)
        || snapshot.connection != parent_snapshot.connection
        || snapshot.server_id != parent_snapshot.server_id
    {
        return Err(ApiError::bad_request(
            "artifact source run and checkout must use the same Perforce server and submitted changelist",
        ));
    }
    let mut result = Vec::new();
    for name in &definition.artifact_inputs {
        let rows: Vec<(Uuid,i64,String)> = sqlx::query_as("SELECT a.id,a.size,a.sha256 FROM artifacts a JOIN job_attempts t ON t.id=a.attempt_id WHERE a.run_id=$1 AND a.name=$2 AND a.status='verified' AND t.status='succeeded' AND a.purge_requested_at IS NULL AND a.deleted_at IS NULL FOR SHARE OF a")
            .bind(source).bind(name).fetch_all(&mut **tx).await?;
        if rows.len() != 1 {
            return Err(ApiError::conflict(format!(
                "verified source artifact unavailable: {name}"
            )));
        }
        let (artifact_id, size, sha256) = &rows[0];
        result.push(ArtifactInput {
            artifact_id: *artifact_id,
            source_run_id: source,
            name: name.clone(),
            size: *size as u64,
            sha256: sha256.clone(),
        });
    }
    if result.iter().map(|r| r.size).sum::<u64>() > MAX_ARTIFACT_TOTAL {
        return Err(ApiError::bad_request("artifact inputs exceed 256 MiB"));
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}
pub(crate) async fn bind(
    tx: &mut Transaction<'_, Postgres>,
    run: Uuid,
    inputs: &[ArtifactInput],
) -> Result<(), ApiError> {
    for input in inputs {
        sqlx::query("INSERT INTO run_artifact_inputs(run_id,artifact_id,source_run_id,name,size,sha256) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(run).bind(input.artifact_id).bind(input.source_run_id).bind(&input.name).bind(input.size as i64).bind(&input.sha256).execute(&mut **tx).await?;
    }
    Ok(())
}
pub(crate) async fn load(
    tx: &mut Transaction<'_, Postgres>,
    run: Uuid,
) -> Result<Vec<ArtifactInput>, ApiError> {
    let rows: Vec<DbJson<ArtifactInput>> = sqlx::query_scalar("SELECT jsonb_build_object('artifact_id',artifact_id,'source_run_id',source_run_id,'name',name,'size',size,'sha256',sha256) FROM run_artifact_inputs WHERE run_id=$1 ORDER BY name").bind(run).fetch_all(&mut **tx).await?;
    Ok(rows.into_iter().map(|row| row.0).collect())
}
pub async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(run): Path<Uuid>,
) -> Result<Json<Vec<ArtifactInput>>, ApiError> {
    let _ = store::run_detail(State(state.clone()), Extension(session), Path(run)).await?;
    let mut tx = state.pool.begin().await?;
    Ok(Json(load(&mut tx, run).await?))
}
pub async fn read(
    State(state): State<AppState>,
    Extension(agent): Extension<agents::Agent>,
    Path((attempt, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<ArtifactRead>,
) -> Result<Response, ApiError> {
    let mut tx = state.pool.begin().await?;
    let run = agents::input_attempt(&mut tx, agent.id, attempt, input.fence).await?;
    let bound = load(&mut tx, run)
        .await?
        .into_iter()
        .find(|item| item.artifact_id == id)
        .ok_or_else(ApiError::not_found)?;
    let available: bool = sqlx::query_scalar("SELECT status='verified' AND size=$2 AND sha256=$3 AND purge_requested_at IS NULL AND deleted_at IS NULL FROM artifacts WHERE id=$1 FOR SHARE")
        .bind(id).bind(bound.size as i64).bind(&bound.sha256).fetch_one(&mut *tx).await?;
    if !available {
        return Err(ApiError::conflict("bound artifact is unavailable"));
    }
    if input.offset > bound.size {
        return Err(ApiError::bad_request("artifact offset exceeds size"));
    }
    let mut file = tokio::fs::File::open(crate::artifacts::blob(&state, id))
        .await
        .map_err(|_| ApiError::unavailable("artifact storage unavailable"))?;
    if file.metadata().await.map_err(ApiError::internal)?.len() != bound.size {
        return Err(ApiError::unavailable("artifact storage size mismatch"));
    }
    file.seek(std::io::SeekFrom::Start(input.offset))
        .await
        .map_err(ApiError::internal)?;
    let mut bytes = vec![0; (bound.size - input.offset).min(ARTIFACT_CHUNK as u64) as usize];
    file.read_exact(&mut bytes)
        .await
        .map_err(|_| ApiError::unavailable("artifact storage read failed"))?;
    tx.commit().await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(ArtifactData {
            offset: input.offset,
            data: STANDARD.encode(bytes),
        }),
    )
        .into_response())
}
