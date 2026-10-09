//! Advisory, read-only review of saved definitions. Publish, queue and claim remain authoritative.
use crate::{AppState, auth::Session, dispatch, error::ApiError, store::Pipeline};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Utc};
use p4forge_domain::{MAX_ARTIFACT_TOTAL, PipelineDefinition};
use p4forge_perforce::{Connection, SourceSnapshot};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction, types::Json as DbJson};
use uuid::Uuid;

type Tx<'a> = Transaction<'a, Postgres>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    from: Option<i64>,
    to: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    version: i64,
    changelist: i64,
    source_run_id: Option<Uuid>,
}

pub(crate) async fn access(
    tx: &mut Tx<'_>,
    session: &Session,
    id: Uuid,
) -> Result<(Pipeline, String), ApiError> {
    let pipeline: Pipeline = sqlx::query_as("SELECT p.id,p.project_id,p.name,p.current_version FROM pipelines p WHERE p.id=$1 AND ($2 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=p.project_id AND m.user_id=$3))")
        .bind(id).bind(session.user.is_admin).bind(session.user.id).fetch_optional(&mut **tx).await?.ok_or_else(ApiError::not_found)?;
    let role = if session.user.is_admin {
        "admin".to_owned()
    } else {
        sqlx::query_scalar("SELECT role FROM project_members WHERE project_id=$1 AND user_id=$2")
            .bind(pipeline.project_id)
            .bind(session.user.id)
            .fetch_one(&mut **tx)
            .await?
    };
    Ok((pipeline, role))
}
async fn revision(tx: &mut Tx<'_>, id: Uuid, version: i64) -> Result<Value, ApiError> {
    if version == 0 {
        return Ok(json!({"label":"빈 기준", "version":0, "definition":null}));
    }
    sqlx::query_scalar("SELECT jsonb_build_object('label','v' || version,'version',version,'definition',definition,'definition_hash',definition_hash,'created_at',created_at) FROM pipeline_revisions WHERE pipeline_id=$1 AND version=$2")
        .bind(id).bind(version).fetch_optional(&mut **tx).await?.ok_or_else(ApiError::not_found)
}
fn check(checks: &mut Vec<Value>, level: &str, code: &str, message: impl Into<String>) {
    checks.push(json!({"level":level,"code":code,"message":message.into()}));
}
fn blocked(checks: &[Value]) -> bool {
    checks.iter().any(|c| c["level"] == "error")
}

pub async fn compare(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Query(input): Query<Comparison>,
) -> Result<Json<Value>, ApiError> {
    if input.from.is_some_and(|v| v < 0) {
        return Err(ApiError::bad_request("from must be zero or positive"));
    }
    let target = match input.to.as_deref().unwrap_or("draft") {
        "draft" => None,
        value => Some(
            value
                .parse::<i64>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| ApiError::bad_request("to must be draft or a positive version"))?,
        ),
    };
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let (pipeline, role) = access(&mut tx, &session, id).await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await?;
    let mut checks = Vec::new();
    let after = if let Some(version) = target {
        revision(&mut tx, id, version).await?
    } else {
        let draft: Value = sqlx::query_scalar("SELECT jsonb_build_object('label','저장된 초안','generation',generation,'base_version',base_version,'definition',definition) FROM pipeline_drafts WHERE pipeline_id=$1")
            .bind(id).fetch_one(&mut *tx).await?;
        if draft["base_version"].as_i64() != Some(pipeline.current_version) {
            check(
                &mut checks,
                "error",
                "draft_conflict",
                "초안의 기준 버전이 현재 발행 버전과 다릅니다. 초안을 다시 확인하세요.",
            );
        }
        let configuration: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM configuration_drafts WHERE pipeline_id=$1)",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if configuration {
            check(
                &mut checks,
                "error",
                "configuration_mode",
                "프로젝트 구성 초안은 설정에서 실행 설정으로 변환한 뒤 비교할 수 있습니다.",
            );
        }
        draft
    };
    let from = input
        .from
        .unwrap_or_else(|| target.map_or(pipeline.current_version, |v| v - 1));
    let before = revision(&mut tx, id, from).await?;
    let summary = if after["definition"].is_null() {
        check(
            &mut checks,
            "error",
            "empty_draft",
            "비교할 실행 설정이 없습니다. 설정에서 초안을 저장하세요.",
        );
        Value::Null
    } else {
        let definition: PipelineDefinition =
            serde_json::from_value(after["definition"].clone()).map_err(ApiError::internal)?;
        summarize(&mut tx, &pipeline, &definition, now, &mut checks)
            .await?
            .0
    };
    let can_publish =
        target.is_none() && matches!(role.as_str(), "editor" | "admin") && !blocked(&checks);
    tx.commit().await?;
    Ok(Json(
        json!({"checked_at":now,"pipeline_id":id,"current_version":pipeline.current_version,"before":before,"after":after,"summary":summary,"checks":checks,"can_publish":can_publish}),
    ))
}

/// Returns a safe UI summary and the connection for source-run identity comparison.
pub(crate) async fn summarize(
    tx: &mut Tx<'_>,
    pipeline: &Pipeline,
    definition: &PipelineDefinition,
    now: DateTime<Utc>,
    checks: &mut Vec<Value>,
) -> Result<(Value, Option<Connection>), ApiError> {
    if let Err(error) = definition.validate() {
        check(checks, "error", "definition", error.to_string());
    }
    let root: Option<(String, Value, DbJson<Connection>)> = sqlx::query_as("SELECT r.name,r.definition,c.definition FROM perforce_roots r JOIN perforce_connections c ON c.id=r.connection_id WHERE r.id=$1 AND r.project_id=$2")
        .bind(definition.source_root_id).bind(pipeline.project_id).fetch_optional(&mut **tx).await?;
    if root.is_none() {
        check(
            checks,
            "error",
            "source_root",
            "이 프로젝트에서 소스 루트를 찾을 수 없습니다.",
        );
    }
    let mut followups = Vec::new();
    for target in &definition.on_success {
        let child: Option<(String, DbJson<PipelineDefinition>)> = sqlx::query_as("SELECT p.name,r.definition FROM pipelines p JOIN pipeline_revisions r ON r.pipeline_id=p.id AND r.version=$2 WHERE p.id=$1 AND p.project_id=$3")
            .bind(target.pipeline_id).bind(target.version).bind(pipeline.project_id).fetch_optional(&mut **tx).await?;
        if let Some((name, child)) = child {
            let missing: Vec<_> = child
                .artifact_inputs
                .iter()
                .filter(|name| !definition.artifacts.iter().any(|a| &a.name == *name))
                .collect();
            if !missing.is_empty() {
                check(
                    checks,
                    "error",
                    "followup_artifacts",
                    format!(
                        "후속 {name} v{}에 필요한 산출물이 없습니다: {}",
                        target.version,
                        missing
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
            followups.push(json!({"pipeline_id":target.pipeline_id,"name":name,"version":target.version,"artifact_inputs":child.artifact_inputs}));
        } else {
            check(
                checks,
                "error",
                "followup_missing",
                "같은 프로젝트에서 고정된 후속 발행 버전을 찾을 수 없습니다.",
            );
        }
    }
    let mut executor = None;
    let mut deployment = Value::Null;
    if let Some(config) = &definition.deployment {
        let row: Option<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'name',name,'generation',generation,'health',health,'active_run_id',active_run_id,'executor_configured',executor_agent_id IS NOT NULL,'recovery_required',EXISTS(SELECT 1 FROM run_deployments d WHERE d.run_id=active_run_id AND d.status='recovery_required')) FROM deployment_environments WHERE id=$1 AND project_id=$2 AND pipeline_id=$3")
            .bind(config.environment_id).bind(pipeline.project_id).bind(pipeline.id).fetch_optional(&mut **tx).await?;
        if let Some(row) = row {
            executor = sqlx::query_scalar(
                "SELECT executor_agent_id FROM deployment_environments WHERE id=$1",
            )
            .bind(config.environment_id)
            .fetch_one(&mut **tx)
            .await?;
            if executor.is_none() {
                check(
                    checks,
                    "warning",
                    "executor_missing",
                    "배포 환경에 실행 에이전트가 지정되지 않았습니다.",
                );
            }
            if !row["active_run_id"].is_null() {
                check(
                    checks,
                    "warning",
                    "environment_locked",
                    "배포 환경을 다른 실행이 사용 중입니다.",
                );
            }
            if row["recovery_required"] == true {
                check(
                    checks,
                    "warning",
                    "environment_recovery",
                    "배포 환경의 복구 확인이 필요합니다.",
                );
            }
            deployment = row;
            check(
                checks,
                "info",
                "deployment_approval",
                "배포 실행은 등록 후 별도의 승인과 환경 검사를 거칩니다.",
            );
        } else {
            check(
                checks,
                "error",
                "deployment_missing",
                "이 파이프라인에 연결된 배포 환경을 찾을 수 없습니다.",
            );
        }
    }
    let value = serde_json::to_value(definition).map_err(ApiError::internal)?;
    let root_definition = root.as_ref().map_or(Value::Null, |(_, r, _)| r.clone());
    let candidates = dispatch::preview_candidates(
        tx,
        &value,
        &root_definition,
        executor,
        definition.deployment.is_some(),
        now,
    )
    .await?;
    for (job_index, job) in candidates.iter().enumerate() {
        if job["available"] == 0 {
            check(
                checks,
                "warning",
                "agent_unavailable",
                format!(
                    "작업 {}: 현재 실행 조건을 만족하는 가용 에이전트가 없습니다. 등록 후 대기할 수 있습니다.",
                    job["name"].as_str().unwrap_or("")
                ),
            );
            checks.last_mut().unwrap()["job_index"] = json!(job_index);
        }
    }
    check(
        checks,
        "info",
        "advisory",
        "에이전트 가용성은 점검 시각의 참고 정보입니다. Perforce 인증·실제 소스 접근과 최종 배정 가능성은 실행 등록·배정 시 다시 검사합니다.",
    );
    let source = root
        .as_ref()
        .map(|(name, r, _)| json!({"id":definition.source_root_id,"name":name,"kind":r["kind"]}));
    Ok((
        json!({"source":source,"workspace":definition.workspace,"max_parallel_jobs":definition.max_parallel_jobs,"jobs":candidates,"artifact_inputs":definition.artifact_inputs,"artifacts":definition.artifacts,"followups":followups,"deployment":deployment,"publishes_generated":definition.publish_generated.is_some()}),
        root.map(|(_, _, c)| c.0),
    ))
}

pub async fn preflight(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Query(input): Query<Execution>,
) -> Result<Json<Value>, ApiError> {
    if input.version <= 0
        || input.changelist <= 0
        || input.source_run_id.is_some_and(|id| id.is_nil())
    {
        return Err(ApiError::bad_request(
            "positive version/changelist and a valid source_run_id are required",
        ));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let (pipeline, role) = access(&mut tx, &session, id).await?;
    if role == "viewer" {
        return Err(ApiError::forbidden("operator role required"));
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await?;
    let selected = revision(&mut tx, id, input.version).await?;
    let definition: PipelineDefinition =
        serde_json::from_value(selected["definition"].clone()).map_err(ApiError::internal)?;
    let mut checks = Vec::new();
    let (summary, connection) =
        summarize(&mut tx, &pipeline, &definition, now, &mut checks).await?;
    let mut artifacts = Vec::new();
    if definition.artifact_inputs.is_empty() {
        if input.source_run_id.is_some() {
            check(
                &mut checks,
                "error",
                "unexpected_source",
                "이 버전은 산출물 입력을 사용하지 않습니다. 제공 실행 ID를 비워 주세요.",
            );
        }
    } else if let Some(source) = input.source_run_id {
        let parent: Option<(String,bool,DbJson<SourceSnapshot>)> = sqlx::query_as("SELECT r.status,r.cancel_requested,r.source_snapshot FROM runs r JOIN pipelines p ON p.id=r.pipeline_id WHERE r.id=$1 AND p.project_id=$2")
            .bind(source).bind(pipeline.project_id).fetch_optional(&mut *tx).await?;
        // Keep inaccessible and missing sources indistinguishable.
        let (status, canceled, snapshot) = parent.ok_or_else(ApiError::not_found)?;
        if status != "succeeded" || canceled {
            check(
                &mut checks,
                "error",
                "source_status",
                "산출물 제공 실행이 성공 상태가 아닙니다.",
            );
        }
        let published: Option<i64> = sqlx::query_scalar("SELECT changelist FROM run_publications WHERE run_id=$1 AND status IN ('submitted','unchanged')").bind(source).fetch_optional(&mut *tx).await?.flatten();
        if published.unwrap_or(snapshot.changelist) != input.changelist {
            check(
                &mut checks,
                "error",
                "source_changelist",
                "CL이 제공 실행의 최종 제출 CL과 다릅니다.",
            );
        }
        if connection.as_ref() != Some(&snapshot.connection) {
            check(
                &mut checks,
                "error",
                "source_connection",
                "제공 실행과 현재 소스의 Perforce 연결이 다릅니다.",
            );
        }
        for name in &definition.artifact_inputs {
            let rows: Vec<(Uuid,i64,String)> = sqlx::query_as("SELECT a.id,a.size,a.sha256 FROM artifacts a JOIN job_attempts t ON t.id=a.attempt_id WHERE a.run_id=$1 AND a.name=$2 AND a.status='verified' AND t.status='succeeded' AND a.purge_requested_at IS NULL AND a.deleted_at IS NULL")
                .bind(source).bind(name).fetch_all(&mut *tx).await?;
            if rows.len() != 1 {
                check(
                    &mut checks,
                    "error",
                    "artifact_unavailable",
                    format!("검증된 산출물을 사용할 수 없습니다: {name}"),
                );
            } else {
                let (id, size, sha256) = &rows[0];
                artifacts.push(json!({"id":id,"name":name,"size":size,"sha256":sha256}));
            }
        }
        if artifacts
            .iter()
            .map(|a| a["size"].as_u64().unwrap_or(u64::MAX))
            .try_fold(0u64, u64::checked_add)
            .is_none_or(|size| size > MAX_ARTIFACT_TOTAL)
        {
            check(
                &mut checks,
                "error",
                "artifact_size",
                "입력 산출물 총 크기가 256 MiB를 초과합니다.",
            );
        }
        check(
            &mut checks,
            "info",
            "source_recheck",
            "산출물은 아직 고정되지 않았습니다. 실행 등록 시 Perforce 서버 ID·CL·보관 상태를 다시 확인하고 입력을 고정합니다.",
        );
    } else {
        check(
            &mut checks,
            "error",
            "source_required",
            "이 버전은 산출물 제공 실행 ID가 필요합니다.",
        );
    }
    let can_queue = !blocked(&checks);
    tx.commit().await?;
    Ok(Json(
        json!({"checked_at":now,"pipeline_id":id,"version":input.version,"definition_hash":selected["definition_hash"],"changelist":input.changelist,"source_run_id":input.source_run_id,"summary":summary,"artifacts":artifacts,"checks":checks,"can_queue":can_queue}),
    ))
}
