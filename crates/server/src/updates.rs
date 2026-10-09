use crate::{AppState, agents::Agent, auth::Session, error::ApiError, store};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::header,
};
use chrono::{DateTime, Utc};
use p4forge_protocol::{MAX_AGENT_BINARY, UpdateOffer, UpdatePoll, UpdateResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Serialize, FromRow)]
pub struct Release {
    pub id: Uuid,
    pub version: String,
    pub os: String,
    pub arch: String,
    pub sha256: String,
    pub size: i64,
    pub created_at: DateTime<Utc>,
}
const RELEASE_COLUMNS: &str = "id,version,os,arch,sha256,size,created_at";
#[derive(Serialize, FromRow)]
pub struct Update {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub release_id: Uuid,
    pub previous_sha256: String,
    pub previous_draining: bool,
    pub status: String,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
fn hash_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub async fn register(
    pool: &PgPool,
    version: &str,
    os: &str,
    arch: &str,
    binary: Vec<u8>,
) -> anyhow::Result<Release> {
    anyhow::ensure!(
        !version.is_empty()
            && version.len() <= 80
            && version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-+".contains(&c)),
        "invalid release version"
    );
    anyhow::ensure!(
        matches!(
            (os, arch),
            ("macos", "aarch64" | "x86_64") | ("windows", "x86_64")
        ),
        "unsupported native Agent platform"
    );
    anyhow::ensure!(
        !binary.is_empty() && binary.len() <= MAX_AGENT_BINARY,
        "binary exceeds 64 MiB or is empty"
    );
    let sha256 = format!("{:x}", Sha256::digest(&binary));
    // Binaries are imported by a local operator; remote APIs cannot supply URLs or commands.
    let mut tx = pool.begin().await?;
    let release = sqlx::query_as::<_,Release>(&format!("INSERT INTO agent_releases(id,version,os,arch,sha256,size,payload) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(os,arch,sha256) DO UPDATE SET sha256=EXCLUDED.sha256 RETURNING {RELEASE_COLUMNS}"))
        .bind(Uuid::new_v4()).bind(version).bind(os).bind(arch).bind(sha256).bind(binary.len() as i64).bind(binary).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO audit_events(action,details) VALUES('agent.release.registered',$1)")
        .bind(json!({"release_id":release.id,"sha256":release.sha256,"version":release.version,"os":os,"arch":arch})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(release)
}
pub async fn releases(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Vec<Release>>, ApiError> {
    store::require_admin(&session.user)?;
    Ok(Json(
        sqlx::query_as(&format!(
            "SELECT {RELEASE_COLUMNS} FROM agent_releases ORDER BY created_at DESC LIMIT 100"
        ))
        .fetch_all(&state.pool)
        .await?,
    ))
}
pub async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Vec<Update>>, ApiError> {
    store::require_admin(&session.user)?;
    Ok(Json(
        sqlx::query_as("SELECT * FROM agent_updates ORDER BY created_at DESC LIMIT 200")
            .fetch_all(&state.pool)
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestUpdate {
    pub release_id: Uuid,
}
pub async fn request(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<RequestUpdate>,
) -> Result<Json<Update>, ApiError> {
    store::require_admin(&session.user)?;
    let mut tx = state.pool.begin().await?;
    let agent: Agent = sqlx::query_as("SELECT * FROM agents WHERE id=$1 AND active FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if !agent.self_patch_ready
        || agent
            .last_seen_at
            .is_none_or(|seen| (Utc::now() - seen).num_seconds() > 30)
    {
        return Err(ApiError::conflict(
            "install the self-patch module and connect this Agent first",
        ));
    }
    let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_updates WHERE agent_id=$1 AND status IN ('requested','staged'))").bind(id).fetch_one(&mut *tx).await?;
    if pending {
        return Err(ApiError::conflict("an Agent update is already pending"));
    }
    let release: Release = sqlx::query_as(&format!(
        "SELECT {RELEASE_COLUMNS} FROM agent_releases WHERE id=$1"
    ))
    .bind(input.release_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let previous = agent
        .binary_sha256
        .filter(|hash| hash_valid(hash))
        .ok_or_else(|| ApiError::conflict("Agent binary identity is unavailable"))?;
    if agent.os != release.os || agent.arch != release.arch || previous == release.sha256 {
        return Err(ApiError::bad_request(
            "release must match the Agent platform and differ from its installed binary",
        ));
    }
    let update: Update = sqlx::query_as("INSERT INTO agent_updates(id,agent_id,release_id,previous_sha256,previous_draining,requested_by) VALUES($1,$2,$3,$4,$5,$6) RETURNING *")
        .bind(Uuid::new_v4()).bind(id).bind(release.id).bind(previous).bind(agent.draining).bind(session.user.id).fetch_one(&mut *tx).await?;
    sqlx::query("UPDATE agents SET draining=true WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    store::audit(
        &mut tx,
        session.user.id,
        None,
        "agent.update.requested",
        json!({"update_id":update.id,"agent_id":id,"release_id":release.id}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(update))
}
pub async fn cancel(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Update>, ApiError> {
    store::require_admin(&session.user)?;
    let agent_id: Uuid = sqlx::query_scalar("SELECT agent_id FROM agent_updates WHERE id=$1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let mut tx = state.pool.begin().await?;
    let _: Uuid = sqlx::query_scalar("SELECT id FROM agents WHERE id=$1 FOR UPDATE")
        .bind(agent_id)
        .fetch_one(&mut *tx)
        .await?;
    let update: Update = sqlx::query_as("UPDATE agent_updates SET status='cancelled',updated_at=now() WHERE id=$1 AND status='requested' RETURNING *").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::conflict("only an update that has not started staging can be cancelled"))?;
    sqlx::query("UPDATE agents SET draining=$2 WHERE id=$1")
        .bind(agent_id)
        .bind(update.previous_draining)
        .execute(&mut *tx)
        .await?;
    store::audit(
        &mut tx,
        session.user.id,
        None,
        "agent.update.cancelled",
        json!({"update_id":id,"agent_id":agent_id}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(update))
}
pub async fn poll(
    State(state): State<AppState>,
    Extension(identity): Extension<Agent>,
    Json(input): Json<UpdatePoll>,
) -> Result<Json<UpdateResponse>, ApiError> {
    if !hash_valid(&input.sha256) || input.version.is_empty() || input.version.len() > 80 {
        return Err(ApiError::bad_request("invalid installed Agent identity"));
    }
    let mut tx = state.pool.begin().await?;
    let _: Uuid = sqlx::query_scalar("SELECT id FROM agents WHERE id=$1 AND active FOR UPDATE")
        .bind(identity.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::unauthorized("agent revoked"))?;
    sqlx::query("UPDATE agents SET binary_sha256=$2,agent_version=$3,self_patch_ready=$4,last_seen_at=now() WHERE id=$1")
        .bind(identity.id).bind(&input.sha256).bind(&input.version).bind(input.ready).execute(&mut *tx).await?;
    let mut response = UpdateResponse {
        offer: None,
        healthy_id: None,
        receipt_accepted: false,
    };
    if let Some(receipt) = &input.receipt {
        let update: Update =
            sqlx::query_as("SELECT * FROM agent_updates WHERE id=$1 AND agent_id=$2 FOR UPDATE")
                .bind(receipt.id)
                .bind(identity.id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(ApiError::not_found)?;
        let target: String = sqlx::query_scalar("SELECT sha256 FROM agent_releases WHERE id=$1")
            .bind(update.release_id)
            .fetch_one(&mut *tx)
            .await?;
        let valid = match receipt.status.as_str() {
            "succeeded" => input.sha256 == target && update.status != "requested",
            "rolled_back" | "failed" => input.sha256 == update.previous_sha256,
            _ => false,
        };
        if !valid || receipt.reason.len() > 250 {
            return Err(ApiError::bad_request(
                "invalid update receipt or installed binary",
            ));
        }
        if matches!(update.status.as_str(), "requested" | "staged") {
            sqlx::query(
                "UPDATE agent_updates SET status=$2,reason=$3,updated_at=now() WHERE id=$1",
            )
            .bind(update.id)
            .bind(&receipt.status)
            .bind(&receipt.reason)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE agents SET draining=$2 WHERE id=$1")
                .bind(identity.id)
                .bind(update.previous_draining)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO audit_events(action,details) VALUES('agent.update.completed',$1)").bind(json!({"agent_id":identity.id,"update_id":update.id,"status":receipt.status,"sha256":input.sha256})).execute(&mut *tx).await?;
        } else if update.status != receipt.status {
            return Err(ApiError::conflict(
                "update already has another terminal result",
            ));
        }
        response.receipt_accepted = true;
    }
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM job_attempts WHERE agent_id=$1 AND status='running')",
    )
    .bind(identity.id)
    .fetch_one(&mut *tx)
    .await?;
    let pending: Option<Update> = sqlx::query_as(
        "SELECT * FROM agent_updates WHERE agent_id=$1 AND status IN ('requested','staged')",
    )
    .bind(identity.id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(update) = pending {
        let release: Release = sqlx::query_as(&format!(
            "SELECT {RELEASE_COLUMNS} FROM agent_releases WHERE id=$1"
        ))
        .bind(update.release_id)
        .fetch_one(&mut *tx)
        .await?;
        if update.status == "staged"
            && input.pending_id == Some(update.id)
            && input.sha256 == release.sha256
        {
            response.healthy_id = Some(update.id);
        } else if !busy
            && input.ready
            && input.pending_id.is_none()
            && input.receipt.is_none()
            && input.sha256 == update.previous_sha256
        {
            // Pin staging before returning the offer; cancellation cannot race the native updater.
            sqlx::query("UPDATE agent_updates SET status='staged',updated_at=now() WHERE id=$1")
                .bind(update.id)
                .execute(&mut *tx)
                .await?;
            response.offer = Some(UpdateOffer {
                id: update.id,
                release_id: release.id,
                version: release.version,
                sha256: release.sha256,
                size: release.size,
            });
        }
    }
    tx.commit().await?;
    Ok(Json(response))
}
pub async fn binary(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
    Path(id): Path<Uuid>,
) -> Result<([(header::HeaderName, &'static str); 1], Vec<u8>), ApiError> {
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT r.payload FROM agent_releases r JOIN agent_updates u ON u.release_id=r.id WHERE u.id=$1 AND u.agent_id=$2 AND u.status='staged'").bind(id).bind(agent.id).fetch_optional(&state.pool).await?.ok_or_else(ApiError::not_found)?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes))
}
