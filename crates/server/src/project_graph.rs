//! Project-wide read-only graph: published version identities and imported needs stay separate.
use crate::{AppState, auth::Session, error::ApiError};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use p4forge_domain::PipelineDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, types::Json as DbJson};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum View {
    #[default]
    Published,
    Configuration,
}
#[derive(Default, Deserialize)]
pub struct Selection {
    #[serde(default)]
    view: View,
}

#[derive(FromRow)]
struct Pipeline {
    id: Uuid,
    name: String,
    current_version: i64,
    configuration: Option<Value>,
}
#[derive(FromRow)]
struct Revision {
    pipeline_id: Uuid,
    version: i64,
    definition: DbJson<PipelineDefinition>,
    latest_run: Option<Value>,
}
fn key(id: Uuid, version: i64) -> String {
    format!("{id}:v{version}")
}

pub async fn get(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Query(selection): Query<Selection>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let allowed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects p WHERE p.id=$1 AND ($2 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=p.id AND m.user_id=$3)))")
        .bind(project).bind(session.user.is_admin).bind(session.user.id).fetch_one(&mut *tx).await?;
    if !allowed {
        return Err(ApiError::not_found());
    }
    let checked_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await?;
    let pipelines: Vec<Pipeline> = sqlx::query_as("SELECT p.id,p.name,p.current_version,
        CASE WHEN c.pipeline_id IS NOT NULL THEN jsonb_build_object('generation',c.generation,'category',c.definition->>'category','needs',COALESCE(c.definition->'needs','[]'::jsonb),'job_count',jsonb_array_length(c.definition->'jobs')) END AS configuration
        FROM pipelines p LEFT JOIN configuration_drafts c ON c.pipeline_id=p.id
        WHERE p.project_id=$1 ORDER BY p.name,p.id LIMIT 501")
        .bind(project).fetch_all(&mut *tx).await?;
    if pipelines.len() > 500 {
        return Err(ApiError::conflict(
            "project graph supports up to 500 pipelines; no partial graph returned",
        ));
    }
    let by_id: BTreeMap<_, _> = pipelines.iter().map(|p| (p.id, p)).collect();
    let mut nodes = Vec::<Value>::new();
    let mut edges = Vec::<Value>::new();
    let mut warnings = Vec::<Value>::new();
    let view = if selection.view == View::Configuration {
        "configuration"
    } else {
        "published"
    };
    if selection.view == View::Configuration {
        let by_name: BTreeMap<_, _> = pipelines.iter().map(|p| (p.name.as_str(), p.id)).collect();
        for p in &pipelines {
            nodes.push(json!({"key":p.id.to_string(),"pipeline_id":p.id,"name":p.name,"version":null,"current_version":p.current_version,"configuration":p.configuration,"job_count":p.configuration.as_ref().and_then(|c|c.get("job_count")),"latest_run":null}));
            if let Some(needs) = p.configuration.as_ref().and_then(|c| c["needs"].as_array()) {
                for name in needs
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>()
                {
                    if let Some(from) = by_name.get(name) {
                        edges.push(json!({"from":from.to_string(),"to":p.id.to_string(),"kind":"configuration"}));
                    } else {
                        warnings.push(json!({"key":p.id.to_string(),"message":format!("{}: 구성의 선행 파이프라인 {name}을 이 프로젝트에서 찾을 수 없습니다.",p.name)}));
                    }
                }
            }
        }
    } else {
        // Seed every current version, including disconnected components. Older pinned
        // versions remain separate nodes and are expanded with their own definitions.
        let mut pending: Vec<_> = pipelines
            .iter()
            .filter(|p| p.current_version > 0)
            .map(|p| (p.id, p.current_version))
            .collect();
        let mut seen: BTreeSet<_> = pending.iter().copied().collect();
        let mut revisions = Vec::<Revision>::new();
        while !pending.is_empty() {
            if seen.len() > 1000 {
                return Err(ApiError::conflict(
                    "project graph exceeds 1000 pinned versions; no partial graph returned",
                ));
            }
            let (ids, versions): (Vec<_>, Vec<_>) = pending.iter().copied().unzip();
            let rows: Vec<Revision> = sqlx::query_as("SELECT r.pipeline_id,r.version,r.definition,latest.value AS latest_run
                FROM unnest($1::uuid[],$2::bigint[]) target(id,version)
                JOIN pipelines p ON p.id=target.id AND p.project_id=$3
                JOIN pipeline_revisions r ON r.pipeline_id=p.id AND r.version=target.version
                LEFT JOIN LATERAL (SELECT jsonb_build_object('id',run.id,'version',run.version,'status',run.status,'changelist',run.source_snapshot->'changelist','created_at',run.created_at,'deployment_status',d.status) AS value FROM runs run LEFT JOIN run_deployments d ON d.run_id=run.id WHERE run.pipeline_id=p.id AND run.version=r.version ORDER BY run.created_at DESC,run.id DESC LIMIT 1) latest ON true
                ORDER BY p.name,r.version")
                .bind(ids).bind(versions).bind(project).fetch_all(&mut *tx).await?;
            pending.clear();
            for row in &rows {
                for target in &row.definition.on_success {
                    if by_id.contains_key(&target.pipeline_id)
                        && seen.insert((target.pipeline_id, target.version))
                    {
                        pending.push((target.pipeline_id, target.version));
                    }
                }
            }
            revisions.extend(rows);
        }
        let loaded: BTreeMap<_, _> = revisions
            .iter()
            .map(|r| ((r.pipeline_id, r.version), r))
            .collect();
        for p in &pipelines {
            if p.current_version == 0 {
                nodes.push(json!({"key":key(p.id,0),"pipeline_id":p.id,"name":p.name,"version":0,"current_version":0,"configuration":p.configuration,"job_count":null,"latest_run":null}));
            } else if !loaded.contains_key(&(p.id, p.current_version)) {
                return Err(ApiError::conflict(
                    "current published revision is unavailable",
                ));
            }
        }
        for r in &revisions {
            let p = by_id[&r.pipeline_id];
            let definition = &r.definition;
            let node_key = key(p.id, r.version);
            nodes.push(json!({"key":node_key,"pipeline_id":p.id,"name":p.name,"version":r.version,"current_version":p.current_version,"configuration":p.configuration,"job_count":definition.jobs.len(),"latest_run":r.latest_run,"artifact_inputs":definition.artifact_inputs,"artifacts":definition.artifacts.iter().map(|a|&a.name).collect::<Vec<_>>(),"deployment":definition.deployment.is_some()}));
            for target in &definition.on_success {
                if let Some(child) = loaded.get(&(target.pipeline_id, target.version)) {
                    edges.push(json!({"from":node_key,"to":key(target.pipeline_id,target.version),"kind":"published","artifacts":child.definition.artifact_inputs,"changelist_mode":if definition.publish_generated.is_some(){"published_or_input"}else{"input"}}));
                } else {
                    // No target identifier or metadata from another project leaks.
                    warnings.push(json!({"key":node_key,"message":format!("{} v{}: 일부 후속 발행 연결을 이 프로젝트에서 확인할 수 없습니다.",p.name,r.version)}));
                }
            }
        }
        nodes.sort_by(|a, b| {
            a["name"]
                .as_str()
                .cmp(&b["name"].as_str())
                .then_with(|| a["version"].as_i64().cmp(&b["version"].as_i64()))
        });
    }
    tx.commit().await?;
    Ok(Json(
        json!({"project_id":project,"view":view,"checked_at":checked_at,"pipeline_count":pipelines.len(),"nodes":nodes,"edges":edges,"warnings":warnings}),
    ))
}
