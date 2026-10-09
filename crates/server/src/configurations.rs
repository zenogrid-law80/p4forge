use axum::{
    Extension, Json,
    extract::{Path, State},
};
use p4forge_domain::{
    hash_bytes,
    project_config::{PipelineConfiguration, ProjectConfiguration},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::types::Json as DbJson;
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::{
    AppState,
    auth::Session,
    error::ApiError,
    store::{self, Role},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportInput {
    source_name: String,
    toml: String,
}

pub async fn import(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Json(input): Json<ImportInput>,
) -> Result<Json<Value>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Editor).await?;
    if input.source_name.is_empty()
        || input.source_name.len() > 255
        || input.source_name.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request("invalid source_name"));
    }
    let config = ProjectConfiguration::parse(&input.toml).map_err(ApiError::bad_request)?;
    let hash = hash_bytes(input.toml.as_bytes());
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM configuration_imports WHERE project_id=$1 AND source_hash=$2",
    )
    .bind(project)
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = existing {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT pipeline_id FROM configuration_drafts WHERE import_id=$1 ORDER BY pipeline_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        return Ok(Json(
            json!({"import_id":id,"pipeline_ids":ids,"already_imported":true}),
        ));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO configuration_imports(id,project_id,source_name,source_toml,source_hash,imported_by) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(id).bind(project).bind(&input.source_name).bind(&input.toml).bind(&hash).bind(session.user.id).execute(&mut *tx).await?;
    let mut ids = Vec::new();
    for definition in &config.pipelines {
        let pipeline = Uuid::new_v4();
        sqlx::query("INSERT INTO pipelines(id,project_id,name) VALUES($1,$2,$3)")
            .bind(pipeline)
            .bind(project)
            .bind(&definition.name)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO pipeline_drafts(pipeline_id) VALUES($1)")
            .bind(pipeline)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO configuration_drafts(pipeline_id,import_id,definition,edited_by) VALUES($1,$2,$3,$4)")
            .bind(pipeline).bind(id).bind(DbJson(definition)).bind(session.user.id).execute(&mut *tx).await?;
        ids.push(pipeline);
    }
    store::audit(
        &mut tx,
        session.user.id,
        Some(project),
        "configuration.imported",
        json!({"import_id":id,"pipeline_ids":ids,"source_hash":hash}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"import_id":id,"pipeline_ids":ids,"already_imported":false}),
    ))
}

pub async fn detail(state: &AppState, pipeline: Uuid) -> Result<Option<Value>, ApiError> {
    let row: Option<(i64, DbJson<PipelineConfiguration>, String, String)> = sqlx::query_as("SELECT d.generation,d.definition,i.source_name,i.source_hash FROM configuration_drafts d JOIN configuration_imports i ON i.id=d.import_id WHERE d.pipeline_id=$1")
        .bind(pipeline).fetch_optional(&state.pool).await?;
    row.map(|(generation, definition, source_name, source_hash)| {
        Ok(json!({"generation":generation,"toml":definition.to_toml().map_err(ApiError::internal)?,"readiness":definition.readiness(),"definition":definition.0,"source_name":source_name,"source_hash":source_hash,"execution_ready":false}))
    }).transpose()
}

pub async fn require_executable_mode(state: &AppState, pipeline: Uuid) -> Result<(), ApiError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM configuration_drafts WHERE pipeline_id=$1)",
    )
    .bind(pipeline)
    .fetch_one(&state.pool)
    .await?;
    if exists {
        return Err(ApiError::conflict(
            "project configuration requires Perforce source and execution capabilities before conversion/publishing",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveInput {
    generation: i64,
    toml: String,
}

pub async fn save(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(pipeline): Path<Uuid>,
    Json(input): Json<SaveInput>,
) -> Result<Json<Value>, ApiError> {
    let record = store::pipeline_access(&state, &session.user, pipeline, Role::Editor).await?;
    if input.toml.len() > p4forge_domain::MAX_CONFIG_BYTES {
        return Err(ApiError::bad_request("configuration exceeds 256 KiB"));
    }
    // Cross-pipeline dependencies are checked against the other drafts below.
    let mut parsed: ProjectConfiguration =
        toml::from_str(&input.toml).map_err(ApiError::bad_request)?;
    if parsed.schema_version != 1
        || parsed.pipelines.len() != 1
        || parsed.pipelines[0].name != record.name
    {
        return Err(ApiError::bad_request(
            "expected one pipeline with the existing name and schema_version 1",
        ));
    }
    let definition = parsed.pipelines.remove(0);
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(record.project_id)
        .execute(&mut *tx)
        .await?;
    let peers: Vec<DbJson<PipelineConfiguration>> = sqlx::query_scalar("SELECT d.definition FROM configuration_drafts d JOIN pipelines p ON p.id=d.pipeline_id WHERE p.project_id=$1 AND p.id<>$2")
        .bind(record.project_id).bind(pipeline).fetch_all(&mut *tx).await?;
    let mut all = peers.into_iter().map(|p| p.0).collect::<Vec<_>>();
    all.push(definition.clone());
    ProjectConfiguration {
        schema_version: 1,
        pipelines: all,
    }
    .validate()
    .map_err(ApiError::bad_request)?;
    let generation: i64 = sqlx::query_scalar("UPDATE configuration_drafts SET definition=$2,generation=generation+1,edited_by=$3,updated_at=now() WHERE pipeline_id=$1 AND generation=$4 RETURNING generation")
        .bind(pipeline).bind(DbJson(&definition)).bind(session.user.id).bind(input.generation).fetch_optional(&mut *tx).await?
        .ok_or_else(|| ApiError::conflict("configuration changed; reload before saving"))?;
    store::audit(
        &mut tx,
        session.user.id,
        Some(record.project_id),
        "configuration.saved",
        json!({"pipeline_id":pipeline,"generation":generation}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"generation":generation})))
}

pub async fn export(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Viewer).await?;
    let rows: Vec<DbJson<PipelineConfiguration>> = sqlx::query_scalar("SELECT d.definition FROM configuration_drafts d JOIN pipelines p ON p.id=d.pipeline_id WHERE p.project_id=$1 ORDER BY p.created_at,p.name")
        .bind(project).fetch_all(&state.pool).await?;
    let config = ProjectConfiguration {
        schema_version: 1,
        pipelines: rows.into_iter().map(|r| r.0).collect(),
    };
    let generations: Vec<(Uuid,i64)> = sqlx::query_as("SELECT d.pipeline_id,d.generation FROM configuration_drafts d JOIN pipelines p ON p.id=d.pipeline_id WHERE p.project_id=$1")
        .bind(project).fetch_all(&state.pool).await?;
    Ok(Json(
        json!({"toml":toml::to_string_pretty(&config).map_err(ApiError::internal)?,"generations":generations.into_iter().collect::<BTreeMap<_,_>>()}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindSourceInput {
    source_root_id: Uuid,
    generations: BTreeMap<Uuid, i64>,
}

pub async fn bind_source(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Json(input): Json<BindSourceInput>,
) -> Result<Json<Value>, ApiError> {
    store::require_role(&state.pool, &session.user, project, Role::Editor).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    let valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM perforce_roots WHERE id=$1 AND project_id=$2)",
    )
    .bind(input.source_root_id)
    .bind(project)
    .fetch_one(&mut *tx)
    .await?;
    if !valid {
        return Err(ApiError::bad_request(
            "source root must belong to this project",
        ));
    }
    let rows:Vec<(Uuid,i64,DbJson<PipelineConfiguration>)>=sqlx::query_as("SELECT d.pipeline_id,d.generation,d.definition FROM configuration_drafts d JOIN pipelines p ON p.id=d.pipeline_id WHERE p.project_id=$1")
        .bind(project).fetch_all(&mut *tx).await?;
    if rows.is_empty()
        || rows.len() != input.generations.len()
        || rows
            .iter()
            .any(|(id, g, _)| input.generations.get(id) != Some(g))
    {
        return Err(ApiError::conflict(
            "configuration changed; reload before binding source",
        ));
    }
    for (id, _, mut definition) in rows {
        definition.source_root_id = Some(input.source_root_id);
        sqlx::query("UPDATE configuration_drafts SET definition=$2,generation=generation+1,edited_by=$3,updated_at=now() WHERE pipeline_id=$1")
            .bind(id).bind(definition).bind(session.user.id).execute(&mut *tx).await?;
    }
    store::audit(
        &mut tx,
        session.user.id,
        Some(project),
        "configuration.source_bound",
        json!({"source_root_id":input.source_root_id}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
