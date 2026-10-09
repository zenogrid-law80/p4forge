//! Build artifacts live outside workspaces. Only verified files of successful runs are visible.
use crate::{AppState, agents, auth::Session, error::ApiError, store};
use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, State},
    http::header,
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use p4forge_domain::{MAX_ARTIFACT_FILE, MAX_ARTIFACT_TOTAL, PipelineDefinition, hash_bytes};
use p4forge_protocol::{ARTIFACT_CHUNK, ArtifactChunk, ArtifactManifest, ArtifactUpload};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Postgres, Transaction, types::Json as DbJson};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

#[derive(FromRow, Serialize)]
pub struct Artifact {
    pub id: Uuid,
    pub name: String,
    pub path: String,
    pub size: i64,
    pub sha256: String,
    pub uploaded_size: i64,
    pub status: String,
    pub purge_requested_at: Option<chrono::DateTime<chrono::Utc>>,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}
impl Artifact {
    fn progress(&self) -> ArtifactUpload {
        ArtifactUpload {
            id: self.id,
            name: self.name.clone(),
            uploaded_size: self.uploaded_size,
            status: self.status.clone(),
        }
    }
}
const COLUMNS: &str = "id,name,path,size,sha256,uploaded_size,status,purge_requested_at,deleted_at";
pub(crate) fn blob(state: &AppState, id: Uuid) -> std::path::PathBuf {
    state.artifact_dir.join(format!("{id}.blob"))
}

pub async fn manifest(
    State(state): State<AppState>,
    Extension(agent): Extension<agents::Agent>,
    Path(id): Path<Uuid>,
    Json(input): Json<ArtifactManifest>,
) -> Result<Json<Vec<ArtifactUpload>>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let run = agents::artifact_attempt(&mut tx, agent.id, id, input.fence).await?;
    let definition: DbJson<PipelineDefinition> =
        sqlx::query_scalar("SELECT definition FROM runs WHERE id=$1")
            .bind(run)
            .fetch_one(&mut *tx)
            .await?;
    if input.files.is_empty() || input.files.len() != definition.artifacts.len() {
        return Err(ApiError::bad_request(
            "artifact manifest must match the frozen pipeline",
        ));
    }
    let mut names = std::collections::HashSet::new();
    let mut total = 0;
    for file in &input.files {
        if !names.insert(&file.name)
            || !definition
                .artifacts
                .iter()
                .any(|s| s.name == file.name && s.path == file.path)
            || file.size > MAX_ARTIFACT_FILE
            || file.sha256.len() != 64
            || !file
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ApiError::bad_request("invalid artifact manifest"));
        }
        total += file.size;
    }
    if total > MAX_ARTIFACT_TOTAL {
        return Err(ApiError::bad_request("artifact total exceeds 256 MiB"));
    }
    let hash = hash_bytes(&serde_json::to_vec(&input.files).map_err(ApiError::internal)?);
    let previous: Option<String> =
        sqlx::query_scalar("SELECT definition_hash FROM artifact_manifests WHERE attempt_id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(previous) = previous {
        if previous != hash {
            return Err(ApiError::conflict("artifact manifest changed"));
        }
    } else {
        sqlx::query(
            "INSERT INTO artifact_manifests(attempt_id,run_id,definition_hash) VALUES($1,$2,$3)",
        )
        .bind(id)
        .bind(run)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
        for file in input.files {
            sqlx::query("INSERT INTO artifacts(id,attempt_id,run_id,name,path,size,sha256) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(Uuid::new_v4()).bind(id).bind(run).bind(file.name).bind(file.path).bind(file.size as i64).bind(file.sha256).execute(&mut *tx).await?;
        }
    }
    let rows: Vec<Artifact> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM artifacts WHERE attempt_id=$1 ORDER BY name"
    ))
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(rows.iter().map(Artifact::progress).collect()))
}

pub async fn chunk(
    State(state): State<AppState>,
    Extension(agent): Extension<agents::Agent>,
    Path((attempt, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<ArtifactChunk>,
) -> Result<Json<ArtifactUpload>, ApiError> {
    let data = STANDARD
        .decode(input.data)
        .map_err(|_| ApiError::bad_request("invalid artifact encoding"))?;
    if data.len() > ARTIFACT_CHUNK {
        return Err(ApiError::bad_request("artifact chunk too large"));
    }
    let mut tx = state.pool.begin().await?;
    agents::artifact_attempt(&mut tx, agent.id, attempt, input.fence).await?;
    let mut row: Artifact = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM artifacts WHERE id=$1 AND attempt_id=$2 FOR UPDATE"
    ))
    .bind(id)
    .bind(attempt)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let end = input
        .offset
        .checked_add(data.len() as u64)
        .ok_or_else(|| ApiError::bad_request("invalid offset"))?;
    if input.offset > row.uploaded_size as u64
        || end > row.size as u64
        || (data.is_empty() && row.size != 0)
    {
        return Err(ApiError::conflict("invalid artifact offset or length"));
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true).write(true);
    // A committed offset must never silently recreate a missing file.
    options.create(row.uploaded_size == 0 && row.status == "pending");
    let mut file = options
        .open(blob(&state, id))
        .await
        .map_err(ApiError::internal)?;
    if file.metadata().await.map_err(ApiError::internal)?.len() < row.uploaded_size as u64 {
        return Err(ApiError::unavailable("artifact storage is incomplete"));
    }
    if input.offset < row.uploaded_size as u64 || row.status == "verified" {
        if end > row.uploaded_size as u64 {
            return Err(ApiError::conflict("overlapping artifact chunk"));
        }
        file.seek(std::io::SeekFrom::Start(input.offset))
            .await
            .map_err(ApiError::internal)?;
        let mut saved = vec![0; data.len()];
        file.read_exact(&mut saved)
            .await
            .map_err(ApiError::internal)?;
        if saved != data {
            return Err(ApiError::conflict("artifact chunk changed"));
        }
        return Ok(Json(row.progress()));
    }
    // Discard bytes written by a request whose SQL transaction never committed.
    file.set_len(row.uploaded_size as u64)
        .await
        .map_err(ApiError::internal)?;
    file.seek(std::io::SeekFrom::Start(input.offset))
        .await
        .map_err(ApiError::internal)?;
    file.write_all(&data).await.map_err(ApiError::internal)?;
    file.sync_all().await.map_err(ApiError::internal)?;
    row.uploaded_size = end as i64;
    if row.uploaded_size == row.size {
        file.rewind().await.map_err(ApiError::internal)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let n = file.read(&mut buffer).await.map_err(ApiError::internal)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if format!("{:x}", hash.finalize()) != row.sha256 {
            return Err(ApiError::conflict("artifact SHA-256 mismatch"));
        }
        row.status = "verified".into();
    }
    sqlx::query("UPDATE artifacts SET uploaded_size=$2,status=$3 WHERE id=$1")
        .bind(id)
        .bind(row.uploaded_size)
        .bind(&row.status)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(row.progress()))
}

pub(crate) async fn require_complete(
    tx: &mut Transaction<'_, Postgres>,
    run: Uuid,
    attempt: Uuid,
) -> Result<(), ApiError> {
    let missing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jsonb_array_elements(COALESCE(definition->'artifacts','[]'::jsonb)) s WHERE NOT EXISTS(SELECT 1 FROM artifacts a WHERE a.run_id=$1 AND a.attempt_id=$2 AND a.name=s->>'name' AND a.path=s->>'path' AND a.status='verified' AND a.uploaded_size=a.size)) FROM runs WHERE id=$1").bind(run).bind(attempt).fetch_one(&mut **tx).await?;
    if missing {
        return Err(ApiError::conflict("artifacts have not all been verified"));
    }
    Ok(())
}

pub async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(run): Path<Uuid>,
) -> Result<Json<Vec<Artifact>>, ApiError> {
    let detail = store::run_detail(State(state.clone()), Extension(session), Path(run))
        .await?
        .0;
    if detail.status != "succeeded" {
        return Ok(Json(Vec::new()));
    }
    let rows=sqlx::query_as("SELECT a.id,a.name,a.path,a.size,a.sha256,a.uploaded_size,a.status,a.purge_requested_at,a.deleted_at FROM artifacts a JOIN job_attempts t ON t.id=a.attempt_id WHERE a.run_id=$1 AND a.status='verified' AND t.status='succeeded' ORDER BY a.name").bind(run).fetch_all(&state.pool).await?;
    Ok(Json(rows))
}

pub async fn download(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path((run, id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let rows = list(State(state.clone()), Extension(session), Path(run))
        .await?
        .0;
    let artifact = rows
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(ApiError::not_found)?;
    // Open under a shared row lock so deletion cannot unlink between lookup and open.
    let mut tx = state.pool.begin().await?;
    let available: bool = sqlx::query_scalar("SELECT purge_requested_at IS NULL AND deleted_at IS NULL FROM artifacts WHERE id=$1 FOR SHARE")
        .bind(id).fetch_one(&mut *tx).await?;
    if !available {
        return Err(ApiError(
            axum::http::StatusCode::GONE,
            "artifact removed by retention policy".into(),
        ));
    }
    let file = tokio::fs::File::open(blob(&state, id))
        .await
        .map_err(ApiError::internal)?;
    if file.metadata().await.map_err(ApiError::internal)?.len() != artifact.size as u64 {
        return Err(ApiError::unavailable("artifact storage is incomplete"));
    }
    tx.commit().await?;
    let stream = futures_util::stream::try_unfold(file, |mut file| async move {
        let mut buffer = vec![0; 64 * 1024];
        let n = file.read(&mut buffer).await?;
        buffer.truncate(n);
        Ok::<_, std::io::Error>(if n == 0 { None } else { Some((buffer, file)) })
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, artifact.size)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", artifact.name),
        )
        .header(header::ETAG, format!("\"{}\"", artifact.sha256))
        .body(Body::from_stream(stream))
        .map_err(ApiError::internal)
}
