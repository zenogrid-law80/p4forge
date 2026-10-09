pub mod agents;
pub mod artifact_inputs;
pub mod artifacts;
pub mod auth;
pub mod configurations;
pub mod connections;
pub mod credentials;
pub mod deployments;
pub mod diagnostics;
pub mod dispatch;
pub mod error;
pub mod followups;
pub mod project_graph;
pub mod retention;
pub mod review;
pub mod run_chain;
pub mod store;
pub mod triggers;
pub mod updates;
pub mod workspace;

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use p4forge_perforce::Perforce;
use sqlx::PgPool;
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};

use auth::AuthService;
use error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub credentials: Option<std::sync::Arc<credentials::Broker>>,
    pub pool: PgPool,
    pub auth: AuthService,
    pub perforce: Perforce,
    pub artifact_dir: std::path::PathBuf,
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}

async fn require_login(
    State(auth): State<AuthService>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let session = auth
        .authenticate(request.method(), request.headers())
        .await?;
    request.extensions_mut().insert(session);
    Ok(next.run(request).await)
}

pub fn router(state: AppState) -> Router {
    let bootstrap_auth = state.auth.clone();
    let ready_pool = state.pool.clone();
    let api = Router::new()
        .route("/api/workspace/overview", get(workspace::overview))
        .route("/api/workspace/runs", get(workspace::runs))
        .route("/api/operations", get(workspace::operations))
        .route("/api/runs/{id}/dispatch", get(dispatch::get))
        .route("/api/runs/{id}/chain", get(run_chain::get))
        .route(
            "/api/projects/{id}/environments",
            get(deployments::list).post(deployments::create),
        )
        .route("/api/environments/{id}/recover", post(deployments::recover))
        .route(
            "/api/projects/{id}/deployment-agents",
            get(deployments::executors),
        )
        .route(
            "/api/environments/{id}/executor",
            post(deployments::bind_executor),
        )
        .route(
            "/api/environments/{id}/rollback",
            post(deployments::rollback),
        )
        .route(
            "/api/runs/{id}/deployment",
            get(deployments::get).post(deployments::decide),
        )
        .route(
            "/api/pipelines/{id}/trigger",
            get(triggers::get).put(triggers::save),
        )
        .route("/api/perforce-credentials", get(credentials::list))
        .route(
            "/api/projects/{id}/artifact-retention",
            get(retention::get).put(retention::save),
        )
        .route(
            "/api/projects/{id}/artifact-retention/preview",
            post(retention::preview),
        )
        .route(
            "/api/projects/{id}/configuration",
            get(configurations::export).post(configurations::import),
        )
        .route(
            "/api/projects/{id}/configuration/source",
            axum::routing::put(configurations::bind_source),
        )
        .route(
            "/api/pipelines/{id}/configuration",
            axum::routing::put(configurations::save),
        )
        .route("/api/agents", get(agents::list).post(agents::create))
        .route("/api/agent-releases", get(updates::releases))
        .route("/api/agent-updates", get(updates::list))
        .route("/api/agents/{id}/update", post(updates::request))
        .route("/api/agent-updates/{id}/cancel", post(updates::cancel))
        .route("/api/agents/{id}", axum::routing::put(agents::control))
        .route("/api/runs/{id}/jobs", get(agents::jobs))
        .route("/api/runs/{id}/outputs", get(followups::details))
        .route("/api/runs/{id}/artifacts", get(artifacts::list))
        .route("/api/runs/{id}/inputs", get(artifact_inputs::list))
        .route("/api/runs/{run}/artifacts/{id}", get(artifacts::download))
        .route("/api/attempts/{id}/logs", get(agents::logs))
        .route("/api/me", get(store::me))
        .route("/api/users", get(store::users))
        .route(
            "/api/projects",
            get(store::projects).post(store::create_project),
        )
        .route(
            "/api/projects/{id}/members",
            get(store::members).put(store::set_member),
        )
        .route(
            "/api/connections",
            get(store::connections).post(store::create_connection),
        )
        .route("/api/connections/{id}/check", post(store::check_connection))
        .route(
            "/api/projects/{id}/roots",
            get(store::roots).post(store::create_root),
        )
        .route(
            "/api/projects/{id}/pipelines",
            get(store::pipelines).post(store::create_pipeline),
        )
        .route("/api/pipelines/{id}", get(store::pipeline_detail))
        .route("/api/pipelines/{id}/connections", get(connections::get))
        .route("/api/projects/{id}/graph", get(project_graph::get))
        .route("/api/pipelines/{id}/review", get(review::compare))
        .route(
            "/api/pipelines/{id}/diagnostics",
            get(diagnostics::saved).post(diagnostics::preview),
        )
        .route("/api/pipelines/{id}/preflight", get(review::preflight))
        .route(
            "/api/pipelines/{id}/draft",
            axum::routing::put(store::save_draft),
        )
        .route("/api/pipelines/{id}/validate", post(store::validate))
        .route(
            "/api/pipelines/{id}/definition-preview",
            post(store::preview_definition),
        )
        .route("/api/pipelines/{id}/publish", post(store::publish))
        .route("/api/pipelines/{id}/revisions", get(store::revisions))
        .route("/api/pipelines/{id}/restore", post(store::restore))
        .route(
            "/api/pipelines/{id}/runs",
            get(store::runs).post(store::create_run),
        )
        .route("/api/runs/{id}", get(store::run_detail))
        .route("/api/runs/{id}/cancel", post(store::cancel_run))
        .route_layer(middleware::from_fn_with_state(
            state.auth.clone(),
            require_login,
        ))
        .with_state(state.clone());
    let auth = Router::new()
        .route("/auth/google/login", get(auth::login))
        .route("/auth/google/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .with_state(state.auth.clone());
    let agent_api = Router::new()
        .route("/agent/credentials", post(credentials::offer))
        .route(
            "/agent/attempts/{attempt}/inputs/{id}",
            post(artifact_inputs::read),
        )
        .route("/agent/credentials/status", post(credentials::report))
        .route("/agent/update", post(updates::poll))
        .route("/agent/updates/{id}/binary", get(updates::binary))
        .route("/agent/claim", post(agents::claim))
        .route("/agent/docker-status", post(agents::docker_status))
        .route("/agent/attempts/{id}/artifacts", post(artifacts::manifest))
        .route(
            "/agent/attempts/{attempt}/artifacts/{id}",
            post(artifacts::chunk),
        )
        .route("/agent/attempts/{id}/heartbeat", post(agents::heartbeat))
        .route(
            "/agent/attempts/{id}/deployment",
            post(deployments::authorize_attempt),
        )
        .route("/agent/attempts/{id}/logs", post(agents::append_log))
        .route("/agent/attempts/{id}/complete", post(agents::complete))
        .route("/agent/attempts/{id}/step", post(agents::step))
        .route(
            "/agent/attempts/{id}/publication",
            post(agents::publication),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            agents::authenticate,
        ))
        .with_state(state.clone());
    Router::new().merge(api).merge(auth).merge(agent_api)
        .route("/",get(|| async { Html(include_str!("../../../web/index.html")) }))
        .route("/app.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/app.js")) }))
        .route("/run-graph.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/run-graph.js")) }))
        .route("/agent-status.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/agent-status.js")) }))
        .route("/navigation.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/navigation.js")) }))
        .route("/workspace.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/workspace.js")) }))
        .route("/dispatch.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/dispatch.js")) }))
        .route("/run-chain.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/run-chain.js")) }))
        .route("/pipeline-connections.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/pipeline-connections.js")) }))
        .route("/project-graph.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/project-graph.js")) }))
        .route("/pipeline-review.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/pipeline-review.js")) }))
        .route("/pipeline-editor.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/pipeline-editor.js")) }))
        .route("/editor-diagnostics.js",get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("../../../web/editor-diagnostics.js")) }))
        .route("/app.css",get(|| async { ([(header::CONTENT_TYPE,"text/css; charset=utf-8")],include_str!("../../../web/app.css")) }))
        .route("/api/bootstrap",get(move || { let configured=bootstrap_auth.configured(); let login_url=bootstrap_auth.config.public_url.join("auth/google/login").expect("validated origin").to_string(); async move { axum::Json(serde_json::json!({"google_configured":configured,"login_url":login_url,"workspace_domain":auth::ALLOWED_DOMAIN,"release":"linux-agent-preview","execution_available":true})) } }))
        .route("/healthz",get(|| async { "ok" }))
        .route("/readyz",get(move || { let pool=ready_pool.clone(); async move { if sqlx::query("SELECT 1").execute(&pool).await.is_ok() { StatusCode::OK.into_response() } else { StatusCode::SERVICE_UNAVAILABLE.into_response() } } }))
        .layer(DefaultBodyLimit::max(512*1024))
        .layer(SetResponseHeaderLayer::overriding(header::CACHE_CONTROL,HeaderValue::from_static("no-store")))
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS,HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(header::CONTENT_SECURITY_POLICY,HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")))
        .layer(TraceLayer::new_for_http().make_span_with(|request: &Request| {
            let route = request.extensions().get::<axum::extract::MatchedPath>().map_or("<unmatched>", |path| path.as_str());
            tracing::info_span!("http_request", method = %request.method(), route)
        }))
}
