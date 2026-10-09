use axum::{
    Extension, Json,
    extract::{Path, State},
};
use chrono::{DateTime, Utc};
use p4forge_domain::{PipelineDefinition, hash_bytes, validate_name};
use p4forge_perforce::{Connection, Root, SourceSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool, Postgres, Transaction, types::Json as DbJson};
use uuid::Uuid;

use crate::{
    AppState,
    auth::{Session, User},
    error::ApiError,
};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Operator,
    Editor,
    Admin,
}
impl Role {
    fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Operator => 1,
            Self::Editor => 2,
            Self::Admin => 3,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Editor => "editor",
            Self::Admin => "admin",
        }
    }
}

pub async fn require_role(
    pool: &PgPool,
    user: &User,
    project: Uuid,
    role: Role,
) -> Result<(), ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id=$1)")
        .bind(project)
        .fetch_one(pool)
        .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    if user.is_admin {
        return Ok(());
    }
    let member: Option<String> =
        sqlx::query_scalar("SELECT role FROM project_members WHERE project_id=$1 AND user_id=$2")
            .bind(project)
            .bind(user.id)
            .fetch_optional(pool)
            .await?;
    let member = member.ok_or_else(ApiError::not_found)?;
    let rank = match member.as_str() {
        "viewer" => 0,
        "operator" => 1,
        "editor" => 2,
        "admin" => 3,
        _ => 0,
    };
    if rank < role.rank() {
        return Err(ApiError::forbidden("insufficient project permission"));
    }
    Ok(())
}

pub(crate) fn require_admin(user: &User) -> Result<(), ApiError> {
    if !user.is_admin {
        return Err(ApiError::forbidden("administrator permission required"));
    }
    Ok(())
}

pub(crate) async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
    project: Option<Uuid>,
    action: &str,
    details: Value,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO audit_events(user_id,project_id,action,details) VALUES($1,$2,$3,$4)")
        .bind(user)
        .bind(project)
        .bind(action)
        .bind(details)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn me(Extension(session): Extension<Session>) -> Json<User> {
    Json(session.user)
}

#[derive(FromRow, Serialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub role: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Named {
    pub name: String,
}

pub async fn projects(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(sqlx::query_as("SELECT p.*,CASE WHEN $1 THEN 'admin' ELSE (SELECT role FROM project_members WHERE project_id=p.id AND user_id=$2) END AS role FROM projects p WHERE $1 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=p.id AND m.user_id=$2) ORDER BY p.name").bind(session.user.is_admin).bind(session.user.id).fetch_all(&state.pool).await?))
}
pub async fn create_project(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Json(input): Json<Named>,
) -> Result<Json<Project>, ApiError> {
    require_admin(&session.user)?;
    validate_name(&input.name).map_err(ApiError::bad_request)?;
    let mut tx = state.pool.begin().await?;
    let project: Project =
        sqlx::query_as("INSERT INTO projects(id,name) VALUES($1,$2) RETURNING *,'admin' AS role")
            .bind(Uuid::new_v4())
            .bind(&input.name)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'admin')")
        .bind(project.id)
        .bind(session.user.id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        session.user.id,
        Some(project.id),
        "project.created",
        json!({"name":project.name}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(project))
}

pub async fn users(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Vec<User>>, ApiError> {
    require_admin(&session.user)?;
    Ok(Json(
        sqlx::query_as("SELECT id,email,name,is_admin FROM users WHERE active ORDER BY email")
            .fetch_all(&state.pool)
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberInput {
    user_id: Uuid,
    role: Role,
}
#[derive(Serialize, FromRow)]
pub struct Member {
    user_id: Uuid,
    email: String,
    role: String,
}
pub async fn members(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<Member>>, ApiError> {
    require_role(&state.pool, &session.user, project, Role::Admin).await?;
    Ok(Json(sqlx::query_as("SELECT m.user_id,u.email,m.role FROM project_members m JOIN users u ON u.id=m.user_id WHERE m.project_id=$1 ORDER BY u.email").bind(project).fetch_all(&state.pool).await?))
}
pub async fn set_member(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Json(input): Json<MemberInput>,
) -> Result<Json<Value>, ApiError> {
    require_role(&state.pool, &session.user, project, Role::Admin).await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,$3) ON CONFLICT(project_id,user_id) DO UPDATE SET role=EXCLUDED.role").bind(project).bind(input.user_id).bind(input.role.as_str()).execute(&mut *tx).await?;
    audit(
        &mut tx,
        session.user.id,
        Some(project),
        "member.updated",
        json!({"user_id":input.user_id,"role":input.role.as_str()}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}

#[derive(FromRow, Serialize)]
pub struct ConnectionRecord {
    pub id: Uuid,
    pub name: String,
    pub definition: DbJson<Connection>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionInput {
    name: String,
    definition: Connection,
}
pub async fn connections(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Vec<ConnectionRecord>>, ApiError> {
    require_admin(&session.user)?;
    Ok(Json(
        sqlx::query_as("SELECT id,name,definition FROM perforce_connections ORDER BY name")
            .fetch_all(&state.pool)
            .await?,
    ))
}
pub async fn create_connection(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Json(input): Json<ConnectionInput>,
) -> Result<Json<ConnectionRecord>, ApiError> {
    require_admin(&session.user)?;
    validate_name(&input.name).map_err(ApiError::bad_request)?;
    input.definition.validate().map_err(ApiError::bad_request)?;
    let mut tx = state.pool.begin().await?;
    let record: ConnectionRecord=sqlx::query_as("INSERT INTO perforce_connections(id,name,definition) VALUES($1,$2,$3) RETURNING id,name,definition").bind(Uuid::new_v4()).bind(&input.name).bind(DbJson(&input.definition)).fetch_one(&mut *tx).await?;
    audit(
        &mut tx,
        session.user.id,
        None,
        "connection.created",
        json!({"connection_id":record.id}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(record))
}
pub async fn check_connection(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&session.user)?;
    let connection: DbJson<Connection> =
        sqlx::query_scalar("SELECT definition FROM perforce_connections WHERE id=$1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(ApiError::not_found)?;
    let info = state.perforce.info(&connection.0).await.map_err(|error| {
        tracing::warn!(%error,"Perforce connection check failed");
        ApiError::unavailable(
            "Perforce connection failed; check server, ticket and trust configuration",
        )
    })?;
    Ok(Json(
        json!({"server_id":info.get("ServerID").or_else(|| info.get("serverID")),"server_version":info.get("serverVersion"),"server_address":info.get("serverAddress")}),
    ))
}

#[derive(FromRow, Serialize)]
pub struct RootRecord {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub name: String,
    pub definition: DbJson<Root>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootInput {
    name: String,
    connection_id: Uuid,
    definition: Root,
}
pub async fn roots(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<RootRecord>>, ApiError> {
    require_role(&state.pool, &session.user, project, Role::Viewer).await?;
    Ok(Json(sqlx::query_as("SELECT id,connection_id,name,definition FROM perforce_roots WHERE project_id=$1 ORDER BY name").bind(project).fetch_all(&state.pool).await?))
}
pub async fn create_root(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Json(input): Json<RootInput>,
) -> Result<Json<RootRecord>, ApiError> {
    // A connection can grant source access; only a global admin assigns it to a project.
    require_admin(&session.user)?;
    require_role(&state.pool, &session.user, project, Role::Admin).await?;
    validate_name(&input.name).map_err(ApiError::bad_request)?;
    input.definition.validate().map_err(ApiError::bad_request)?;
    let mut tx = state.pool.begin().await?;
    let root: RootRecord=sqlx::query_as("INSERT INTO perforce_roots(id,project_id,connection_id,name,definition) VALUES($1,$2,$3,$4,$5) RETURNING id,connection_id,name,definition").bind(Uuid::new_v4()).bind(project).bind(input.connection_id).bind(&input.name).bind(DbJson(&input.definition)).fetch_one(&mut *tx).await?;
    audit(
        &mut tx,
        session.user.id,
        Some(project),
        "root.created",
        json!({"root_id":root.id}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(root))
}

#[derive(Clone, FromRow, Serialize)]
pub struct Pipeline {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub current_version: i64,
}
#[derive(FromRow, Serialize)]
pub struct Draft {
    pub generation: i64,
    pub base_version: i64,
    pub definition: Option<DbJson<PipelineDefinition>>,
}
#[derive(FromRow, Serialize)]
pub struct Revision {
    pub version: i64,
    pub definition: DbJson<PipelineDefinition>,
    pub definition_hash: String,
    pub created_at: DateTime<Utc>,
}
pub(crate) async fn pipeline_access(
    state: &AppState,
    user: &User,
    id: Uuid,
    role: Role,
) -> Result<Pipeline, ApiError> {
    let pipeline: Pipeline =
        sqlx::query_as("SELECT id,project_id,name,current_version FROM pipelines WHERE id=$1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(ApiError::not_found)?;
    require_role(&state.pool, user, pipeline.project_id, role).await?;
    Ok(pipeline)
}
#[derive(FromRow, Serialize)]
pub struct PipelineSummary {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pipeline: Pipeline,
    latest_run_id: Option<Uuid>,
    latest_run_status: Option<String>,
    latest_run_at: Option<DateTime<Utc>>,
    trigger_enabled: bool,
}

pub async fn pipelines(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<PipelineSummary>>, ApiError> {
    require_role(&state.pool, &session.user, project, Role::Viewer).await?;
    Ok(Json(
        sqlx::query_as(
            "SELECT p.id,p.project_id,p.name,p.current_version,
                r.id AS latest_run_id,r.status AS latest_run_status,r.created_at AS latest_run_at,
                COALESCE(t.enabled,false) AS trigger_enabled
         FROM pipelines p
         LEFT JOIN LATERAL (
             SELECT id,status,created_at FROM runs WHERE pipeline_id=p.id
             ORDER BY created_at DESC,id DESC LIMIT 1
         ) r ON true
         LEFT JOIN pipeline_triggers t ON t.pipeline_id=p.id
         WHERE p.project_id=$1 ORDER BY p.name",
        )
        .bind(project)
        .fetch_all(&state.pool)
        .await?,
    ))
}
pub async fn create_pipeline(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(project): Path<Uuid>,
    Json(input): Json<Named>,
) -> Result<Json<Pipeline>, ApiError> {
    require_role(&state.pool, &session.user, project, Role::Editor).await?;
    validate_name(&input.name).map_err(ApiError::bad_request)?;
    let mut tx = state.pool.begin().await?;
    let pipeline: Pipeline=sqlx::query_as("INSERT INTO pipelines(id,project_id,name) VALUES($1,$2,$3) RETURNING id,project_id,name,current_version").bind(Uuid::new_v4()).bind(project).bind(input.name).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO pipeline_drafts(pipeline_id) VALUES($1)")
        .bind(pipeline.id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        session.user.id,
        Some(project),
        "pipeline.created",
        json!({"pipeline_id":pipeline.id}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(pipeline))
}
pub async fn pipeline_detail(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let pipeline = pipeline_access(&state, &session.user, id, Role::Viewer).await?;
    let draft: Draft = sqlx::query_as(
        "SELECT generation,base_version,definition FROM pipeline_drafts WHERE pipeline_id=$1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    let revision: Option<Revision>=sqlx::query_as("SELECT version,definition,definition_hash,created_at FROM pipeline_revisions WHERE pipeline_id=$1 AND version=$2").bind(id).bind(pipeline.current_version).fetch_optional(&state.pool).await?;
    let toml = draft
        .definition
        .as_ref()
        .map(|definition| definition.0.to_toml())
        .transpose()
        .map_err(ApiError::internal)?;
    Ok(Json(
        json!({"pipeline":pipeline,"draft":draft,"current_revision":revision,"draft_toml":toml,"configuration_draft":crate::configurations::detail(&state,id).await?}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionInput {
    pub toml: String,
}
async fn validate_definition(
    state: &AppState,
    project: Uuid,
    pipeline: Uuid,
    input: &str,
) -> Result<PipelineDefinition, ApiError> {
    let definition = PipelineDefinition::parse(input).map_err(ApiError::bad_request)?;
    let belongs: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM perforce_roots WHERE id=$1 AND project_id=$2)",
    )
    .bind(definition.source_root_id)
    .bind(project)
    .fetch_one(&state.pool)
    .await?;
    if !belongs {
        return Err(ApiError::bad_request(
            "source_root_id must belong to this project",
        ));
    }
    crate::followups::validate(state, project, &definition).await?;
    let mut tx = state.pool.begin().await?;
    crate::deployments::validate(&mut tx, project, pipeline, &definition).await?;
    Ok(definition)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionPreviewInput {
    pub definition: PipelineDefinition,
}

pub async fn preview_definition(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<DefinitionPreviewInput>,
) -> Result<Json<Value>, ApiError> {
    let pipeline = pipeline_access(&state, &session.user, id, Role::Editor).await?;
    crate::configurations::require_executable_mode(&state, id).await?;
    let toml = input.definition.to_toml().map_err(ApiError::bad_request)?;
    // Use the same scope and reference checks as a draft save, without writing a draft.
    let definition = validate_definition(&state, pipeline.project_id, id, &toml).await?;
    Ok(Json(json!({
        "toml": toml,
        "graph": definition.validate().map_err(ApiError::bad_request)?,
        "definition_hash": definition.hash().map_err(ApiError::bad_request)?
    })))
}

pub async fn validate(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<DefinitionInput>,
) -> Result<Json<Value>, ApiError> {
    let pipeline = pipeline_access(&state, &session.user, id, Role::Editor).await?;
    let definition = validate_definition(&state, pipeline.project_id, id, &input.toml).await?;
    Ok(Json(
        json!({"definition":definition,"graph":definition.validate().map_err(ApiError::bad_request)?,"definition_hash":definition.hash().map_err(ApiError::bad_request)?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftInput {
    pub toml: String,
    pub generation: i64,
    pub base_version: i64,
}
pub async fn save_draft(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<DraftInput>,
) -> Result<Json<Draft>, ApiError> {
    let pipeline = pipeline_access(&state, &session.user, id, Role::Editor).await?;
    crate::configurations::require_executable_mode(&state, id).await?;
    let definition = validate_definition(&state, pipeline.project_id, id, &input.toml).await?;
    let mut tx = state.pool.begin().await?;
    let current: i64 =
        sqlx::query_scalar("SELECT current_version FROM pipelines WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if current != input.base_version {
        return Err(ApiError::conflict(
            "published revision changed; reload and merge your draft",
        ));
    }
    let draft: Draft=sqlx::query_as("UPDATE pipeline_drafts SET definition=$2,generation=generation+1,base_version=$3,edited_by=$4,updated_at=now() WHERE pipeline_id=$1 AND generation=$5 RETURNING generation,base_version,definition").bind(id).bind(DbJson(definition)).bind(current).bind(session.user.id).bind(input.generation).fetch_optional(&mut *tx).await?.ok_or_else(|| ApiError::conflict("draft changed; reload before saving"))?;
    audit(
        &mut tx,
        session.user.id,
        Some(pipeline.project_id),
        "draft.saved",
        json!({"pipeline_id":id,"generation":draft.generation}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(draft))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishInput {
    pub generation: i64,
    pub base_version: i64,
}
pub async fn publish(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<PublishInput>,
) -> Result<Json<Revision>, ApiError> {
    let pipeline = pipeline_access(&state, &session.user, id, Role::Editor).await?;
    crate::configurations::require_executable_mode(&state, id).await?;
    let mut tx = state.pool.begin().await?;
    let current: i64 =
        sqlx::query_scalar("SELECT current_version FROM pipelines WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let draft: Draft=sqlx::query_as("SELECT generation,base_version,definition FROM pipeline_drafts WHERE pipeline_id=$1 FOR UPDATE").bind(id).fetch_one(&mut *tx).await?;
    if current != input.base_version
        || current != draft.base_version
        || draft.generation != input.generation
    {
        return Err(ApiError::conflict(
            "draft or published revision changed; reload before publishing",
        ));
    }
    let definition = draft
        .definition
        .ok_or_else(|| ApiError::bad_request("save a draft before publishing"))?
        .0;
    definition.validate().map_err(ApiError::bad_request)?;
    crate::deployments::validate(&mut tx, pipeline.project_id, id, &definition).await?;
    let revision: Revision=sqlx::query_as("INSERT INTO pipeline_revisions(pipeline_id,version,definition,definition_hash,published_by) VALUES($1,$2,$3,$4,$5) RETURNING version,definition,definition_hash,created_at").bind(id).bind(current+1).bind(DbJson(&definition)).bind(definition.hash().map_err(ApiError::internal)?).bind(session.user.id).fetch_one(&mut *tx).await?;
    sqlx::query("UPDATE pipelines SET current_version=$2 WHERE id=$1")
        .bind(id)
        .bind(revision.version)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE pipeline_drafts SET base_version=$2,generation=generation+1,updated_at=now() WHERE pipeline_id=$1").bind(id).bind(revision.version).execute(&mut *tx).await?;
    audit(
        &mut tx,
        session.user.id,
        Some(pipeline.project_id),
        "revision.published",
        json!({"pipeline_id":id,"version":revision.version,"hash":revision.definition_hash}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(revision))
}
pub async fn revisions(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Revision>>, ApiError> {
    pipeline_access(&state, &session.user, id, Role::Viewer).await?;
    Ok(Json(sqlx::query_as("SELECT version,definition,definition_hash,created_at FROM pipeline_revisions WHERE pipeline_id=$1 ORDER BY version DESC LIMIT 100").bind(id).fetch_all(&state.pool).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreInput {
    pub version: i64,
    pub generation: i64,
    pub base_version: i64,
}
pub async fn restore(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<RestoreInput>,
) -> Result<Json<Draft>, ApiError> {
    pipeline_access(&state, &session.user, id, Role::Editor).await?;
    let definition: DbJson<PipelineDefinition> = sqlx::query_scalar(
        "SELECT definition FROM pipeline_revisions WHERE pipeline_id=$1 AND version=$2",
    )
    .bind(id)
    .bind(input.version)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    save_draft(
        State(state),
        Extension(session),
        Path(id),
        Json(DraftInput {
            toml: definition.0.to_toml().map_err(ApiError::internal)?,
            generation: input.generation,
            base_version: input.base_version,
        }),
    )
    .await
}

#[derive(FromRow, Serialize)]
pub struct Run {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub version: i64,
    pub status: String,
    pub cancel_requested: bool,
    pub definition: DbJson<PipelineDefinition>,
    pub source_snapshot: Value,
    pub input_hash: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunInput {
    pub changelist: i64,
    pub request_id: Uuid,
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<Uuid>,
}
pub async fn create_run(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<RunInput>,
) -> Result<Json<Run>, ApiError> {
    let pipeline = pipeline_access(&state, &session.user, id, Role::Operator).await?;
    if input.request_id.is_nil() || input.changelist <= 0 {
        return Err(ApiError::bad_request(
            "request_id and positive changelist are required",
        ));
    }
    let request_hash = hash_bytes(&serde_json::to_vec(&(id, &input)).map_err(ApiError::internal)?);
    let previous: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,request_hash FROM runs WHERE submitted_by=$1 AND request_id=$2")
            .bind(session.user.id)
            .bind(input.request_id)
            .fetch_optional(&state.pool)
            .await?;
    if let Some((run, hash)) = previous {
        if hash != request_hash {
            return Err(ApiError::conflict(
                "request_id was used for different inputs",
            ));
        }
        return run_detail(State(state), Extension(session), Path(run)).await;
    }
    let version = input.version.unwrap_or(pipeline.current_version);
    let definition: DbJson<PipelineDefinition> = sqlx::query_scalar(
        "SELECT definition FROM pipeline_revisions WHERE pipeline_id=$1 AND version=$2",
    )
    .bind(id)
    .bind(version)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::bad_request("publish a valid pipeline version before running"))?;
    definition.0.validate().map_err(ApiError::internal)?;
    crate::followups::validate(&state, pipeline.project_id, &definition).await?;
    let (root,connection): (DbJson<Root>,DbJson<Connection>)=sqlx::query_as("SELECT r.definition,c.definition FROM perforce_roots r JOIN perforce_connections c ON c.id=r.connection_id WHERE r.id=$1 AND r.project_id=$2").bind(definition.source_root_id).bind(pipeline.project_id).fetch_one(&state.pool).await?;
    let snapshot=state.perforce.snapshot(&connection.0,&root.0,input.changelist).await.map_err(|error| { tracing::warn!(%error,"source snapshot failed"); ApiError::bad_request("could not resolve submitted changelist and pinned Perforce view; check source and server credentials") })?;
    require_role(
        &state.pool,
        &session.user,
        pipeline.project_id,
        Role::Operator,
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("{}:{}", session.user.id, input.request_id))
        .execute(&mut *tx)
        .await?;
    let concurrent: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,request_hash FROM runs WHERE submitted_by=$1 AND request_id=$2")
            .bind(session.user.id)
            .bind(input.request_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((run, hash)) = concurrent {
        if hash != request_hash {
            return Err(ApiError::conflict(
                "request_id was used for different inputs",
            ));
        }
        tx.commit().await?;
        return run_detail(State(state), Extension(session), Path(run)).await;
    }
    let run = enqueue_run(
        &mut tx,
        &pipeline,
        &session.user,
        &RunInput {
            version: Some(version),
            ..input
        },
        &definition.0,
        &snapshot,
        &request_hash,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(run))
}

// Shared transaction boundary for manual runs and automatic change triggers.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn enqueue_run(
    tx: &mut Transaction<'_, Postgres>,
    pipeline: &Pipeline,
    user: &User,
    input: &RunInput,
    definition: &PipelineDefinition,
    snapshot: &SourceSnapshot,
    request_hash: &str,
    rollback_of: Option<Uuid>,
) -> Result<Run, ApiError> {
    let graph = definition.validate().map_err(ApiError::internal)?;
    let inputs = crate::artifact_inputs::resolve(
        tx,
        pipeline.project_id,
        input.source_run_id,
        definition,
        snapshot,
    )
    .await?;
    let input_hash = if inputs.is_empty() {
        hash_bytes(&serde_json::to_vec(&(definition, snapshot)).map_err(ApiError::internal)?)
    } else {
        hash_bytes(
            &serde_json::to_vec(&(definition, snapshot, &inputs)).map_err(ApiError::internal)?,
        )
    };
    let run: Run=sqlx::query_as("INSERT INTO runs(id,pipeline_id,version,submitted_by,request_id,request_hash,definition,source_snapshot,input_hash) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id,pipeline_id,version,status,cancel_requested,definition,source_snapshot,input_hash,created_at")
        .bind(Uuid::new_v4()).bind(pipeline.id).bind(input.version).bind(user.id).bind(input.request_id).bind(request_hash).bind(DbJson(definition)).bind(DbJson(snapshot)).bind(input_hash).fetch_one(&mut **tx).await?;
    crate::artifact_inputs::bind(tx, run.id, &inputs).await?;
    crate::deployments::bind(
        tx,
        run.id,
        pipeline.project_id,
        pipeline.id,
        definition,
        rollback_of,
    )
    .await?;
    let mut job_ids = std::collections::BTreeMap::new();
    for (position, job) in definition.jobs.iter().enumerate() {
        let job_id = Uuid::new_v4();
        job_ids.insert(&job.name, job_id);
        let blocked = graph
            .edges
            .iter()
            .any(|(_, downstream)| downstream == &job.name);
        sqlx::query("INSERT INTO job_runs(id,run_id,name,position,definition,status) VALUES($1,$2,$3,$4,$5,$6)").bind(job_id).bind(run.id).bind(&job.name).bind(position as i32).bind(DbJson(job)).bind(if blocked {"blocked"}else{"queued"}).execute(&mut **tx).await?;
    }
    for (upstream, downstream) in graph.edges {
        sqlx::query("INSERT INTO job_dependencies(run_id,job_id,needs_id) VALUES($1,$2,$3)")
            .bind(run.id)
            .bind(job_ids[&downstream])
            .bind(job_ids[&upstream])
            .execute(&mut **tx)
            .await?;
    }
    audit(
        tx,
        user.id,
        Some(pipeline.project_id),
        "run.created",
        json!({"run_id":run.id,"version":input.version,"changelist":input.changelist}),
    )
    .await?;
    Ok(run)
}

pub async fn runs(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Run>>, ApiError> {
    pipeline_access(&state, &session.user, id, Role::Viewer).await?;
    Ok(Json(sqlx::query_as("SELECT id,pipeline_id,version,status,cancel_requested,definition,source_snapshot,input_hash,created_at FROM runs WHERE pipeline_id=$1 ORDER BY created_at DESC,id DESC LIMIT 100").bind(id).fetch_all(&state.pool).await?))
}
pub async fn run_detail(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Run>, ApiError> {
    let run: Run=sqlx::query_as("SELECT id,pipeline_id,version,status,cancel_requested,definition,source_snapshot,input_hash,created_at FROM runs WHERE id=$1").bind(id).fetch_optional(&state.pool).await?.ok_or_else(ApiError::not_found)?;
    pipeline_access(&state, &session.user, run.pipeline_id, Role::Viewer).await?;
    Ok(Json(run))
}
pub async fn cancel_run(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let run = run_detail(State(state.clone()), Extension(session.clone()), Path(id))
        .await?
        .0;
    let pipeline = pipeline_access(&state, &session.user, run.pipeline_id, Role::Operator).await?;
    let mut tx = state.pool.begin().await?;
    let changed = sqlx::query(
        "UPDATE runs SET cancel_requested=true WHERE id=$1 AND status IN ('queued','running')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(ApiError::conflict(
            "only queued or running runs can be canceled",
        ));
    }
    sqlx::query(
        "UPDATE job_runs SET status='canceled' WHERE run_id=$1 AND status IN ('queued','blocked')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE job_attempts SET cancel_requested=true WHERE job_id IN (SELECT id FROM job_runs WHERE run_id=$1) AND status='running'")
        .bind(id).execute(&mut *tx).await?;
    crate::agents::settle(&mut tx, id).await?;
    audit(
        &mut tx,
        session.user.id,
        Some(pipeline.project_id),
        "run.canceled",
        json!({"run_id":id}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
