//! Self-hosted ticket broker. Passwords stay in server-side secret files; only
//! explicitly bound agents receive the corresponding finite-lived ticket.
use crate::{AppState, agents::Agent, auth::Session, error::ApiError, store};
use anyhow::{Context, Result, ensure};
use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, HeaderValue, header},
};
use chrono::{DateTime, Utc};
use p4forge_perforce::{
    Connection, Perforce,
    credentials::{MIN_VALID_SECONDS, atomic_private_file, validate_ticket_file},
};
use p4forge_protocol::{TicketGrant, TicketHealth};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, types::Json as DbJson};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub id: Uuid,
    pub project_id: Uuid,
    pub role: String,
    pub connection: Connection,
    pub server_id: String,
    pub password_file: PathBuf,
    pub agents: Vec<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub credentials: Vec<Policy>,
}

pub struct Broker {
    _process_lock: std::fs::File,
    config: Config,
    directory: PathBuf,
    grants: RwLock<Vec<TicketGrant>>,
    tick_lock: Mutex<()>,
}
impl Broker {
    pub async fn load(pool: &PgPool, path: &Path, directory: &Path) -> Result<Arc<Self>> {
        let config: Config = serde_json::from_slice(&private_read(path)?)
            .map_err(|_| anyhow::anyhow!("invalid credential policy"))?;
        ensure!(
            directory.is_absolute(),
            "credential directory must be absolute"
        );
        std::fs::create_dir_all(directory)?;
        ensure!(
            std::fs::symlink_metadata(directory)?.is_dir(),
            "invalid credential directory"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        ensure!(
            std::fs::symlink_metadata(directory)?.is_dir(),
            "invalid credential directory"
        );
        let process_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("broker.lock"))?;
        process_lock
            .try_lock()
            .context("another credential broker owns this directory")?;
        ensure!(
            !config.credentials.is_empty() && config.credentials.len() <= 32,
            "one to 32 credentials required"
        );
        let mut ids = HashSet::new();
        let mut bindings = HashSet::new();
        let mut grants = Vec::new();
        let mut tx = pool.begin().await?;
        for p in &config.credentials {
            p.connection.validate()?;
            ensure!(
                !p.id.is_nil() && ids.insert(p.id) && matches!(p.role.as_str(), "read" | "publish"),
                "invalid credential identity or role"
            );
            ensure!(
                !p.server_id.is_empty() && p.server_id.len() <= 128 && !p.agents.is_empty(),
                "server identity and agent bindings required"
            );
            ensure!(
                p.password_file.is_absolute(),
                "absolute password path required"
            );
            private_read(&p.password_file)?;
            let project_exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id=$1)")
                    .bind(p.project_id)
                    .fetch_one(&mut *tx)
                    .await?;
            ensure!(project_exists, "credential audit project must exist");
            let generation = Uuid::new_v4();
            for agent in &p.agents {
                ensure!(
                    bindings.insert((*agent, p.role.clone())),
                    "one credential per agent role is supported"
                );
                let updated = sqlx::query("UPDATE agents SET perforce_managed=true WHERE id=$1")
                    .bind(agent)
                    .execute(&mut *tx)
                    .await?;
                ensure!(updated.rows_affected() == 1, "credential agent must exist");
                sqlx::query("INSERT INTO agent_perforce_credentials(agent_id,credential_id,role,connection,server_id,generation) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(agent_id,credential_id) DO UPDATE SET role=$3,connection=$4,server_id=$5,generation=$6,server_state='checking',agent_state='checking',server_expires_at=NULL,agent_expires_at=NULL")
                    .bind(agent).bind(p.id).bind(&p.role).bind(DbJson(&p.connection)).bind(&p.server_id).bind(generation).execute(&mut *tx).await?;
            }
            grants.push(TicketGrant {
                id: p.id,
                generation,
                role: p.role.clone(),
                connection: p.connection.clone(),
                server_id: p.server_id.clone(),
                tickets: None,
            });
        }
        // Removed bindings lose access. The agent's managed flag deliberately
        // remains set so removing the policy cannot silently bypass dispatch checks.
        let rows: Vec<(Uuid, Uuid)> =
            sqlx::query_as("SELECT agent_id,credential_id FROM agent_perforce_credentials")
                .fetch_all(&mut *tx)
                .await?;
        for (agent, id) in rows {
            if !config
                .credentials
                .iter()
                .any(|p| p.id == id && p.agents.contains(&agent))
            {
                sqlx::query(
                    "DELETE FROM agent_perforce_credentials WHERE agent_id=$1 AND credential_id=$2",
                )
                .bind(agent)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(Arc::new(Self {
            _process_lock: process_lock,
            config,
            directory: directory.into(),
            grants: RwLock::new(grants),
            tick_lock: Mutex::new(()),
        }))
    }
    pub fn read_tickets_path(&self) -> PathBuf {
        self.directory.join("p4tickets")
    }

    pub async fn tick(&self, pool: &PgPool, base: &Perforce) -> Result<()> {
        let _lock = self.tick_lock.lock().await;
        for policy in &self.config.credentials {
            let result = self.refresh(policy, base).await;
            let mut grants = self.grants.write().await;
            let grant = grants
                .iter_mut()
                .find(|g| g.id == policy.id)
                .context("credential missing")?;
            let (state, expires, renewed) = match result {
                Ok((ticket, remaining, renewed)) => {
                    if grant.tickets.as_ref() != Some(&ticket) {
                        grant.generation = Uuid::new_v4();
                    }
                    grant.tickets = Some(ticket);
                    (
                        "ready",
                        Some(Utc::now() + chrono::Duration::seconds(remaining as i64)),
                        renewed,
                    )
                }
                Err(_) => {
                    // Do not expose CLI output, filesystem errors, or secret values.
                    grant.tickets = None;
                    ("error", None, false)
                }
            };
            let mut tx = pool.begin().await?;
            let previous: Option<String> = sqlx::query_scalar("SELECT server_state FROM agent_perforce_credentials WHERE credential_id=$1 LIMIT 1").bind(policy.id).fetch_optional(&mut *tx).await?;
            sqlx::query("UPDATE agent_perforce_credentials SET server_state=$2,server_checked_at=now(),server_expires_at=$3,renewed_at=CASE WHEN $4 THEN now() ELSE renewed_at END,agent_state=CASE WHEN generation<>$5 THEN 'checking' ELSE agent_state END,generation=$5 WHERE credential_id=$1")
                .bind(policy.id).bind(state).bind(expires).bind(renewed).bind(grant.generation).execute(&mut *tx).await?;
            if renewed || previous.as_deref() != Some(state) {
                sqlx::query("INSERT INTO audit_events(project_id,action,details) VALUES($1,'perforce.credential_state',$2)")
                    .bind(policy.project_id).bind(json!({"credential_id":policy.id,"role":policy.role,"state":state,"renewed":renewed})).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            // Server snapshot/trigger operations use only read credentials.
            let tickets = grants
                .iter()
                .filter(|g| g.role == "read")
                .filter_map(|g| g.tickets.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            atomic_private_file(&self.read_tickets_path(), tickets.as_bytes())?;
        }
        Ok(())
    }
    async fn refresh(&self, policy: &Policy, base: &Perforce) -> Result<(String, u64, bool)> {
        let path = self.directory.join(format!("{}.tickets", policy.id));
        if !path.exists() {
            atomic_private_file(&path, b"")?;
        }
        let p4 = Perforce {
            tickets_file: Some(path.clone()),
            ..base.clone()
        };
        // Verify the trusted destination before sending its password.
        let info = p4.tagged(&policy.connection, &["info", "-s"]).await?;
        ensure!(
            info.get("ServerID")
                .or_else(|| info.get("serverID"))
                .or_else(|| info.get("serverAddress"))
                == Some(&policy.server_id),
            "server identity mismatch"
        );
        let remaining = p4.ticket_lifetime(&policy.connection).await.unwrap_or(0);
        // Each login requires the password on this installation. Renew below four
        // hours; a one-minute loop retries failures without busy-looping.
        let renewed = remaining < 4 * 3600;
        let remaining = if renewed {
            let data = private_read(&policy.password_file)?;
            let password = std::str::from_utf8(&data)
                .context("invalid credential file")?
                .trim_end_matches(['\r', '\n']);
            p4.renew_ticket(&policy.connection, password).await?
        } else {
            remaining
        };
        ensure!(
            remaining > MIN_VALID_SECONDS && remaining <= 10 * 365 * 86400,
            "ticket lifetime outside supported range"
        );
        let mut ticket =
            String::from_utf8(private_read(&path)?).context("invalid ticket encoding")?;
        validate_ticket_file(&ticket, &policy.connection.user)?;
        // Linux and native clients can key the same server differently. Retain
        // the server-reported key and include the explicitly configured P4PORT.
        let value = ticket
            .lines()
            .next()
            .and_then(|l| l.rsplit_once(':'))
            .context("missing ticket")?
            .1
            .to_owned();
        let alias = format!(
            "{}={}:{}",
            policy.connection.port, policy.connection.user, value
        );
        if !ticket.lines().any(|l| l == alias) {
            ticket.push('\n');
            ticket.push_str(&alias);
            ticket.push('\n');
        }
        ticket = ticket
            .lines()
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        validate_ticket_file(&ticket, &policy.connection.user)?;
        Ok((ticket, remaining, renewed))
    }
}
fn private_read(path: &Path) -> Result<Vec<u8>> {
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() && m.len() <= 16384,
        "credential file must be a bounded regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(m.mode() & 0o077 == 0, "credential files must be private");
    }
    Ok(std::fs::read(path)?)
}

pub async fn offer(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
) -> Result<(HeaderMap, Json<Vec<TicketGrant>>), ApiError> {
    let mut output = Vec::new();
    if let Some(broker) = &state.credentials {
        let grants = broker.grants.read().await;
        let ids:Vec<Uuid> = sqlx::query_scalar("SELECT c.credential_id FROM agent_perforce_credentials c JOIN agents a ON a.id=c.agent_id WHERE a.id=$1 AND a.active")
            .bind(agent.id).fetch_all(&state.pool).await?;
        for grant in grants.iter().filter(|g| ids.contains(&g.id)) {
            output.push(grant.clone());
        }
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((headers, Json(output)))
}
pub async fn report(
    State(state): State<AppState>,
    Extension(agent): Extension<Agent>,
    Json(input): Json<Vec<TicketHealth>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if input.len() > 2
        || input.iter().any(|h| {
            h.remaining_seconds > 10 * 365 * 86400 || (!h.ready && h.remaining_seconds != 0)
        })
    {
        return Err(ApiError::bad_request("invalid credential health report"));
    }
    let mut tx = state.pool.begin().await?;
    // Serialize with revocation and claims; no network calls under this lock.
    let active: bool = sqlx::query_scalar("SELECT active FROM agents WHERE id=$1 FOR UPDATE")
        .bind(agent.id)
        .fetch_one(&mut *tx)
        .await?;
    if !active {
        return Err(ApiError::unauthorized("agent revoked"));
    }
    for h in input {
        let ready = h.ready && h.remaining_seconds > MIN_VALID_SECONDS;
        let updated = sqlx::query("UPDATE agent_perforce_credentials SET agent_state=$4,agent_checked_at=now(),agent_expires_at=LEAST(server_expires_at,now()+make_interval(secs=>$5)) WHERE agent_id=$1 AND credential_id=$2 AND generation=$3")
            .bind(agent.id).bind(h.id).bind(h.generation).bind(if ready {"ready"} else {"error"}).bind(h.remaining_seconds as f64).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(ApiError::conflict(
                "credential grant changed; refresh required",
            ));
        }
    }
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Serialize, sqlx::FromRow)]
pub struct Health {
    agent_id: Uuid,
    credential_id: Uuid,
    role: String,
    connection: DbJson<Connection>,
    server_state: String,
    server_checked_at: Option<DateTime<Utc>>,
    server_expires_at: Option<DateTime<Utc>>,
    renewed_at: Option<DateTime<Utc>>,
    agent_state: String,
    agent_checked_at: Option<DateTime<Utc>>,
    agent_expires_at: Option<DateTime<Utc>>,
}
pub async fn list(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> Result<Json<Vec<Health>>, ApiError> {
    store::require_admin(&session.user)?;
    Ok(Json(sqlx::query_as("SELECT agent_id,credential_id,role,connection,server_state,server_checked_at,server_expires_at,renewed_at,agent_state,agent_checked_at,agent_expires_at FROM agent_perforce_credentials ORDER BY agent_id,role").fetch_all(&state.pool).await?))
}
