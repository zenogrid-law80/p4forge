//! Read-only connections between explicitly pinned published pipeline versions.
use crate::{AppState, auth::Session, error::ApiError, store::Pipeline};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Utc};
use p4forge_domain::PipelineDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, types::Json as DbJson};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct Selection {
    version: Option<i64>,
}

#[derive(FromRow)]
struct Revision {
    pipeline_id: Uuid,
    name: String,
    current_version: i64,
    version: i64,
    created_at: DateTime<Utc>,
    definition: DbJson<PipelineDefinition>,
    latest_run: Option<Value>,
}

fn key(id: Uuid, version: i64) -> String {
    format!("{id}:v{version}")
}

pub async fn get(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Query(selection): Query<Selection>,
) -> Result<Json<Value>, ApiError> {
    if selection.version.is_some_and(|v| v <= 0) {
        return Err(ApiError::bad_request("version must be positive"));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let pipeline: Pipeline = sqlx::query_as("SELECT p.id,p.project_id,p.name,p.current_version FROM pipelines p WHERE p.id=$1 AND ($2 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=p.project_id AND m.user_id=$3))")
        .bind(id).bind(session.user.is_admin).bind(session.user.id).fetch_optional(&mut *tx).await?.ok_or_else(ApiError::not_found)?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await?;
    let version = selection.version.unwrap_or(pipeline.current_version);
    if version == 0 {
        tx.commit().await?;
        return Ok(Json(
            json!({"checked_at":now,"project_id":pipeline.project_id,"root":null,"nodes":[],"edges":[]}),
        ));
    }
    let mut seen = BTreeSet::from([(id, version)]);
    let mut pending = vec![(id, version)];
    let mut records = Vec::<Revision>::new();
    while !pending.is_empty() {
        // Publishing already limits a downstream tree to 64 descendants. Bound reads too.
        if seen.len() > 65 {
            return Err(ApiError::conflict(
                "published connections exceed 65 versions",
            ));
        }
        let (ids, versions): (Vec<_>, Vec<_>) = pending.iter().copied().unzip();
        let rows: Vec<Revision> = sqlx::query_as("SELECT p.id AS pipeline_id,p.name,p.current_version,r.version,r.created_at,r.definition,latest.value AS latest_run
            FROM unnest($1::uuid[],$2::bigint[]) AS target(id,version)
            JOIN pipelines p ON p.id=target.id AND p.project_id=$3
            JOIN pipeline_revisions r ON r.pipeline_id=p.id AND r.version=target.version
            LEFT JOIN LATERAL (SELECT jsonb_build_object('id',run.id,'status',run.status,'cancel_requested',run.cancel_requested,'changelist',(run.source_snapshot->>'changelist')::bigint,'created_at',run.created_at,'deployment_status',d.status) AS value FROM runs run LEFT JOIN run_deployments d ON d.run_id=run.id WHERE run.pipeline_id=p.id AND run.version=r.version ORDER BY run.created_at DESC,run.id DESC LIMIT 1) latest ON true
            ORDER BY p.name,p.id,r.version")
            .bind(&ids).bind(&versions).bind(pipeline.project_id).fetch_all(&mut *tx).await?;
        if rows.len() != pending.len() {
            // Do not reveal metadata from an invalid reference into another project.
            return Err(ApiError::not_found());
        }
        pending.clear();
        for row in rows {
            for target in &row.definition.on_success {
                if seen.insert((target.pipeline_id, target.version)) {
                    pending.push((target.pipeline_id, target.version));
                }
            }
            records.push(row);
        }
    }
    let by_version: BTreeMap<_, _> = records
        .iter()
        .map(|r| ((r.pipeline_id, r.version), r))
        .collect();
    let mut edges = Vec::new();
    let mut nodes = Vec::new();
    for row in &records {
        let definition = &row.definition;
        let node_key = key(row.pipeline_id, row.version);
        for target in &definition.on_success {
            let child = by_version
                .get(&(target.pipeline_id, target.version))
                .ok_or_else(ApiError::not_found)?;
            edges.push(json!({"from":node_key,"to":key(target.pipeline_id,target.version),"artifacts":child.definition.artifact_inputs,"changelist_mode":if definition.publish_generated.is_some() {"published_or_input"} else {"input"}}));
        }
        nodes.push(json!({"key":node_key,"pipeline_id":row.pipeline_id,"name":row.name,"version":row.version,"current_version":row.current_version,"published_at":row.created_at,"job_count":definition.jobs.len(),"artifact_inputs":definition.artifact_inputs,"artifacts":definition.artifacts.iter().map(|a| &a.name).collect::<Vec<_>>(),"publishes_generated":definition.publish_generated.is_some(),"deployment":definition.deployment.is_some(),"latest_run":row.latest_run}));
    }
    tx.commit().await?;
    Ok(Json(
        json!({"checked_at":now,"project_id":pipeline.project_id,"root":key(id,version),"nodes":nodes,"edges":edges}),
    ))
}
