//! Read-only diagnostics for saved drafts and unsaved editor input.
use crate::{AppState, auth::Session, error::ApiError, review};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use p4forge_domain::{DefinitionDiagnostic, MAX_CONFIG_BYTES, PipelineDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    toml: Option<String>,
    definition: Option<Value>,
    generation: i64,
    base_version: i64,
}
fn issue(category: &str, code: &str, field: &str, message: impl ToString) -> Value {
    json!({"level":"error","category":category,"code":code,"field":field,"message":message.to_string()})
}
fn parse(source: &str, issues: &mut Vec<Value>) -> Option<PipelineDefinition> {
    if source.len() > MAX_CONFIG_BYTES {
        issues.push(issue(
            "definition",
            "size",
            "toml",
            "설정은 256 KiB 이하여야 합니다.",
        ));
        return None;
    }
    match toml::from_str(source) {
        Ok(value) => Some(value),
        Err(error) => {
            let error: toml::de::Error = error;
            let mut offset = error.span().map_or(0, |span| span.start).min(source.len());
            while !source.is_char_boundary(offset) {
                offset -= 1;
            }
            let prefix = &source[..offset];
            let mut item = issue("syntax", "toml", "toml", error.message());
            item["line"] = json!(prefix.bytes().filter(|b| *b == b'\n').count() + 1);
            item["column"] = json!(prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1);
            item["offset"] = json!(prefix.encode_utf16().count());
            issues.push(item);
            None
        }
    }
}
pub async fn saved(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    inspect(&state, &session, id, None).await
}
pub async fn preview(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<Uuid>,
    Json(input): Json<Input>,
) -> Result<Json<Value>, ApiError> {
    inspect(&state, &session, id, Some(input)).await
}
async fn inspect(
    state: &AppState,
    session: &Session,
    id: Uuid,
    input: Option<Input>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let (pipeline, role) = review::access(&mut tx, session, id).await?;
    if input.is_some() && !matches!(role.as_str(), "editor" | "admin") {
        return Err(ApiError::forbidden("insufficient project permission"));
    }
    let checked_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await?;
    let (generation, base_version, stored): (i64, i64, Option<Value>) = sqlx::query_as(
        "SELECT generation,base_version,definition FROM pipeline_drafts WHERE pipeline_id=$1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let configuration: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM configuration_drafts WHERE pipeline_id=$1)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let mut issues = Vec::new();
    if configuration {
        issues.push(issue(
            "state",
            "configuration_mode",
            "configuration",
            "가져온 프로젝트 구성은 실행 정의 전환 전 상태입니다.",
        ));
    }
    let source = if input.is_some() { "editor" } else { "saved" };
    let definition = if let Some(input) = input {
        if input.generation != generation {
            issues.push(issue(
                "state",
                "draft_changed",
                "draft",
                "다른 사용자가 초안을 저장했습니다. 입력을 보관하고 최신 초안과 비교하세요.",
            ));
        }
        if input.base_version != pipeline.current_version {
            issues.push(issue(
                "state",
                "published_changed",
                "draft",
                "현재 발행 버전과 편집 기준이 다릅니다. 저장 전에 최신 버전을 확인하세요.",
            ));
        }
        match (input.toml, input.definition) {
            (Some(text), None) => parse(&text, &mut issues),
            (None, Some(value)) => {
                if serde_json::to_vec(&value)
                    .map_err(ApiError::internal)?
                    .len()
                    > MAX_CONFIG_BYTES
                {
                    issues.push(issue(
                        "definition",
                        "size",
                        "toml",
                        "설정은 256 KiB 이하여야 합니다.",
                    ));
                    None
                } else {
                    match serde_json::from_value(value) {
                        Ok(value) => Some(value),
                        Err(error) => {
                            issues.push(issue("definition", "shape", "toml", error));
                            None
                        }
                    }
                }
            }
            _ => {
                return Err(ApiError::bad_request(
                    "provide exactly one of toml or definition",
                ));
            }
        }
    } else {
        if base_version != pipeline.current_version {
            issues.push(issue(
                "state",
                "published_changed",
                "draft",
                "초안의 기준 버전이 현재 발행 버전과 다릅니다.",
            ));
        }
        match stored {
            Some(value) => Some(
                serde_json::from_value::<PipelineDefinition>(value).map_err(ApiError::internal)?,
            ),
            None => {
                issues.push(issue(
                    "state",
                    "empty_draft",
                    "toml",
                    "저장된 실행 초안이 없습니다.",
                ));
                None
            }
        }
    };
    let mut graph = Value::Null;
    let mut definition_hash = Value::Null;
    let mut normalized = Value::Null;
    if let Some(definition) = &definition {
        // Serialization alone is allowed for fixing invalid input in TOML. It never saves it.
        normalized = json!(toml::to_string_pretty(definition).map_err(ApiError::internal)?);
        match definition.validate() {
            Err(error) => {
                let mut item = issue("definition", "definition", "toml", &error);
                if let Some(location) = error.downcast_ref::<DefinitionDiagnostic>() {
                    item["field"] = json!(location.field);
                    item["job_index"] = json!(location.job_index);
                }
                issues.push(item);
                issues.push(json!({"level":"info","category":"reference","code":"not_checked","message":"정의 오류를 수정한 후 소스·후속·배포 참조와 에이전트 가용성을 검사합니다."}));
            }
            Ok(validated) => {
                graph = json!(validated);
                definition_hash = json!(definition.hash().map_err(ApiError::internal)?);
                let mut checks = Vec::new();
                review::summarize(&mut tx, &pipeline, definition, checked_at, &mut checks).await?;
                // Include transitive lineage, same-connection and publication-root rules
                // from saving/publishing, under this same read-only snapshot.
                if !checks.iter().any(|check| check["level"] == "error")
                    && let Err(error) = crate::followups::validate_with_connection(
                        &mut tx,
                        pipeline.project_id,
                        definition,
                    )
                    .await
                {
                    if !error.0.is_client_error() {
                        return Err(error);
                    }
                    let field = if error.1 == "publication requires a stream root" {
                        "publish_generated"
                    } else {
                        "on_success"
                    };
                    issues.push(issue("reference", "followup_validation", field, error.1));
                }
                for mut check in checks {
                    let (category, field) = match check["code"].as_str().unwrap_or("") {
                        "source_root" => ("reference", "source_root_id"),
                        "followup_artifacts" | "followup_missing" => ("reference", "on_success"),
                        "agent_unavailable" => ("availability", "runner"),
                        "advisory" => ("availability", ""),
                        _ => ("reference", "deployment"),
                    };
                    check["category"] = json!(category);
                    check["field"] = json!(field);
                    issues.push(check);
                }
            }
        }
    }
    tx.commit().await?;
    Ok(Json(
        json!({"pipeline_id":id,"source":source,"checked_at":checked_at,"current_version":pipeline.current_version,"generation":generation,"base_version":base_version,"valid":!issues.iter().any(|i|i["level"]=="error"),"issues":issues,"graph":graph,"definition_hash":definition_hash,"definition":definition,"toml":normalized}),
    ))
}
