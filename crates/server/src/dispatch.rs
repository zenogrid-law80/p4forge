//! Read-only observations. Claims still recheck authority and capacity under locks.
use crate::{AppState, auth::Session, error::ApiError};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(FromRow)]
struct Run {
    status: String,
    cancel_requested: bool,
    definition: Value,
    source_snapshot: Value,
}
#[derive(FromRow)]
struct Job {
    id: Uuid,
    name: String,
    status: String,
    runner: Value,
    pending_dependencies: Value,
}
#[derive(FromRow)]
struct Deployment {
    status: String,
    active_run_id: Option<Uuid>,
    executor_agent_id: Option<Uuid>,
    approved_executor: Option<Uuid>,
    approver_valid: bool,
}
#[derive(FromRow)]
struct Candidate {
    id: Uuid,
    name: String,
    os: String,
    arch: String,
    pool: String,
    active: bool,
    draining: bool,
    last_seen_at: Option<DateTime<Utc>>,
    last_protocol_version: Option<i64>,
    protocol_reported_at: Option<DateTime<Utc>>,
    docker_state: String,
    docker_engine_os: Option<String>,
    docker_checked_at: Option<DateTime<Utc>>,
    perforce_managed: bool,
    busy: bool,
    read_ready: bool,
    publish_ready: bool,
}
fn fresh(value: Option<DateTime<Utc>>, now: DateTime<Utc>, seconds: i64) -> bool {
    value.is_some_and(|value| {
        let age = now.signed_duration_since(value);
        age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(seconds)
    })
}
fn required_protocol(definition: &Value, source: &Value, runner: &Value) -> i64 {
    let mut version = 1;
    if definition["workspace"] == "pipeline" {
        version = version.max(2);
    }
    if definition.get("publish_generated").is_some() {
        version = version.max(3);
    }
    if source["root"]["kind"] == "view" {
        version = version.max(4);
    }
    if definition["artifacts"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        version = version.max(5);
    }
    if definition["artifact_inputs"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        version = version.max(7);
    }
    if runner["docker"].as_str().is_some() {
        version = version.max(8);
    }
    version
}
impl Candidate {
    fn reasons(
        &self,
        runner: &Value,
        protocol: i64,
        publishes: bool,
        now: DateTime<Utc>,
    ) -> Vec<&'static str> {
        let mut reasons = self.capability_reasons(runner, protocol, now);
        if !self.active {
            return reasons;
        }
        if self.perforce_managed {
            if !self.read_ready {
                reasons.push("read_credentials");
            }
            if publishes && !self.publish_ready {
                reasons.push("publish_credentials");
            }
        }
        reasons
    }
    fn capability_reasons(
        &self,
        runner: &Value,
        protocol: i64,
        now: DateTime<Utc>,
    ) -> Vec<&'static str> {
        if !self.active {
            return vec!["revoked"];
        }
        let mut reasons = Vec::new();
        let online = self.last_seen_at.is_some_and(|seen| {
            let age = now.signed_duration_since(seen);
            age >= chrono::Duration::seconds(-30) && age < chrono::Duration::seconds(30)
        });
        if !online {
            reasons.push("offline");
        }
        if self.draining {
            reasons.push("draining");
        }
        if self.busy {
            reasons.push("busy");
        }
        let protocol = if self.perforce_managed {
            protocol.max(6)
        } else {
            protocol
        };
        if !fresh(self.protocol_reported_at, now, 150) || self.last_protocol_version.is_none() {
            reasons.push("protocol_unknown");
        } else if self
            .last_protocol_version
            .is_some_and(|value| value < protocol)
        {
            reasons.push("protocol_old");
        }
        if let Some(engine) = runner["docker"].as_str() {
            if !fresh(self.docker_checked_at, now, 150) {
                reasons.push("docker_stale");
            } else if self.docker_state != "available" {
                reasons.push("docker_unavailable");
            } else if self.docker_engine_os.as_deref() != Some(engine) {
                reasons.push("docker_engine");
            }
        }
        reasons
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
    // Scope the frozen run and every related observation to current project membership.
    let run: Run = sqlx::query_as("SELECT r.status,r.cancel_requested,r.definition,r.source_snapshot FROM runs r JOIN pipelines p ON p.id=r.pipeline_id WHERE r.id=$1 AND ($2 OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=p.project_id AND m.user_id=$3))")
        .bind(id).bind(session.user.is_admin).bind(session.user.id).fetch_optional(&mut *tx).await?.ok_or_else(ApiError::not_found)?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await?;
    let jobs: Vec<Job> = sqlx::query_as("SELECT j.id,j.name,j.status,j.definition->'runner' AS runner,COALESCE((SELECT jsonb_agg(jsonb_build_object('name',n.name,'status',n.status) ORDER BY n.position) FROM job_dependencies d JOIN job_runs n ON n.id=d.needs_id WHERE d.job_id=j.id AND n.status<>'succeeded'),'[]'::jsonb) AS pending_dependencies FROM job_runs j WHERE j.run_id=$1 ORDER BY j.position")
        .bind(id).fetch_all(&mut *tx).await?;
    let deployment: Option<Deployment> = sqlx::query_as("SELECT d.status,e.active_run_id,e.executor_agent_id,d.executor_agent_id AS approved_executor,COALESCE(u.active AND (u.is_admin OR EXISTS(SELECT 1 FROM project_members m WHERE m.project_id=e.project_id AND m.user_id=u.id AND m.role='admin')),false) AS approver_valid FROM run_deployments d JOIN deployment_environments e ON e.id=d.environment_id LEFT JOIN users u ON u.id=d.approved_by WHERE d.run_id=$1")
        .bind(id).fetch_optional(&mut *tx).await?;
    let candidates: Vec<Candidate> = sqlx::query_as("SELECT a.id,a.name,a.os,a.arch,a.pool,a.active,a.draining,a.last_seen_at,a.last_protocol_version,a.protocol_reported_at,a.docker_state,a.docker_engine_os,a.docker_checked_at,a.perforce_managed,
        EXISTS(SELECT 1 FROM job_attempts j WHERE j.agent_id=a.id AND j.status='running') AS busy,
        EXISTS(SELECT 1 FROM agent_perforce_credentials c WHERE c.agent_id=a.id AND c.role='read' AND jsonb_strip_nulls(c.connection)=jsonb_strip_nulls(r.source_snapshot->'connection') AND c.server_id=r.source_snapshot->>'server_id' AND c.server_state='ready' AND c.agent_state='ready' AND c.server_checked_at>now()-interval '150 seconds' AND c.agent_checked_at>now()-interval '150 seconds' AND LEAST(c.server_expires_at,c.agent_expires_at)>now()+interval '5 minutes') AS read_ready,
        EXISTS(SELECT 1 FROM agent_perforce_credentials c WHERE c.agent_id=a.id AND c.role='publish' AND c.connection->>'user'=r.definition->'publish_generated'->>'user' AND c.connection->>'port'=r.source_snapshot->'connection'->>'port' AND c.server_id=r.source_snapshot->>'server_id' AND c.server_state='ready' AND c.agent_state='ready' AND c.server_checked_at>now()-interval '150 seconds' AND c.agent_checked_at>now()-interval '150 seconds' AND LEAST(c.server_expires_at,c.agent_expires_at)>now()+interval '5 minutes') AS publish_ready
        FROM agents a CROSS JOIN runs r WHERE r.id=$1 ORDER BY a.name,a.id")
        .bind(id).fetch_all(&mut *tx).await?;
    let ongoing = matches!(run.status.as_str(), "queued" | "running");
    let mut run_issues = Vec::new();
    let mut executor = None;
    if ongoing && run.cancel_requested {
        run_issues.push("cancel_requested");
    }
    if run.definition.get("deployment").is_some() {
        if let Some(d) = &deployment {
            executor = d.executor_agent_id;
            if d.status == "recovery_required" {
                run_issues.push("recovery_required");
            }
            if ongoing {
                if d.status == "awaiting_approval" {
                    run_issues.push("awaiting_approval");
                }
                if d.executor_agent_id.is_none() {
                    run_issues.push("executor_missing");
                }
                if d.status == "approved" {
                    if d.active_run_id != Some(id) || d.approved_executor != d.executor_agent_id {
                        run_issues.push("environment_changed");
                    }
                    if !d.approver_valid {
                        run_issues.push("approval_invalid");
                    }
                } else if !matches!(
                    d.status.as_str(),
                    "awaiting_approval" | "running" | "recovery_required"
                ) {
                    run_issues.push("deployment_not_ready");
                }
            }
        } else if ongoing {
            run_issues.push("deployment_not_ready");
        }
    }
    let running = jobs.iter().filter(|job| job.status == "running").count();
    let max_parallel = run.definition["max_parallel_jobs"].as_u64().unwrap_or(1) as usize;
    let mut output = Vec::new();
    for job in jobs {
        let protocol = required_protocol(&run.definition, &run.source_snapshot, &job.runner);
        let waiting = ongoing && matches!(job.status.as_str(), "queued" | "blocked");
        let mut matches = Vec::new();
        let mut active = 0;
        let mut ready = 0;
        let mut blockers = BTreeMap::<&str, usize>::new();
        if waiting {
            for candidate in &candidates {
                if job.runner["os"] != candidate.os
                    || job.runner["arch"] != candidate.arch
                    || job.runner["pool"] != candidate.pool
                    || executor.is_some_and(|id| id != candidate.id)
                {
                    continue;
                }
                let reasons = candidate.reasons(
                    &job.runner,
                    protocol,
                    run.definition.get("publish_generated").is_some(),
                    now,
                );
                if candidate.active {
                    active += 1;
                }
                if reasons.is_empty() {
                    ready += 1;
                }
                for reason in &reasons {
                    *blockers.entry(reason).or_default() += 1;
                }
                matches.push((candidate, reasons));
            }
        }
        let reason = if !waiting {
            "not_waiting"
        } else if let Some(issue) = run_issues.first() {
            issue
        } else if !job
            .pending_dependencies
            .as_array()
            .is_some_and(|items| items.is_empty())
        {
            "dependencies"
        } else if job.status == "blocked" {
            "scheduler_transition"
        } else if running >= max_parallel {
            "parallel_limit"
        } else if active == 0 {
            "no_agents"
        } else if ready == 0 {
            "agents_unavailable"
        } else {
            "awaiting_claim"
        };
        matches.sort_by_key(|(candidate, reasons)| {
            (!reasons.is_empty(), !candidate.active, &candidate.name)
        });
        let details: Vec<Value> = if session.user.is_admin {
            matches.iter().take(50).map(|(a,reasons)| json!({"id":a.id,"name":a.name,"reasons":reasons,"protocol_version":a.last_protocol_version,"protocol_required":if a.perforce_managed {protocol.max(6)} else {protocol},"protocol_reported_at":a.protocol_reported_at})).collect()
        } else {
            Vec::new()
        };
        output.push(json!({"id":job.id,"name":job.name,"status":job.status,"runner":job.runner,"required_protocol":protocol,"reason":reason,"pending_dependencies":job.pending_dependencies,"candidates":if waiting {Some(json!({"registered":matches.len(),"active":active,"precheck_passed":ready,"blockers":blockers,"items":details}))} else {None}}));
    }
    tx.commit().await?;
    Ok(Json(
        json!({"checked_at":now,"run_status":run.status,"cancel_requested":run.cancel_requested,"run_issues":run_issues,"max_parallel_jobs":max_parallel,"running_jobs":running,"shared_workspace":run.definition["workspace"]=="pipeline","admin_details":session.user.is_admin,"jobs":output}),
    ))
}

/// Global agent observations only; no credentials or claim authorization is inferred.
pub(crate) async fn preview_candidates(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    definition: &Value,
    root: &Value,
    executor: Option<Uuid>,
    restricted: bool,
    now: DateTime<Utc>,
) -> Result<Vec<Value>, ApiError> {
    let candidates: Vec<Candidate> = sqlx::query_as("SELECT a.id,a.name,a.os,a.arch,a.pool,a.active,a.draining,a.last_seen_at,a.last_protocol_version,a.protocol_reported_at,a.docker_state,a.docker_engine_os,a.docker_checked_at,a.perforce_managed,EXISTS(SELECT 1 FROM job_attempts t WHERE t.agent_id=a.id AND t.status='running') AS busy,false AS read_ready,false AS publish_ready FROM agents a WHERE a.active")
        .fetch_all(&mut **tx).await?;
    let mut output = Vec::new();
    for job in definition["jobs"].as_array().into_iter().flatten() {
        let runner = &job["runner"];
        let protocol = required_protocol(definition, &json!({"root":root}), runner);
        let mut active = 0;
        let mut ready = 0;
        let mut blockers = BTreeMap::<&str, usize>::new();
        for candidate in &candidates {
            if runner["os"] != candidate.os
                || runner["arch"] != candidate.arch
                || runner["pool"] != candidate.pool
                || (restricted && executor != Some(candidate.id))
            {
                continue;
            }
            active += 1;
            let reasons = candidate.capability_reasons(runner, protocol, now);
            if reasons.is_empty() {
                ready += 1;
            }
            for reason in reasons {
                *blockers.entry(reason).or_default() += 1;
            }
        }
        output.push(json!({"name":job["name"],"runner":runner,"required_protocol":protocol,"active":active,"available":ready,"blockers":blockers}));
    }
    Ok(output)
}
