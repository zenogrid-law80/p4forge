//! Durable submitted-change polling. Cursor, event, run and jobs commit together.
use crate::{
    AppState,
    auth::{Session, User},
    error::ApiError,
    store::{self, Role},
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use chrono::{DateTime, Utc};
use p4forge_domain::{PipelineDefinition, hash_bytes};
use p4forge_perforce::{Connection, Root};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction, types::Json as DbJson};
use uuid::Uuid;

#[derive(FromRow, Serialize)]
pub struct Trigger {
    pipeline_id: Uuid,
    version: i64,
    generation: i64,
    enabled: bool,
    includes: DbJson<Vec<String>>,
    excludes: DbJson<Vec<String>>,
    configured_by: Uuid,
    #[serde(skip)]
    source_binding: Value,
    server_id: String,
    cursor: i64,
    checked_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    updated_at: DateTime<Utc>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    generation: i64,
    version: i64,
    enabled: bool,
    includes: Vec<String>,
    excludes: Vec<String>,
    #[serde(default)]
    reset_to_head: bool,
}

// Deliberately small filter language: exact depot file, or a directory ending /... .
// Perforce matching is case-sensitive here; exclusions always win, including deletes.
fn validate_paths(paths: &[String], required: bool) -> Result<(), ApiError> {
    if paths.len() > 32 || required && paths.is_empty() {
        return Err(ApiError::bad_request(
            "include needs 1..32 paths; exclude allows 0..32",
        ));
    }
    for path in paths {
        let base = path.strip_suffix("/...").unwrap_or(path);
        let valid = base.strip_prefix("//").is_some_and(|value| {
            value.split('/').count() >= 2
                && value
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
        });
        if !valid
            || path.len() > 512
            || base.contains("...")
            || base
                .chars()
                .any(|c| c.is_control() || "\\@#%*\"?:<>|".contains(c))
        {
            return Err(ApiError::bad_request(
                "use an exact //depot/path/file or //depot/path/...; wildcards other than trailing /... are unsupported",
            ));
        }
    }
    Ok(())
}
fn matches(path: &str, pattern: &str) -> bool {
    pattern
        .strip_suffix("...")
        .filter(|_| pattern.ends_with("/..."))
        .map_or(path == pattern, |prefix| path.starts_with(prefix))
}
fn selected(path: &str, includes: &[String], excludes: &[String]) -> bool {
    includes.iter().any(|p| matches(path, p)) && !excludes.iter().any(|p| matches(path, p))
}
async fn source(
    state: &AppState,
    pipeline: Uuid,
    version: i64,
) -> Result<(PipelineDefinition, Connection, Root, Value), ApiError> {
    type SourceRow = (
        DbJson<PipelineDefinition>,
        Uuid,
        DbJson<Connection>,
        DbJson<Root>,
    );
    let row: Option<SourceRow> = sqlx::query_as(
        "SELECT v.definition,r.connection_id,c.definition,r.definition FROM pipeline_revisions v JOIN pipelines p ON p.id=v.pipeline_id JOIN perforce_roots r ON r.id=(v.definition->>'source_root_id')::uuid AND r.project_id=p.project_id JOIN perforce_connections c ON c.id=r.connection_id WHERE v.pipeline_id=$1 AND v.version=$2")
        .bind(pipeline).bind(version).fetch_optional(&state.pool).await?;
    let (definition, connection_id, connection, root) =
        row.ok_or_else(|| ApiError::bad_request("select a published version with a valid source"))?;
    let binding = json!({"root_id":definition.source_root_id,"connection_id":connection_id,"connection":connection.0,"root":root.0});
    Ok((definition.0, connection.0, root.0, binding))
}
async fn identity(state: &AppState, connection: &Connection) -> Result<String, ApiError> {
    let info = state.perforce.info(connection).await.map_err(|_| {
        ApiError::unavailable("Perforce authentication or server check failed; cursor preserved")
    })?;
    info.get("ServerID")
        .or_else(|| info.get("serverID"))
        .or_else(|| info.get("serverAddress"))
        .filter(|s| !s.is_empty())
        .cloned()
        .ok_or_else(|| ApiError::unavailable("Perforce server identity is unavailable"))
}
async fn change_number(
    state: &AppState,
    connection: &Connection,
    after: Option<i64>,
) -> Result<Option<i64>, ApiError> {
    let lower = after
        .map(|v| {
            v.checked_add(1)
                .ok_or_else(|| ApiError::conflict("changelist cursor overflow"))
        })
        .transpose()?
        .map(|v| v.to_string());
    // One record per transaction: do not collapse repeated -ztag keys from multiple records.
    let mut args = vec!["changes", "-s", "submitted", "-m", "1"];
    if let Some(lower) = &lower {
        args.extend(["-r", "-e", lower]);
    }
    let result = state
        .perforce
        .tagged(connection, &args)
        .await
        .map_err(|_| {
            ApiError::unavailable("Perforce submitted-change scan failed; cursor preserved")
        })?;
    if result.is_empty() {
        return Ok(None);
    }
    let number = result
        .get("change")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 0 && after.is_none_or(|a| *n > a))
        .ok_or_else(|| {
            ApiError::unavailable("Perforce returned an invalid submitted change; cursor preserved")
        })?;
    Ok(Some(number))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    store::pipeline_access(&state, &session.user, id, Role::Viewer).await?;
    let trigger: Option<Trigger> =
        sqlx::query_as("SELECT * FROM pipeline_triggers WHERE pipeline_id=$1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    let events: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('changelist',e.changelist,'generation',e.generation,'version',e.version,'outcome',e.outcome,'run_id',e.run_id,'status',r.status,'created_at',e.created_at) FROM pipeline_trigger_events e LEFT JOIN runs r ON r.id=e.run_id WHERE e.pipeline_id=$1 ORDER BY e.created_at DESC,e.changelist DESC LIMIT 20").bind(id).fetch_all(&state.pool).await?;
    Ok(Json(json!({"trigger":trigger,"events":events})))
}

pub async fn save(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<Save>,
) -> Result<Json<Value>, ApiError> {
    let pipeline = store::pipeline_access(&state, &session.user, id, Role::Admin).await?;
    validate_paths(&input.includes, true)?;
    validate_paths(&input.excludes, false)?;
    let mut tx = state.pool.begin().await?;
    // Also serialize first creation (there is no row to lock yet).
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("trigger:{id}"))
        .execute(&mut *tx)
        .await?;
    let existing: Option<Trigger> =
        sqlx::query_as("SELECT * FROM pipeline_triggers WHERE pipeline_id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if input.generation != existing.as_ref().map_or(0, |t| t.generation) {
        return Err(ApiError::conflict("trigger changed; refresh before saving"));
    }
    // Disabling must work even while Perforce is unreachable.
    if !input.enabled
        && !input.reset_to_head
        && let Some(old) = &existing
    {
        if old.version != input.version
            || old.includes.0 != input.includes
            || old.excludes.0 != input.excludes
        {
            return Err(ApiError::bad_request(
                "disable without editing filters/version first",
            ));
        }
        sqlx::query("UPDATE pipeline_triggers SET enabled=false,generation=generation+1,configured_by=$2,updated_at=now() WHERE pipeline_id=$1").bind(id).bind(session.user.id).execute(&mut *tx).await?;
    } else {
        let (definition, connection, _, binding) = source(&state, id, input.version).await?;
        if !definition.artifact_inputs.is_empty() {
            return Err(ApiError::bad_request(
                "artifact consumers require a successful source run; use a followup or manual run",
            ));
        }
        crate::followups::validate(&state, pipeline.project_id, &definition).await?;
        let server_id = identity(&state, &connection).await?;
        if existing
            .as_ref()
            .is_some_and(|old| old.source_binding != binding || old.server_id != server_id)
            && !input.reset_to_head
        {
            return Err(ApiError::conflict(
                "source or Perforce identity changed; explicitly reset to current head",
            ));
        }
        let cursor = match existing.as_ref() {
            Some(old) if !input.reset_to_head => old.cursor,
            _ => change_number(&state, &connection, None).await?.unwrap_or(0),
        };
        sqlx::query("INSERT INTO pipeline_triggers(pipeline_id,version,generation,enabled,includes,excludes,configured_by,source_binding,server_id,cursor) VALUES($1,$2,1,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(pipeline_id) DO UPDATE SET version=EXCLUDED.version,generation=pipeline_triggers.generation+1,enabled=EXCLUDED.enabled,includes=EXCLUDED.includes,excludes=EXCLUDED.excludes,configured_by=EXCLUDED.configured_by,source_binding=EXCLUDED.source_binding,server_id=EXCLUDED.server_id,cursor=EXCLUDED.cursor,checked_at=NULL,last_error=NULL,updated_at=now()")
            .bind(id).bind(input.version).bind(input.enabled).bind(DbJson(&input.includes)).bind(DbJson(&input.excludes)).bind(session.user.id).bind(binding).bind(server_id).bind(cursor).execute(&mut *tx).await?;
    }
    authority(&mut tx, session.user.id, pipeline.project_id).await?;
    store::audit(&mut tx, session.user.id, Some(pipeline.project_id), "trigger.configured", json!({"pipeline_id":id,"version":input.version,"enabled":input.enabled,"includes":input.includes,"excludes":input.excludes,"reset_to_head":input.reset_to_head,"generation":input.generation+1})).await?;
    tx.commit().await?;
    get(State(state), Extension(session), Path(id)).await
}

// Read and lock authority through commit, so revocation and dispatch are serialized.
async fn authority(
    tx: &mut Transaction<'_, Postgres>,
    author: Uuid,
    project: Uuid,
) -> Result<User, ApiError> {
    let user: User =
        sqlx::query_as("SELECT id,email,name,is_admin FROM users WHERE id=$1 AND active FOR SHARE")
            .bind(author)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| {
                ApiError::forbidden(
                    "Trigger owner is inactive; an administrator must save the trigger again",
                )
            })?;
    if !user.is_admin {
        let role: Option<String> = sqlx::query_scalar(
            "SELECT role FROM project_members WHERE project_id=$1 AND user_id=$2 FOR SHARE",
        )
        .bind(project)
        .bind(author)
        .fetch_optional(&mut **tx)
        .await?;
        if role.as_deref() != Some("admin") {
            return Err(ApiError::forbidden(
                "Trigger owner no longer has project admin permission",
            ));
        }
    }
    Ok(user)
}

// Public for deterministic reconciliation tests. A failed scan preserves its cursor.
pub async fn tick(state: &AppState) -> Result<(), ApiError> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT pipeline_id FROM pipeline_triggers WHERE enabled ORDER BY checked_at NULLS FIRST,pipeline_id LIMIT 64").fetch_all(&state.pool).await?;
    for id in ids {
        for _ in 0..16 {
            if !poll_one(state, id).await? {
                break;
            }
        }
    }
    Ok(())
}
async fn poll_one(state: &AppState, id: Uuid) -> Result<bool, ApiError> {
    let mut tx = state.pool.begin().await?;
    let trigger: Option<Trigger> = sqlx::query_as(
        "SELECT * FROM pipeline_triggers WHERE pipeline_id=$1 AND enabled FOR UPDATE SKIP LOCKED",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(trigger) = trigger else {
        return Ok(false);
    };
    // A savepoint rolls back partial inserts while keeping the configuration lock.
    sqlx::query("SAVEPOINT scan").execute(&mut *tx).await?;
    match advance(state, &mut tx, &trigger).await {
        Ok(progress) => {
            sqlx::query("UPDATE pipeline_triggers SET checked_at=now(),last_error=NULL WHERE pipeline_id=$1").bind(id).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(progress)
        }
        Err(error) => {
            sqlx::query("ROLLBACK TO SAVEPOINT scan")
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE pipeline_triggers SET checked_at=now(),last_error=$2 WHERE pipeline_id=$1",
            )
            .bind(id)
            .bind(&error.1)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(false)
        }
    }
}
async fn advance(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    trigger: &Trigger,
) -> Result<bool, ApiError> {
    let id = trigger.pipeline_id;
    let pipeline: store::Pipeline =
        sqlx::query_as("SELECT id,project_id,name,current_version FROM pipelines WHERE id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    let user = authority(tx, trigger.configured_by, pipeline.project_id).await?;
    let (definition, connection, root, binding) = source(state, id, trigger.version).await?;
    if binding != trigger.source_binding || identity(state, &connection).await? != trigger.server_id
    {
        return Err(ApiError::conflict(
            "Perforce source or identity changed; cursor preserved",
        ));
    }
    let Some(number) = change_number(state, &connection, Some(trigger.cursor)).await? else {
        if change_number(state, &connection, None).await?.unwrap_or(0) < trigger.cursor {
            return Err(ApiError::conflict(
                "Perforce submitted head moved behind cursor; check server restore or visibility before resetting",
            ));
        }
        return Ok(false);
    };
    let change = state
        .perforce
        .tagged(&connection, &["describe", "-s", &number.to_string()])
        .await
        .map_err(|_| ApiError::unavailable("Could not inspect changed files; cursor preserved"))?;
    if change.get("change") != Some(&number.to_string())
        || change.get("status").map(String::as_str) != Some("submitted")
    {
        return Err(ApiError::unavailable(
            "Invalid submitted-change details; cursor preserved",
        ));
    }
    let mut files = Vec::new();
    for i in 0.. {
        match change.get(&format!("depotFile{i}")) {
            Some(file) => files.push(file),
            None => break,
        }
    }
    if change
        .keys()
        .filter(|key| key.starts_with("depotFile"))
        .count()
        != files.len()
    {
        return Err(ApiError::unavailable(
            "Incomplete changed-file list; cursor preserved",
        ));
    }
    let matched = files
        .iter()
        .any(|file| selected(file, &trigger.includes, &trigger.excludes));
    // Re-check the durable key even after an explicit source reset; never replay an old event.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pipeline_trigger_events WHERE pipeline_id=$1 AND server_id=$2 AND changelist=$3)").bind(id).bind(&trigger.server_id).bind(number).fetch_one(&mut **tx).await?;
    if !exists {
        let run = if matched {
            crate::followups::validate(state, pipeline.project_id, &definition).await?;
            let snapshot = state
                .perforce
                .snapshot(&connection, &root, number)
                .await
                .map_err(|_| {
                    ApiError::unavailable("Could not freeze Perforce snapshot; cursor preserved")
                })?;
            if snapshot.server_id != trigger.server_id {
                return Err(ApiError::conflict(
                    "Perforce identity changed during snapshot",
                ));
            }
            let input = store::RunInput {
                source_run_id: None,
                changelist: number,
                request_id: Uuid::new_v4(),
                version: Some(trigger.version),
            };
            let hash = hash_bytes(&serde_json::to_vec(&(id, &input)).map_err(ApiError::internal)?);
            Some(
                store::enqueue_run(
                    tx,
                    &pipeline,
                    &user,
                    &input,
                    &definition,
                    &snapshot,
                    &hash,
                    None,
                )
                .await?
                .id,
            )
        } else {
            None
        };
        sqlx::query("INSERT INTO pipeline_trigger_events(pipeline_id,server_id,changelist,generation,version,outcome,run_id) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(id).bind(&trigger.server_id).bind(number).bind(trigger.generation).bind(trigger.version).bind(if run.is_some() {"created"} else {"filtered"}).bind(run).execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE pipeline_triggers SET cursor=$2 WHERE pipeline_id=$1")
        .bind(id)
        .bind(number)
        .execute(&mut **tx)
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn depot_filters_are_literal_bounded_and_exclusions_win() {
        let includes = vec!["//streams/root/Server/...".into()];
        let excludes = vec!["//streams/root/Server/generated/...".into()];
        validate_paths(&includes, true).unwrap_or_else(|e| panic!("{}", e.1));
        assert!(selected(
            "//streams/root/Server/removed.rs",
            &includes,
            &excludes
        ));
        assert!(!selected("//streams/root/Server2/x", &includes, &excludes));
        assert!(!selected(
            "//streams/root/Server/generated/x",
            &includes,
            &excludes
        ));
        assert!(!selected("//streams/root/Client/x", &includes, &excludes));
        assert!(matches("//d/a file.txt", "//d/a file.txt"));
        for path in [
            "-//d/a/...",
            "//d/a/...@12",
            "//d/a/*",
            "//d/../a",
            "//d",
            "//d/a/.../...",
            "//d/a\n",
        ] {
            assert!(validate_paths(&[path.into()], true).is_err(), "{path}");
        }
    }
}
