//! Permission-scoped workspace reads. No source definitions, tickets or secrets are returned.
use crate::{AppState, auth::Session, error::ApiError, store};
use axum::{
    Extension, Json,
    extract::{Query, State},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFilter {
    project: Option<Uuid>,
    state: Option<String>,
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}
#[derive(Serialize, FromRow)]
pub struct RunSummary {
    id: Uuid,
    project_id: Uuid,
    project_name: String,
    pipeline_id: Uuid,
    pipeline_name: String,
    version: i64,
    status: String,
    cancel_requested: bool,
    changelist: i64,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    deployment_status: Option<String>,
}
#[derive(Serialize)]
pub struct RunPage {
    items: Vec<RunSummary>,
    next_cursor: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    at: DateTime<Utc>,
    id: Uuid,
}
const RUN_SELECT: &str = "SELECT r.id,pr.id AS project_id,pr.name AS project_name,p.id AS pipeline_id,p.name AS pipeline_name,r.version,r.status,r.cancel_requested,(r.source_snapshot->>'changelist')::bigint AS changelist,r.created_at,r.finished_at,d.status AS deployment_status FROM runs r JOIN pipelines p ON p.id=r.pipeline_id JOIN projects pr ON pr.id=p.project_id LEFT JOIN run_deployments d ON d.run_id=r.id";

async fn run_page(
    tx: &mut Transaction<'_, Postgres>,
    session: &Session,
    filter: &RunFilter,
) -> Result<RunPage, ApiError> {
    let limit = filter.limit.unwrap_or(25);
    if !(1..=100).contains(&limit) || filter.q.as_ref().is_some_and(|q| q.chars().count() > 120) {
        return Err(ApiError::bad_request(
            "limit must be 1..100 and search at most 120 characters",
        ));
    }
    if filter.state.as_deref().is_some_and(|s| {
        !matches!(
            s,
            "queued"
                | "running"
                | "succeeded"
                | "failed"
                | "canceled"
                | "awaiting_approval"
                | "recovery_required"
        )
    }) {
        return Err(ApiError::bad_request("unsupported execution state"));
    }
    let cursor: Option<Cursor> = filter
        .cursor
        .as_ref()
        .map(|value| {
            if value.len() > 256 {
                return Err(ApiError::bad_request("invalid cursor"));
            }
            let bytes = URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|_| ApiError::bad_request("invalid cursor"))?;
            serde_json::from_slice(&bytes).map_err(|_| ApiError::bad_request("invalid cursor"))
        })
        .transpose()?;
    // Permission checks are inside every query, including pages reached through a cursor.
    let sql = format!("{RUN_SELECT} WHERE ($1 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=pr.id AND m.user_id=$2))
        AND ($3::uuid IS NULL OR pr.id=$3)
        AND ($4::text IS NULL OR ($4='queued' AND r.status='queued' AND d.status IS DISTINCT FROM 'awaiting_approval') OR ($4 IN ('awaiting_approval','recovery_required') AND d.status=$4) OR ($4 NOT IN ('queued','awaiting_approval','recovery_required') AND r.status=$4))
        AND strpos(lower(pr.name||' '||p.name||' '||r.id::text),lower($5))>0
        AND ($6::timestamptz IS NULL OR (r.created_at,r.id)<($6,$7::uuid))
        ORDER BY r.created_at DESC,r.id DESC LIMIT $8");
    let mut items: Vec<RunSummary> = sqlx::query_as(&sql)
        .bind(session.user.is_admin)
        .bind(session.user.id)
        .bind(filter.project)
        .bind(filter.state.as_deref())
        .bind(filter.q.as_deref().unwrap_or("").trim())
        .bind(cursor.as_ref().map(|c| c.at))
        .bind(cursor.as_ref().map(|c| c.id))
        .bind(limit + 1)
        .fetch_all(&mut **tx)
        .await?;
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_cursor = if more {
        let last = items.last().expect("positive page size");
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    at: last.created_at,
                    id: last.id,
                })
                .map_err(ApiError::internal)?,
            ),
        )
    } else {
        None
    };
    Ok(RunPage { items, next_cursor })
}
pub async fn runs(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Query(filter): Query<RunFilter>,
) -> Result<Json<RunPage>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let result = run_page(&mut tx, &session, &filter).await?;
    tx.commit().await?;
    Ok(Json(result))
}
#[derive(Serialize, FromRow)]
struct ProjectOverview {
    id: Uuid,
    name: String,
    pipeline_count: i64,
    queued: i64,
    running: i64,
    awaiting_approval: i64,
    recovery_required: i64,
    succeeded_24h: i64,
    failed_24h: i64,
}
pub async fn overview(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await?;
    let projects:Vec<ProjectOverview>=sqlx::query_as("SELECT pr.id,pr.name,(SELECT count(*) FROM pipelines p WHERE p.project_id=pr.id) AS pipeline_count,s.* FROM projects pr CROSS JOIN LATERAL (
        SELECT count(*) FILTER(WHERE r.status='queued' AND d.status IS DISTINCT FROM 'awaiting_approval') AS queued,
        count(*) FILTER(WHERE r.status='running') AS running,
        count(*) FILTER(WHERE d.status='awaiting_approval') AS awaiting_approval,
        count(*) FILTER(WHERE d.status='recovery_required') AS recovery_required,
        count(*) FILTER(WHERE r.status='succeeded' AND r.finished_at>=now()-interval '24 hours') AS succeeded_24h,
        count(*) FILTER(WHERE r.status='failed' AND r.finished_at>=now()-interval '24 hours') AS failed_24h
        FROM runs r JOIN pipelines p ON p.id=r.pipeline_id LEFT JOIN run_deployments d ON d.run_id=r.id WHERE p.project_id=pr.id
    ) s WHERE $1 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=pr.id AND m.user_id=$2) ORDER BY pr.name,pr.id")
        .bind(session.user.is_admin).bind(session.user.id).fetch_all(&mut *tx).await?;
    let summary = json!({"projects":projects.len(),"pipelines":projects.iter().map(|p|p.pipeline_count).sum::<i64>(),"queued":projects.iter().map(|p|p.queued).sum::<i64>(),"running":projects.iter().map(|p|p.running).sum::<i64>(),"awaiting_approval":projects.iter().map(|p|p.awaiting_approval).sum::<i64>(),"recovery_required":projects.iter().map(|p|p.recovery_required).sum::<i64>(),"succeeded_24h":projects.iter().map(|p|p.succeeded_24h).sum::<i64>(),"failed_24h":projects.iter().map(|p|p.failed_24h).sum::<i64>()});
    let recent = run_page(
        &mut tx,
        &session,
        &RunFilter {
            limit: Some(8),
            ..Default::default()
        },
    )
    .await?;
    let attention_sql = format!(
        "{RUN_SELECT} WHERE d.status IN ('awaiting_approval','recovery_required') AND ($1 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=pr.id AND m.user_id=$2)) ORDER BY (d.status='recovery_required') DESC,r.created_at,r.id LIMIT 8"
    );
    let attention: Vec<RunSummary> = sqlx::query_as(&attention_sql)
        .bind(session.user.is_admin)
        .bind(session.user.id)
        .fetch_all(&mut *tx)
        .await?;
    let diagnostics = diagnostics(&mut tx, &session).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"checked_at":now,"summary":summary,"projects":projects,"recent":recent.items,"attention":attention,"diagnostics":diagnostics}),
    ))
}

// All three groups share the overview's read-only snapshot and permission scope.
// Counts cover the full scope; only the displayed rows are bounded.
async fn diagnostics(
    tx: &mut Transaction<'_, Postgres>,
    session: &Session,
) -> Result<Value, ApiError> {
    let actions: Value = sqlx::query_scalar(
        "WITH visible AS (
            SELECT r.id AS run_id,pr.id AS project_id,pr.name AS project_name,
                p.id AS pipeline_id,p.name AS pipeline_name,r.version,
                (r.source_snapshot->>'changelist')::bigint AS changelist,r.created_at,r.finished_at
            FROM runs r JOIN pipelines p ON p.id=r.pipeline_id JOIN projects pr ON pr.id=p.project_id
            WHERE $1 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=pr.id AND m.user_id=$2)
        ), items AS (
            SELECT v.*,d.status AS kind,d.run_id AS key,
                CASE d.status WHEN 'recovery_required' THEN 0 ELSE 2 END AS priority,
                NULL::uuid AS target_pipeline_id,NULL::text AS target_name,NULL::bigint AS target_version,
                CASE d.status WHEN 'recovery_required' THEN '외부 배포 상태 확인 후 환경 잠금을 처리해야 합니다.'
                    ELSE '배포 입력과 대상 환경을 검토한 뒤 승인할 수 있습니다.' END AS reason
            FROM visible v JOIN run_deployments d ON d.run_id=v.run_id
            JOIN deployment_environments e ON e.id=d.environment_id AND e.project_id=v.project_id
            WHERE d.status='awaiting_approval' OR (d.status='recovery_required' AND e.active_run_id=v.run_id)
            UNION ALL
            SELECT v.*,'followup_blocked',f.id,1,p.id,p.name,f.version,left(f.reason,1000)
            FROM visible v JOIN run_followups f ON f.parent_run_id=v.run_id
            JOIN pipelines p ON p.id=f.pipeline_id AND p.project_id=v.project_id
            WHERE f.status='blocked' AND f.child_run_id IS NULL
        ), top_items AS (SELECT * FROM items ORDER BY priority,created_at,run_id,key LIMIT 8)
        SELECT jsonb_build_object('total',(SELECT count(*) FROM items),'items',
            COALESCE((SELECT jsonb_agg(to_jsonb(i)-'priority' ORDER BY priority,created_at,run_id,key) FROM top_items i),'[]'::jsonb))",
    ).bind(session.user.is_admin).bind(session.user.id).fetch_one(&mut **tx).await?;
    let queued: Value = sqlx::query_scalar(
        "WITH items AS (
            SELECT r.id AS run_id,pr.id AS project_id,pr.name AS project_name,
                p.id AS pipeline_id,p.name AS pipeline_name,r.version,
                (r.source_snapshot->>'changelist')::bigint AS changelist,r.created_at,r.cancel_requested
            FROM runs r JOIN pipelines p ON p.id=r.pipeline_id JOIN projects pr ON pr.id=p.project_id
            LEFT JOIN run_deployments d ON d.run_id=r.id
            WHERE r.status='queued' AND d.status IS DISTINCT FROM 'awaiting_approval'
                AND ($1 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=pr.id AND m.user_id=$2))
        ), top_items AS (SELECT * FROM items ORDER BY created_at,run_id LIMIT 8)
        SELECT jsonb_build_object('total',(SELECT count(*) FROM items),'items',
            COALESCE((SELECT jsonb_agg(to_jsonb(i) ORDER BY created_at,run_id) FROM top_items i),'[]'::jsonb))",
    ).bind(session.user.is_admin).bind(session.user.id).fetch_one(&mut **tx).await?;
    let failed: Value = sqlx::query_scalar(
        "WITH items AS (
            SELECT r.id AS run_id,pr.id AS project_id,pr.name AS project_name,
                p.id AS pipeline_id,p.name AS pipeline_name,r.version,
                (r.source_snapshot->>'changelist')::bigint AS changelist,r.created_at,r.finished_at
            FROM runs r JOIN pipelines p ON p.id=r.pipeline_id JOIN projects pr ON pr.id=p.project_id
            WHERE r.status='failed' AND r.finished_at>=now()-interval '24 hours'
                AND ($1 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=pr.id AND m.user_id=$2))
        ), top_items AS (SELECT * FROM items ORDER BY finished_at DESC,run_id DESC LIMIT 8), details AS (
            SELECT i.*,j.id AS job_id,j.name AS job_name,j.status AS job_status,
                (SELECT count(*) FROM job_runs f WHERE f.run_id=i.run_id AND f.status IN ('failed','lost')) AS failed_jobs,
                CASE WHEN j.workspace_attempt_id IS NULL THEN a.exit_code ELSE j.step_exit_code END AS exit_code,
                left(CASE WHEN j.workspace_attempt_id IS NULL THEN a.reason ELSE j.step_reason END,1000) AS reason
            FROM top_items i LEFT JOIN LATERAL (
                SELECT * FROM job_runs j WHERE j.run_id=i.run_id AND j.status IN ('failed','lost') ORDER BY position,id LIMIT 1
            ) j ON true
            LEFT JOIN job_attempts a ON a.id=j.workspace_attempt_id OR (j.workspace_attempt_id IS NULL AND a.job_id=j.id)
        ) SELECT jsonb_build_object('total',(SELECT count(*) FROM items),'items',
            COALESCE((SELECT jsonb_agg(to_jsonb(d) ORDER BY finished_at DESC,run_id DESC) FROM details d),'[]'::jsonb))",
    ).bind(session.user.is_admin).bind(session.user.id).fetch_one(&mut **tx).await?;
    Ok(json!({"actions":actions,"queued":queued,"failed_24h":failed}))
}

pub async fn operations(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Value>, ApiError> {
    store::require_admin(&session.user)?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    // Docker absence is a capability, not an operational failure of a native runner.
    let counts:Value=sqlx::query_scalar("SELECT jsonb_build_object(
        'active_agents',(SELECT count(*) FROM agents WHERE active),
        'online_agents',(SELECT count(*) FROM agents WHERE active AND last_seen_at>now()-interval '30 seconds' AND last_seen_at<=now()+interval '30 seconds'),
        'draining_agents',(SELECT count(*) FROM agents WHERE active AND draining),
        'busy_agents',(SELECT count(DISTINCT j.agent_id) FROM job_attempts j JOIN agents a ON a.id=j.agent_id WHERE a.active AND j.status='running' AND j.lease_expires_at>now()),
        'docker_available',(SELECT count(*) FROM agents WHERE active AND last_seen_at>now()-interval '30 seconds' AND last_seen_at<=now()+interval '30 seconds' AND docker_state='available' AND docker_checked_at>=now()-interval '150 seconds' AND docker_checked_at<=now()),
        'checked_at',now())").fetch_one(&mut *tx).await?;
    let issues:Value=sqlx::query_scalar("WITH issues AS (
        SELECT 'agent_offline' AS kind,a.name AS title,'최근 연결이 없거나 30초 이상 지연되었습니다.' AS detail,a.id AS agent_id,NULL::uuid AS project_id,NULL::uuid AS pipeline_id,NULL::uuid AS run_id,a.last_seen_at AS checked_at FROM agents a WHERE active AND (last_seen_at IS NULL OR last_seen_at<=now()-interval '30 seconds' OR last_seen_at>now()+interval '30 seconds')
        UNION ALL
        SELECT 'credential',a.name,'읽기 인증이 없거나 인증 검사·만료 상태를 확인해야 합니다.',a.id,NULL,NULL,NULL,NULL FROM agents a WHERE a.active AND a.perforce_managed AND (
          NOT EXISTS(SELECT 1 FROM agent_perforce_credentials c WHERE c.agent_id=a.id AND c.role='read') OR EXISTS(SELECT 1 FROM agent_perforce_credentials c WHERE c.agent_id=a.id AND NOT COALESCE(c.server_state='ready' AND c.agent_state='ready' AND c.server_checked_at>now()-interval '150 seconds' AND c.agent_checked_at>now()-interval '150 seconds' AND c.server_expires_at>now()+interval '5 minutes' AND c.agent_expires_at>now()+interval '5 minutes',false)))
        UNION ALL
        SELECT 'trigger',pr.name||' / '||p.name,COALESCE(NULLIF(t.last_error,''),'변경 확인이 아직 없거나 150초 이상 지연되었습니다.'),NULL,pr.id,p.id,NULL,t.checked_at FROM pipeline_triggers t JOIN pipelines p ON p.id=t.pipeline_id JOIN projects pr ON pr.id=p.project_id WHERE t.enabled AND (t.last_error IS NOT NULL OR t.checked_at IS NULL OR t.checked_at<now()-interval '150 seconds')
        UNION ALL
        SELECT 'deployment',pr.name||' / '||e.name,'외부 배포 상태를 확인한 뒤 환경 잠금을 처리해야 합니다.',NULL,pr.id,e.pipeline_id,d.run_id,d.finished_at FROM deployment_environments e JOIN projects pr ON pr.id=e.project_id JOIN run_deployments d ON d.run_id=e.active_run_id WHERE d.status='recovery_required'
    ) SELECT jsonb_build_object('total',count(*),'offline_agents',count(*) FILTER(WHERE kind='agent_offline'),'credential_attention',count(*) FILTER(WHERE kind='credential'),'trigger_attention',count(*) FILTER(WHERE kind='trigger'),'recovery_required',count(*) FILTER(WHERE kind='deployment'),'items',COALESCE((SELECT jsonb_agg(to_jsonb(i)) FROM (SELECT * FROM issues ORDER BY CASE kind WHEN 'deployment' THEN 0 WHEN 'credential' THEN 1 ELSE 2 END,title,agent_id,pipeline_id LIMIT 50) i),'[]'::jsonb)) FROM issues").fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"counts":counts,"issues":issues})))
}
