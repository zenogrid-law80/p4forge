//! Actual automatic follow-up lineage, scoped to the selected run's project.
use crate::{AppState, auth::Session, error::ApiError};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use chrono::{DateTime, Utc};
use p4forge_domain::PipelineDefinition;
use serde_json::{Value, json};
use sqlx::{FromRow, types::Json as DbJson};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const LIMIT: usize = 65;

#[derive(FromRow)]
struct Run {
    id: Uuid,
    pipeline_id: Uuid,
    name: String,
    version: i64,
    status: String,
    cancel_requested: bool,
    changelist: i64,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    definition: DbJson<PipelineDefinition>,
    deployment_status: Option<String>,
    publication: Option<Value>,
}

#[derive(FromRow)]
struct Followup {
    parent_run_id: Uuid,
    pipeline_id: Uuid,
    version: i64,
    child_run_id: Option<Uuid>,
    status: String,
    reason: String,
}

fn unregistered_state(status: &str, canceled: bool) -> &'static str {
    if canceled || matches!(status, "failed" | "lost" | "canceled") {
        "not_scheduled"
    } else if status == "succeeded" {
        "pending"
    } else {
        "awaiting_parent"
    }
}

pub async fn get(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let project: Uuid = sqlx::query_scalar("SELECT p.project_id FROM runs r JOIN pipelines p ON p.id=r.pipeline_id WHERE r.id=$1 AND ($2 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=p.project_id AND m.user_id=$3))")
        .bind(id).bind(session.user.is_admin).bind(session.user.id).fetch_optional(&mut *tx).await?.ok_or_else(ApiError::not_found)?;
    let checked_at: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await?;
    // A recorded child has at most one parent. Never infer lineage from version or CL.
    let mut root = id;
    let mut ancestors = BTreeSet::from([id]);
    loop {
        let parent: Option<(Uuid, Uuid)> = sqlx::query_as("SELECT r.id,p.project_id FROM run_followups f JOIN runs r ON r.id=f.parent_run_id JOIN pipelines p ON p.id=r.pipeline_id WHERE f.child_run_id=$1 AND f.status='created'")
            .bind(root).fetch_optional(&mut *tx).await?;
        let Some((parent, parent_project)) = parent else {
            break;
        };
        if parent_project != project {
            return Err(ApiError::not_found());
        }
        if !ancestors.insert(parent) || ancestors.len() > LIMIT {
            return Err(ApiError::conflict("invalid or oversized run chain"));
        }
        root = parent;
    }

    let mut pending = vec![root];
    let mut seen = BTreeSet::from([root]);
    let mut runs = Vec::<Run>::new();
    let mut followups = Vec::<Followup>::new();
    while !pending.is_empty() {
        if seen.len() > LIMIT {
            return Err(ApiError::conflict("run chain exceeds 65 runs"));
        }
        let batch: Vec<Run> = sqlx::query_as("SELECT r.id,r.pipeline_id,p.name,r.version,r.status,r.cancel_requested,(r.source_snapshot->>'changelist')::bigint AS changelist,r.created_at,r.finished_at,r.definition,d.status AS deployment_status,CASE WHEN pub.run_id IS NOT NULL THEN jsonb_build_object('status',pub.status,'changelist',pub.changelist) END AS publication
            FROM runs r JOIN pipelines p ON p.id=r.pipeline_id LEFT JOIN run_deployments d ON d.run_id=r.id LEFT JOIN run_publications pub ON pub.run_id=r.id
            WHERE r.id=ANY($1) AND p.project_id=$2 ORDER BY r.created_at,r.id")
            .bind(&pending).bind(project).fetch_all(&mut *tx).await?;
        if batch.len() != pending.len() {
            return Err(ApiError::not_found());
        }
        let links: Vec<Followup> = sqlx::query_as("SELECT parent_run_id,pipeline_id,version,child_run_id,status,reason FROM run_followups WHERE parent_run_id=ANY($1) ORDER BY parent_run_id,pipeline_id")
            .bind(&pending).fetch_all(&mut *tx).await?;
        pending.clear();
        for link in &links {
            if link.status == "created" {
                let child = link
                    .child_run_id
                    .ok_or_else(|| ApiError::conflict("missing follow-up run"))?;
                if !seen.insert(child) {
                    return Err(ApiError::conflict("cyclic run chain"));
                }
                pending.push(child);
            }
        }
        runs.extend(batch);
        followups.extend(links);
    }
    let by_id: BTreeMap<_, _> = runs.iter().map(|run| (run.id, run)).collect();
    if !by_id.contains_key(&id) {
        return Err(ApiError::conflict("incomplete run chain"));
    }
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut ongoing = false;
    let links: BTreeMap<_, _> = followups
        .iter()
        .map(|f| ((f.parent_run_id, f.pipeline_id), f))
        .collect();
    // Fetch only names for the immediate configured targets; these are not actual runs.
    let targets: Vec<Uuid> = runs
        .iter()
        .flat_map(|r| r.definition.on_success.iter().map(|t| t.pipeline_id))
        .collect();
    let names: BTreeMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id,name FROM pipelines WHERE id=ANY($1) AND project_id=$2",
    )
    .bind(&targets)
    .bind(project)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    for run in &runs {
        ongoing |= matches!(run.status.as_str(), "queued" | "running");
        nodes.push(json!({"key":run.id,"kind":"run","run_id":run.id,"pipeline_id":run.pipeline_id,"name":run.name,"version":run.version,"status":run.status,"cancel_requested":run.cancel_requested,"changelist":run.changelist,"created_at":run.created_at,"finished_at":run.finished_at,"deployment_status":run.deployment_status,"publication":run.publication}));
        for target in &run.definition.on_success {
            let name = names
                .get(&target.pipeline_id)
                .ok_or_else(ApiError::not_found)?;
            let link = links.get(&(run.id, target.pipeline_id));
            if link.is_some_and(|f| f.version != target.version) {
                return Err(ApiError::conflict("follow-up version mismatch"));
            }
            let key = if let Some(link) = link.filter(|f| f.status == "created") {
                let child = by_id
                    .get(&link.child_run_id.unwrap())
                    .ok_or_else(ApiError::not_found)?;
                if child.pipeline_id != target.pipeline_id || child.version != target.version {
                    return Err(ApiError::conflict("follow-up run mismatch"));
                }
                child.id.to_string()
            } else {
                let status = link
                    .map(|f| f.status.as_str())
                    .unwrap_or_else(|| unregistered_state(&run.status, run.cancel_requested));
                ongoing |= matches!(status, "pending" | "awaiting_parent");
                let key = format!("next:{}:{}:v{}", run.id, target.pipeline_id, target.version);
                nodes.push(json!({"key":key,"kind":"followup","run_id":null,"pipeline_id":target.pipeline_id,"name":name,"version":target.version,"status":status,"reason":link.map(|f| f.reason.as_str()).unwrap_or(""),"parent_run_id":run.id}));
                key
            };
            edges.push(json!({"from":run.id,"to":key}));
        }
    }
    // Invalid links must not silently create a disconnected graph.
    if followups.iter().any(|f| {
        !by_id[&f.parent_run_id]
            .definition
            .on_success
            .iter()
            .any(|t| t.pipeline_id == f.pipeline_id && t.version == f.version)
    }) {
        return Err(ApiError::conflict(
            "follow-up missing from fixed run definition",
        ));
    }
    if nodes.len() > LIMIT {
        return Err(ApiError::conflict("run chain exceeds 65 nodes"));
    }
    tx.commit().await?;
    Ok(Json(
        json!({"checked_at":checked_at,"project_id":project,"root":root,"selected_run_id":id,"ongoing":ongoing,"nodes":nodes,"edges":edges}),
    ))
}
