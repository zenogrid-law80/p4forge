//! Downstream versions are explicit immutable references, resolved before publishing.
use crate::{
    AppState,
    auth::{Session, User},
    error::ApiError,
    store,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use p4forge_domain::PipelineDefinition;
use serde_json::{Value, json};
use sqlx::types::Json as DbJson;
use uuid::Uuid;

pub async fn validate(
    state: &AppState,
    project: Uuid,
    definition: &PipelineDefinition,
) -> Result<(), ApiError> {
    let mut connection = state.pool.acquire().await?;
    validate_with_connection(&mut connection, project, definition).await
}

pub(crate) async fn validate_with_connection(
    connection: &mut sqlx::PgConnection,
    project: Uuid,
    definition: &PipelineDefinition,
) -> Result<(), ApiError> {
    // Only published predecessors can be referenced, so cycles cannot be introduced.
    // Bound the entire transitive graph and disallow diamonds (one clear input lineage).
    let mut pending = definition
        .on_success
        .iter()
        .cloned()
        .map(|target| (target, definition.artifacts.clone()))
        .collect::<Vec<_>>();
    let mut seen = std::collections::BTreeSet::new();
    while let Some((target, parent_artifacts)) = pending.pop() {
        if !seen.insert(target.pipeline_id) || seen.len() > 64 {
            return Err(ApiError::bad_request(
                "downstream graph must be a tree of at most 64 pipelines",
            ));
        }
        let child:Option<DbJson<PipelineDefinition>>=sqlx::query_scalar("SELECT r.definition FROM pipeline_revisions r JOIN pipelines p ON p.id=r.pipeline_id WHERE r.pipeline_id=$1 AND r.version=$2 AND p.project_id=$3")
            .bind(target.pipeline_id).bind(target.version).bind(project).fetch_optional(&mut *connection).await?;
        let child = child.ok_or_else(|| {
            ApiError::bad_request("downstream version must already be published in this project")
        })?;
        // A submitted changelist belongs to a Perforce connection, not a view.
        // The child takes its own immutable snapshot using its narrower build view.
        let same_connection: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM perforce_roots parent JOIN perforce_roots child ON child.connection_id=parent.connection_id WHERE parent.id=$1 AND child.id=$2 AND parent.project_id=$3 AND child.project_id=$3)",
        )
        .bind(definition.source_root_id)
        .bind(child.source_root_id)
        .bind(project)
        .fetch_one(&mut *connection)
        .await?;
        if !same_connection {
            return Err(ApiError::bad_request(
                "downstream pipelines must use the same Perforce connection",
            ));
        }
        for name in &child.artifact_inputs {
            if !parent_artifacts
                .iter()
                .any(|artifact| &artifact.name == name)
            {
                return Err(ApiError::bad_request(format!(
                    "downstream requires missing artifact: {name}"
                )));
            }
        }
        pending.extend(
            child
                .on_success
                .iter()
                .cloned()
                .map(|target| (target, child.artifacts.clone())),
        );
    }
    if definition.publish_generated.is_some() {
        let root: Value = sqlx::query_scalar("SELECT definition FROM perforce_roots WHERE id=$1")
            .bind(definition.source_root_id)
            .fetch_one(&mut *connection)
            .await?;
        if root["kind"] != "stream" {
            return Err(ApiError::bad_request("publication requires a stream root"));
        }
    }
    Ok(())
}

pub async fn tick(state: &AppState) -> Result<(), ApiError> {
    // Deterministic unique records survive server restarts and simultaneous schedulers.
    sqlx::query("INSERT INTO run_followups(id,parent_run_id,pipeline_id,version) SELECT gen_random_uuid(),r.id,(t->>'pipeline_id')::uuid,(t->>'version')::bigint FROM runs r CROSS JOIN LATERAL jsonb_array_elements(COALESCE(r.definition->'on_success','[]'::jsonb)) t WHERE r.status='succeeded' AND NOT r.cancel_requested ON CONFLICT(parent_run_id,pipeline_id) DO NOTHING").execute(&state.pool).await?;
    let rows:Vec<(Uuid,Uuid,Uuid,i64,Uuid,i64)>=sqlx::query_as("SELECT f.id,f.parent_run_id,f.pipeline_id,f.version,r.submitted_by,COALESCE(p.changelist,(r.source_snapshot->>'changelist')::bigint) FROM run_followups f JOIN runs r ON r.id=f.parent_run_id LEFT JOIN run_publications p ON p.run_id=r.id AND p.status IN ('submitted','unchanged') WHERE f.status='pending' ORDER BY r.created_at LIMIT 16")
        .fetch_all(&state.pool).await?;
    for (request, parent, pipeline, version, author, changelist) in rows {
        let user: Option<User> =
            sqlx::query_as("SELECT id,email,name,is_admin FROM users WHERE id=$1 AND active")
                .bind(author)
                .fetch_optional(&state.pool)
                .await?;
        let Some(user) = user else {
            block(state, request, "Submitting user is inactive").await?;
            continue;
        };
        let needs_inputs: bool = sqlx::query_scalar("SELECT jsonb_array_length(COALESCE(definition->'artifact_inputs','[]'::jsonb))>0 FROM pipeline_revisions WHERE pipeline_id=$1 AND version=$2").bind(pipeline).bind(version).fetch_one(&state.pool).await?;
        let session = Session {
            user,
            token_hash: vec![],
        };
        match store::create_run(
            State(state.clone()),
            Extension(session),
            Path(pipeline),
            Json(store::RunInput {
                request_id: request,
                source_run_id: needs_inputs.then_some(parent),
                version: Some(version),
                changelist,
            }),
        )
        .await
        {
            Ok(Json(child)) => {
                sqlx::query("UPDATE run_followups SET child_run_id=$2,status='created',reason='' WHERE id=$1 AND status='pending'").bind(request).bind(child.id).execute(&state.pool).await?;
                tracing::info!(%parent,child=%child.id,%changelist,"downstream run created");
            }
            Err(error) if error.0.is_client_error() => block(state, request, &error.1).await?,
            Err(error) => {
                tracing::warn!(%parent,error=%error.1,"downstream creation will retry with same request ID")
            }
        }
    }
    Ok(())
}
async fn block(state: &AppState, id: Uuid, reason: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE run_followups SET status='blocked',reason=$2 WHERE id=$1 AND status='pending'",
    )
    .bind(id)
    .bind(reason)
    .execute(&state.pool)
    .await?;
    Ok(())
}
pub async fn details(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let _ = store::run_detail(State(state.clone()), Extension(session), Path(id)).await?;
    let publication: Option<Value> =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM run_publications p WHERE run_id=$1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    let children:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('pipeline_id',f.pipeline_id,'pipeline_name',p.name,'version',f.version,'run_id',f.child_run_id,'status',f.status,'reason',f.reason) FROM run_followups f JOIN pipelines p ON p.id=f.pipeline_id WHERE parent_run_id=$1 ORDER BY p.name").bind(id).fetch_all(&state.pool).await?;
    let parent: Option<Uuid> =
        sqlx::query_scalar("SELECT parent_run_id FROM run_followups WHERE child_run_id=$1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    Ok(Json(
        json!({"publication":publication,"children":children,"parent_run_id":parent}),
    ))
}
