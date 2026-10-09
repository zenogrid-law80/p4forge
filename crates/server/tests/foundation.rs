use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use p4forge_perforce::Perforce;
use p4forge_server::{
    AppState,
    auth::{AuthConfig, AuthService, random_token, token_hash},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

async fn agent_request(app: &Router, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn agent_ok(app: &Router, token: &str, path: &str, body: Value) -> Value {
    let (status, value) = agent_request(app, token, path, body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
async fn new_agent(app: &Router, user: &Login, name: &str) -> Value {
    ok(
        app,
        user,
        "POST",
        "/api/agents",
        json!({"name":name,"pool":"default","os":"linux","arch":"x86_64"}),
    )
    .await
}
async fn publish_definition(app: &Router, user: &Login, pipeline: &str, definition: &str) {
    let path = format!("/api/pipelines/{pipeline}");
    let draft = ok(app, user, "GET", &path, Value::Null).await["draft"].clone();
    let saved=ok(app,user,"PUT",&format!("{path}/draft"),json!({"toml":definition,"generation":draft["generation"],"base_version":draft["base_version"]})).await;
    ok(
        app,
        user,
        "POST",
        &format!("{path}/publish"),
        json!({"generation":saved["generation"],"base_version":saved["base_version"]}),
    )
    .await;
}
async fn fixture_run(pool: &PgPool, user: &Login, pipeline: &str) -> Uuid {
    fixture_run_with_root(
        pool,
        user,
        pipeline,
        json!({"kind":"stream","stream":"//streams/main"}),
    )
    .await
}
async fn fixture_run_with_root(pool: &PgPool, user: &Login, pipeline: &str, root: Value) -> Uuid {
    let pipeline = Uuid::parse_str(pipeline).unwrap();
    let run = Uuid::new_v4();
    let (version,definition):(i64,sqlx::types::Json<p4forge_domain::PipelineDefinition>)=sqlx::query_as("SELECT version,definition FROM pipeline_revisions WHERE pipeline_id=$1 ORDER BY version DESC LIMIT 1").bind(pipeline).fetch_one(pool).await.unwrap();
    let snapshot = json!({"connection":{"port":"127.0.0.1:16660","user":"p4forge"},"root":root,"changelist":2,"server_id":"fixture","server_version":"fixture","client_spec":{},"stream_specs":[]});
    sqlx::query("INSERT INTO runs(id,pipeline_id,version,submitted_by,request_id,request_hash,definition,source_snapshot,input_hash) VALUES($1,$2,$3,$4,$5,'fixture',$6,$7,'fixture')")
        .bind(run).bind(pipeline).bind(version).bind(user.id).bind(Uuid::new_v4()).bind(&definition).bind(snapshot).execute(pool).await.unwrap();
    let graph = definition.validate().unwrap();
    let mut ids = std::collections::BTreeMap::new();
    for (position, job) in definition.jobs.iter().enumerate() {
        let id = Uuid::new_v4();
        ids.insert(job.name.clone(), id);
        sqlx::query("INSERT INTO job_runs(id,run_id,name,position,definition,status) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(id).bind(run).bind(&job.name).bind(position as i32).bind(sqlx::types::Json(job)).bind(if graph.edges.iter().any(|(_,downstream)| downstream == &job.name){"blocked"}else{"queued"}).execute(pool).await.unwrap();
    }
    for (upstream, downstream) in graph.edges {
        sqlx::query("INSERT INTO job_dependencies(run_id,job_id,needs_id) VALUES($1,$2,$3)")
            .bind(run)
            .bind(ids[&downstream])
            .bind(ids[&upstream])
            .execute(pool)
            .await
            .unwrap();
    }
    run
}
fn claim_input() -> Value {
    json!({"request_id":Uuid::new_v4(),"os":"linux","arch":"x86_64"})
}

#[sqlx::test(migrations = "../../migrations")]
async fn agent_self_patch_is_admin_scoped_idle_only_and_restores_drain_state(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let (_project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo test")).unwrap();
    for job in &mut definition.jobs {
        job.runner.os = p4forge_domain::OperatingSystem::Windows;
        job.runner.arch = "x86_64".into();
        job.shell = p4forge_domain::Shell::PowerShell;
    }
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    fixture_run(&pool, &admin, &pipeline).await;
    let created = ok(
        &app,
        &admin,
        "POST",
        "/api/agents",
        json!({"name":"patch-agent","pool":"default","os":"windows","arch":"x86_64"}),
    )
    .await;
    let id = created["agent"]["id"].as_str().unwrap();
    let token = created["token"].as_str().unwrap();
    let release = p4forge_server::updates::register(
        &pool,
        "test-patch",
        "windows",
        "x86_64",
        b"test binary".to_vec(),
    )
    .await
    .unwrap();
    let mac = p4forge_server::updates::register(
        &pool,
        "test-mac",
        "macos",
        "aarch64",
        b"mac binary".to_vec(),
    )
    .await
    .unwrap();
    let endpoint = format!("/api/agents/{id}/update");
    let body = json!({"release_id":release.id});
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            "/api/agent-releases",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, Some(&viewer), "POST", &endpoint, body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, Some(&admin), "POST", &endpoint, body.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    let old = "a".repeat(64);
    let mut poll =
        json!({"sha256":old,"version":"old","ready":true,"pending_id":null,"receipt":null});
    agent_ok(&app, token, "/agent/update", poll.clone()).await;
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &endpoint,
            json!({"release_id":mac.id})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let claim =
        json!({"request_id":Uuid::new_v4(),"os":"windows","arch":"x86_64","protocol_version":4});
    let attempt = agent_ok(&app, token, "/agent/claim", claim).await["assignment"]["attempt_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let update = ok(&app, &admin, "POST", &endpoint, body.clone()).await;
    let update_id = update["id"].as_str().unwrap();
    assert_eq!(
        request(&app, Some(&admin), "POST", &endpoint, body.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert!(
        agent_ok(&app, token, "/agent/update", poll.clone()).await["offer"].is_null(),
        "active build must finish"
    );
    let controlled = ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/agents/{id}"),
        json!({"active":true,"draining":false}),
    )
    .await;
    assert_eq!(
        controlled["draining"], true,
        "manual resume must not race a patch"
    );
    sqlx::query("UPDATE job_attempts SET status='succeeded' WHERE id=$1")
        .bind(Uuid::parse_str(&attempt).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let offered = agent_ok(&app, token, "/agent/update", poll.clone()).await;
    assert_eq!(offered["offer"]["id"], update["id"]);
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/agent-updates/{update_id}/cancel"),
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let other = ok(
        &app,
        &admin,
        "POST",
        "/api/agents",
        json!({"name":"other-patch-agent","pool":"default","os":"windows","arch":"x86_64"}),
    )
    .await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/agent/updates/{update_id}/binary"))
                .header(
                    "authorization",
                    format!("Bearer {}", other["token"].as_str().unwrap()),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/agent/updates/{update_id}/binary"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(response.into_body(), 100).await.unwrap()[..],
        b"test binary"
    );
    poll["pending_id"] = json!(update_id);
    poll["sha256"] = json!(release.sha256);
    let healthy = agent_ok(&app, token, "/agent/update", poll.clone()).await;
    assert_eq!(healthy["healthy_id"], update["id"]);
    let state: bool = sqlx::query_scalar("SELECT draining FROM agents WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(state, "health ack alone must not resume builds");
    poll["pending_id"] = Value::Null;
    poll["receipt"] = json!({"id":update_id,"status":"succeeded","reason":""});
    for _ in 0..2 {
        assert_eq!(
            agent_ok(&app, token, "/agent/update", poll.clone()).await["receipt_accepted"],
            true
        );
    }
    let state: bool = sqlx::query_scalar("SELECT draining FROM agents WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!state);
    assert_eq!(
        request(&app, Some(&admin), "POST", &endpoint, body).await.0,
        StatusCode::BAD_REQUEST,
        "same binary is already installed"
    );

    // Cancellation and rollback both retain an operator's pre-existing drain setting.
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/agents/{id}"),
        json!({"active":true,"draining":true}),
    )
    .await;
    let next = p4forge_server::updates::register(
        &pool,
        "next",
        "windows",
        "x86_64",
        b"another binary".to_vec(),
    )
    .await
    .unwrap();
    let update = ok(
        &app,
        &admin,
        "POST",
        &endpoint,
        json!({"release_id":next.id}),
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!(
            "/api/agent-updates/{}/cancel",
            update["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    let update = ok(
        &app,
        &admin,
        "POST",
        &endpoint,
        json!({"release_id":next.id}),
    )
    .await;
    poll["receipt"] = Value::Null;
    let offered = agent_ok(&app, token, "/agent/update", poll.clone()).await;
    assert_eq!(offered["offer"]["id"], update["id"]);
    poll["receipt"] = json!({"id":update["id"],"status":"rolled_back","reason":"health timeout"});
    agent_ok(&app, token, "/agent/update", poll).await;
    let state: bool = sqlx::query_scalar("SELECT draining FROM agents WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(state);
}

#[sqlx::test(migrations = "../../migrations")]
async fn explicit_views_roundtrip_validate_and_require_capable_agents(pool: PgPool) {
    let app = app(&pool, Perforce::new("/no-p4-for-this-test"));
    let admin = login(&pool, true).await;
    let (project, _, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let path = format!("/api/projects/{project}/roots");
    let roots = ok(&app, &admin, "GET", &path, Value::Null).await;
    let definition =
        json!({"kind":"view","view":["//stream/root/Server/... ...","-//stream/root/Client/..."]});
    let saved = ok(&app, &admin, "POST", &path, json!({"name":"server-view","connection_id":roots[0]["connection_id"],"definition":definition})).await;
    assert_eq!(saved["definition"], definition);
    let listed = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == saved["id"] && r["definition"] == definition)
    );
    let invalid = request(&app, Some(&admin), "POST", &path, json!({"name":"invalid-view","connection_id":roots[0]["connection_id"],"definition":{"kind":"view","view":["//stream/root/... ../..."]}})).await;
    assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
    assert!(invalid.1["error"].as_str().unwrap().contains("View line 1"));
    let mut d = p4forge_domain::PipelineDefinition::parse(&toml(
        saved["id"].as_str().unwrap(),
        "echo view",
    ))
    .unwrap();
    for job in &mut d.jobs {
        job.runner.arch = "x86_64".into();
    }
    publish_definition(&app, &admin, &pipeline, &d.to_toml().unwrap()).await;
    let run = fixture_run_with_root(&pool, &admin, &pipeline, definition.clone()).await;
    let created = new_agent(&app, &admin, "view-agent").await;
    let token = created["token"].as_str().unwrap();
    for version in [0, 2, 3] {
        let mut input = claim_input();
        input["protocol_version"] = json!(version);
        assert!(agent_ok(&app, token, "/agent/claim", input).await["assignment"].is_null());
    }
    let mut input = claim_input();
    input["protocol_version"] = json!(4);
    let assignment =
        agent_ok(&app, token, "/agent/claim", input.clone()).await["assignment"].clone();
    assert_eq!(assignment["run_id"], run.to_string());
    assert_eq!(assignment["source"]["root"], definition);
    let retry = agent_ok(&app, token, "/agent/claim", input).await;
    assert_eq!(retry["assignment"]["attempt_id"], assignment["attempt_id"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn native_macos_agents_match_the_registered_platform(pool: PgPool) {
    let http_app = app(&pool, Perforce::new("p4"));
    let admin = login(&pool, true).await;
    let (_project, _, _) = setup(&http_app, &admin, "127.0.0.1:1666").await;
    let created = ok(
        &http_app,
        &admin,
        "POST",
        "/api/agents",
        json!({
            "name":"mac-mini", "pool":"default",
            "os":"macos", "arch":"aarch64"
        }),
    )
    .await;
    let token = created["token"].as_str().unwrap();
    let correct =
        json!({"request_id":Uuid::new_v4(),"os":"macos","arch":"aarch64","protocol_version":2});
    let claimed = agent_ok(&http_app, token, "/agent/claim", correct).await;
    assert!(claimed["assignment"].is_null());
    let wrong =
        json!({"request_id":Uuid::new_v4(),"os":"linux","arch":"aarch64","protocol_version":2});
    assert_eq!(
        agent_request(&http_app, token, "/agent/claim", wrong)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn project_configuration_import_is_atomic_editable_and_not_executable(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let admin = login(&pool, true).await;
    let project = ok(
        &app,
        &admin,
        "POST",
        "/api/projects",
        json!({"name":"root"}),
    )
    .await;
    let path = format!(
        "/api/projects/{}/configuration",
        project["id"].as_str().unwrap()
    );
    let source = include_str!("../../../examples/root.p4forge.toml");
    let input = json!({"source_name":"root.p4forge.toml","toml":source});
    let imported = ok(&app, &admin, "POST", &path, input.clone()).await;
    assert_eq!(imported["pipeline_ids"].as_array().unwrap().len(), 7);
    assert_eq!(
        ok(&app, &admin, "POST", &path, input).await["already_imported"],
        true
    );
    let outsider = login(&pool, false).await;
    assert_eq!(
        request(&app, Some(&outsider), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let pipeline = imported["pipeline_ids"][0].as_str().unwrap();
    let pipeline_path = format!("/api/pipelines/{pipeline}");
    let detail = ok(&app, &admin, "GET", &pipeline_path, Value::Null).await;
    let comparison = ok(
        &app,
        &admin,
        "GET",
        &format!("{pipeline_path}/review"),
        Value::Null,
    )
    .await;
    assert_eq!(comparison["can_publish"], false);
    assert!(
        comparison["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "configuration_mode")
    );
    let draft = &detail["configuration_draft"];
    assert_eq!(draft["execution_ready"], false);
    let changed = draft["toml"]
        .as_str()
        .unwrap()
        .replace("timeout_seconds = 600", "timeout_seconds = 601");
    let save = json!({"generation":draft["generation"],"toml":changed});
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{pipeline_path}/configuration"),
        save.clone(),
    )
    .await;
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "PUT",
            &format!("{pipeline_path}/configuration"),
            save
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("{pipeline_path}/publish"),
            json!({"generation":0,"base_version":0})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let exported = ok(&app, &admin, "GET", &path, Value::Null).await;
    let config = p4forge_domain::project_config::ProjectConfiguration::parse(
        exported["toml"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(config.pipelines.len(), 7);
    assert_eq!(
        config.pipelines.iter().map(|p| p.jobs.len()).sum::<usize>(),
        12
    );
    let (_, foreign_root, _) = setup(&app, &admin, "127.0.0.1:16660").await;
    let bind_path = format!("{path}/source");
    let mut binding = json!({"source_root_id":foreign_root,"generations":exported["generations"]});
    assert_eq!(
        request(&app, Some(&admin), "PUT", &bind_path, binding.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let connection: Uuid = sqlx::query_scalar("SELECT id FROM perforce_connections LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let root=ok(&app,&admin,"POST",&format!("/api/projects/{}/roots",project["id"].as_str().unwrap()),json!({"name":"root-source","connection_id":connection,"definition":{"kind":"stream","stream":"//streams/root"}})).await;
    binding["source_root_id"] = root["id"].clone();
    ok(&app, &admin, "PUT", &bind_path, binding.clone()).await;
    assert_eq!(
        request(&app, Some(&admin), "PUT", &bind_path, binding)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let bound = ok(&app, &admin, "GET", &pipeline_path, Value::Null).await;
    assert_eq!(
        bound["configuration_draft"]["definition"]["source_root_id"],
        root["id"]
    );
    // A different import with conflicting names must roll back all rows.
    let conflict =
        json!({"source_name":"conflict.toml","toml":format!("# another source\n{source}")});
    assert_eq!(
        request(&app, Some(&admin), "POST", &path, conflict).await.0,
        StatusCode::CONFLICT
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM configuration_imports")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let version: i64 = sqlx::query_scalar("SELECT max(current_version) FROM pipelines")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 0);
}

#[cfg(target_os = "linux")]
#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires the dedicated Docker test image and development Perforce"]
async fn docker_agent_executes_pinned_source_and_cleans_all_exit_and_restart_paths(pool: PgPool) {
    use std::{os::unix::fs::PermissionsExt, process::Stdio, time::Duration};
    let mut p4 = Perforce::new("/usr/local/bin/p4");
    let tickets = std::env::var("P4FORGE_TEST_P4_TICKETS_FILE").unwrap();
    p4.tickets_file = Some(tickets.clone().into());
    let port = std::env::var("P4FORGE_TEST_P4PORT").unwrap();
    let http_app = app(&pool, p4.clone());
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&http_app, &admin, &port).await;
    let roots = ok(
        &http_app,
        &admin,
        "GET",
        &format!("/api/projects/{project}/roots"),
        Value::Null,
    )
    .await;
    let classic=ok(&http_app,&admin,"POST",&format!("/api/projects/{project}/roots"),json!({"name":"classic","connection_id":roots[0]["connection_id"],"definition":{"kind":"classic","view":["//streams/main/..."]}})).await;
    let created = ok(
        &http_app,
        &admin,
        "POST",
        "/api/agents",
        json!({"name":"docker-agent","pool":"default","os":"linux","arch":std::env::consts::ARCH}),
    )
    .await;
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let token = temp.path().join("token");
    std::fs::write(&token, created["token"].as_str().unwrap()).unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = http_app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, serving).await.unwrap();
    });
    let agent_log = temp.path().join("agent.log");
    let launch = || {
        let output = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&agent_log)
            .unwrap();
        tokio::process::Command::new(std::env::var("P4FORGE_TEST_AGENT_BIN").unwrap())
            .args([
                "--server",
                &format!("http://{address}/"),
                "--token-file",
                token.to_str().unwrap(),
                "--tickets-file",
                &tickets,
                "--work-dir",
                temp.path().join("work").to_str().unwrap(),
                "--state-dir",
                temp.path().join("state").to_str().unwrap(),
                "--allow-insecure-internal-http",
            ])
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    let mut agent = launch();
    for scenario in [
        "success",
        "failure",
        "timeout",
        "cancel",
        "restart",
        "classic",
        "shared-success",
        "shared-failure",
        "shared-timeout",
        "shared-cancel",
        "shared-restart",
    ] {
        let shared = scenario.starts_with("shared-");
        let scenario = scenario.strip_prefix("shared-").unwrap_or(scenario);
        let mut definition =
            p4forge_domain::PipelineDefinition::parse(&toml(&root, "cat hello.txt")).unwrap();
        for job in &mut definition.jobs {
            job.runner.arch = std::env::consts::ARCH.into();
            job.timeout_seconds = 60;
            if scenario == "classic" {
                job.working_directory = "root0".into();
            }
        }
        if scenario == "classic" {
            definition.source_root_id = Uuid::parse_str(classic["id"].as_str().unwrap()).unwrap();
        }
        definition.jobs[0].script = vec![
            "test \"$(id -u)\" = 10003".into(),
            "test \"$(cat hello.txt)\" = 'first revision'".into(),
            "test ! -e ci.toml".into(),
            "test -z \"${P4TICKETS+x}${P4FORGE_AGENT_TOKEN_FILE+x}${GOOGLE_CLIENT_SECRET+x}\""
                .into(),
            format!("test ! -r '{}'", token.display()),
            format!("echo {scenario}-started"),
        ];
        match scenario {
            "failure" => definition.jobs[0]
                .script
                .push("echo deliberate-failure >&2; exit 7".into()),
            "timeout" => {
                definition.jobs[0].timeout_seconds = 2;
                definition.jobs[0].script.push("sleep 30".into());
            }
            "cancel" | "restart" => definition.jobs[0].script.push("sleep 30".into()),
            _ => definition.jobs[0].script.push("cat hello.txt; setsid sleep 30 >/dev/null 2>&1 & detached=$!; kill -0 \"$detached\"".into()),
        }
        definition.jobs[1].script =
            vec!["test \"$(cat hello.txt)\" = 'first revision'; echo downstream-ok".into()];
        if shared {
            definition.workspace = p4forge_domain::Workspace::Pipeline;
            definition.stages = vec!["generate".into(), "consume".into()];
            definition.max_parallel_jobs = 1;
            definition.jobs[0].stage = "generate".into();
            definition.jobs[1].stage = "consume".into();
            // No explicit needs: the stage barrier must supply the edge.
            definition.jobs[1].needs.clear();
            definition.jobs[0].script.insert(
                0,
                "echo persistent-output > generated.txt; echo home-output > $HOME/shared.txt"
                    .into(),
            );
            definition.jobs[1].script.push("test $(cat generated.txt) = persistent-output; test $(cat $HOME/shared.txt) = home-output; echo workspace-reused".into());
        }
        publish_definition(&http_app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
        let run = ok(
            &http_app,
            &admin,
            "POST",
            &format!("/api/pipelines/{pipeline}/runs"),
            json!({"changelist":2,"request_id":Uuid::new_v4()}),
        )
        .await;
        let id = run["id"].as_str().unwrap();
        let run_id = Uuid::parse_str(id).unwrap();
        if scenario == "cancel" || scenario == "restart" {
            tokio::time::timeout(Duration::from_secs(20),async {
                loop {
                    let started:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM attempt_logs l JOIN job_attempts a ON a.id=l.attempt_id JOIN job_runs j ON j.id=a.job_id WHERE j.run_id=$1 AND l.content LIKE $2)").bind(run_id).bind(format!("%{scenario}-started%")).fetch_one(&pool).await.unwrap();
                    if started { break; }tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }).await.unwrap_or_else(|_|panic!("cancel job did not start: {}",std::fs::read_to_string(&agent_log).unwrap()));
            if scenario == "restart" {
                agent.kill().await.unwrap();
                agent = launch();
                tokio::time::timeout(Duration::from_secs(10), async {
                    while temp.path().join("state/inflight.json").exists() {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .expect("agent restart did not reconcile its previous client and workspace");
                sqlx::query("UPDATE job_attempts SET lease_expires_at=now()-interval '1 second' WHERE job_id IN (SELECT id FROM job_runs WHERE run_id=$1)").bind(run_id).execute(&pool).await.unwrap();
                p4forge_server::agents::reap(&pool)
                    .await
                    .map_err(|error| error.1)
                    .unwrap();
            } else {
                ok(
                    &http_app,
                    &admin,
                    "POST",
                    &format!("/api/runs/{id}/cancel"),
                    Value::Null,
                )
                .await;
            }
        }
        let status = tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id=$1")
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if !["queued", "running"].contains(&status.as_str()) {
                    break status;
                }
                if let Some(exit) = agent.try_wait().unwrap() {
                    panic!(
                        "agent exited {exit}: {}",
                        std::fs::read_to_string(&agent_log).unwrap()
                    );
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{scenario} timed out: {}",
                std::fs::read_to_string(&agent_log).unwrap()
            )
        });
        let jobs = ok(
            &http_app,
            &admin,
            "GET",
            &format!("/api/runs/{id}/jobs"),
            Value::Null,
        )
        .await;
        assert_eq!(
            status,
            match scenario {
                "success" | "classic" => "succeeded",
                "cancel" => "canceled",
                _ => "failed",
            },
            "{jobs}; {}",
            std::fs::read_to_string(&agent_log).unwrap()
        );
        if scenario == "failure" {
            assert_eq!(jobs[0]["exit_code"], 7);
            assert_eq!(jobs[1]["status"], "skipped");
        }
        if scenario == "timeout" {
            assert_eq!(jobs[0]["reason"], "job timeout");
        }
        if shared {
            assert_eq!(jobs[0]["attempt_id"], jobs[1]["attempt_id"]);
            assert_eq!(jobs[1]["needs"], json!([definition.jobs[0].name]));
            assert_eq!(jobs[0]["shared_workspace"], true);
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM job_attempts a JOIN job_runs j ON j.id=a.job_id WHERE j.run_id=$1")
                .bind(run_id).fetch_one(&pool).await.unwrap();
            assert_eq!(count, 1, "pipeline must reserve one agent and one checkout");
        }
        for job in jobs.as_array().unwrap() {
            if let Some(attempt) = job["attempt_id"].as_str() {
                let client = Perforce::client_name(Uuid::parse_str(attempt).unwrap());
                let found = p4
                    .tagged(
                        &p4forge_perforce::Connection {
                            port: port.clone(),
                            user: "p4forge".into(),
                            charset: None,
                        },
                        &["clients", "-e", &client],
                    )
                    .await
                    .unwrap();
                assert!(
                    !found.contains_key("client"),
                    "temporary Perforce client was not removed"
                );
            }
        }
        assert_eq!(
            std::fs::read_dir(temp.path().join("work")).unwrap().count(),
            0
        );
    }
    terminate_agent(agent.id().unwrap());
    assert!(
        tokio::time::timeout(Duration::from_secs(10), agent.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    server.abort();
}

#[cfg(target_os = "linux")]
fn terminate_agent(pid: u32) {
    // Use the standard kill executable so the server crate needs no libc dependency.
    let status = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}
fn finish_input(assignment: &Value, status: &str, last: i64) -> Value {
    json!({"fence":assignment["fence"],"status":status,"exit_code":if status=="succeeded"{0}else{1},"reason":"test","last_log_sequence":last})
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_workspace_fences_stages_and_completion_are_enforced(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let admin = login(&pool, true).await;
    let (_project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo generated")).unwrap();
    definition.workspace = p4forge_domain::Workspace::Pipeline;
    definition.stages = vec!["generate".into(), "publish".into()];
    definition.max_parallel_jobs = 1;
    definition.jobs[0].stage = "generate".into();
    definition.jobs[1].stage = "publish".into();
    definition.jobs[1].needs.clear();
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    let created = new_agent(&app, &admin, "shared-agent").await;
    let token = created["token"].as_str().unwrap();
    for expire in [false, true] {
        let run = fixture_run(&pool, &admin, &pipeline).await;
        assert!(
            agent_ok(&app, token, "/agent/claim", claim_input()).await["assignment"].is_null(),
            "old agents must not claim shared workspaces"
        );
        let mut claim = claim_input();
        claim["protocol_version"] = json!(2);
        let a = agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].clone();
        let retry = agent_ok(&app, token, "/agent/claim", claim).await["assignment"].clone();
        assert_eq!(retry["attempt_id"], a["attempt_id"]);
        assert_eq!(retry["fence"], a["fence"]);
        assert_eq!(retry["workspace_jobs"], a["workspace_jobs"]);
        let steps = a["workspace_jobs"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        let path = format!("/agent/attempts/{}/step", a["attempt_id"].as_str().unwrap());
        let event = |index: usize, status: &str| json!({"fence":a["fence"],"job_id":steps[index]["id"],"status":status,"exit_code":if status=="succeeded"{Some(0)}else{None},"reason":""});
        assert_eq!(
            agent_request(&app, token, &path, event(1, "running"))
                .await
                .0,
            StatusCode::CONFLICT
        );
        let mut wrong = event(0, "running");
        wrong["fence"] = json!(0);
        assert_eq!(
            agent_request(&app, token, &path, wrong).await.0,
            StatusCode::NOT_FOUND
        );
        agent_ok(&app, token, &path, event(0, "running")).await;
        agent_ok(&app, token, &path, event(0, "running")).await;
        let complete = format!(
            "/agent/attempts/{}/complete",
            a["attempt_id"].as_str().unwrap()
        );
        assert_eq!(
            agent_request(&app, token, &complete, finish_input(&a, "succeeded", -1))
                .await
                .0,
            StatusCode::CONFLICT
        );
        agent_ok(&app, token, &path, event(0, "succeeded")).await;
        if expire {
            sqlx::query(
                "UPDATE job_attempts SET lease_expires_at=now()-interval '1 second' WHERE id=$1",
            )
            .bind(Uuid::parse_str(a["attempt_id"].as_str().unwrap()).unwrap())
            .execute(&pool)
            .await
            .unwrap();
            p4forge_server::agents::reap(&pool)
                .await
                .map_err(|e| e.1)
                .unwrap();
            assert_eq!(
                agent_request(&app, token, &path, event(1, "running"))
                    .await
                    .0,
                StatusCode::CONFLICT
            );
        } else {
            agent_ok(&app, token, &path, event(1, "running")).await;
            agent_ok(&app, token, &path, event(1, "succeeded")).await;
            agent_ok(&app, token, &complete, finish_input(&a, "succeeded", -1)).await;
        }
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id=$1")
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, if expire { "failed" } else { "succeeded" });
        let jobs = ok(
            &app,
            &admin,
            "GET",
            &format!("/api/runs/{run}/jobs"),
            Value::Null,
        )
        .await;
        assert_eq!(jobs[0]["status"], "succeeded");
        assert_eq!(
            jobs[1]["status"],
            if expire { "skipped" } else { "succeeded" }
        );
        assert_eq!(jobs[0]["attempt_id"], jobs[1]["attempt_id"]);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn agent_claims_enforce_requirements_capacity_fences_and_idempotent_logs(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let admin = login(&pool, true).await;
    let (_project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    publish_definition(
        &app,
        &admin,
        &pipeline,
        &(toml(&root, "echo built").replace("max_parallel_jobs = 2", "max_parallel_jobs = 1")+"\n[[jobs]]\nname = \"independent\"\nshell = \"sh\"\nrunner = { os = \"linux\" }\nscript = [\"echo independent\"]\n"),
    )
    .await;
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let a = new_agent(&app, &admin, "agent-a").await;
    let b = new_agent(&app, &admin, "agent-b").await;
    let token = a["token"].as_str().unwrap();
    let token_b = b["token"].as_str().unwrap();
    let foreign = ok(
        &app,
        &admin,
        "POST",
        "/api/agents",
        json!({"name":"other-pool-agent","pool":"other","os":"linux","arch":"x86_64"}),
    )
    .await;
    assert!(
        agent_ok(
            &app,
            foreign["token"].as_str().unwrap(),
            "/agent/claim",
            claim_input()
        )
        .await["assignment"]
            .is_null()
    );
    let claim = claim_input();
    let (first, retry) = tokio::join!(
        agent_ok(&app, token, "/agent/claim", claim.clone()),
        agent_ok(&app, token, "/agent/claim", claim)
    );
    assert_eq!(
        first["assignment"]["attempt_id"],
        retry["assignment"]["attempt_id"]
    );
    let assignment = &first["assignment"];
    let id = assignment["attempt_id"].as_str().unwrap();
    let fence = assignment["fence"].as_i64().unwrap();
    assert_eq!(assignment["job"]["name"], "build");
    assert!(agent_ok(&app, token_b, "/agent/claim", claim_input()).await["assignment"].is_null());
    assert_eq!(
        agent_request(
            &app,
            token_b,
            &format!("/agent/attempts/{id}/heartbeat"),
            json!({"fence":fence})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("/agent/attempts/{id}/heartbeat"),
            json!({"fence":fence+1})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let log_path = format!("/agent/attempts/{id}/logs");
    let chunk = json!({"fence":fence,"sequence":0,"stream":"stdout","content":"hello\n"});
    agent_ok(&app, token, &log_path, chunk.clone()).await;
    agent_ok(&app, token, &log_path, chunk).await;
    assert_eq!(
        agent_request(
            &app,
            token,
            &log_path,
            json!({"fence":fence,"sequence":0,"stream":"stdout","content":"changed"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        agent_request(
            &app,
            token,
            &log_path,
            json!({"fence":fence,"sequence":2,"stream":"stdout","content":"gap"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let completion = format!("/agent/attempts/{id}/complete");
    assert_eq!(
        agent_request(
            &app,
            token,
            &completion,
            finish_input(assignment, "succeeded", -1)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let done = finish_input(assignment, "succeeded", 0);
    agent_ok(&app, token, &completion, done.clone()).await;
    agent_ok(&app, token, &completion, done).await;
    assert_eq!(
        agent_request(
            &app,
            token,
            &completion,
            finish_input(assignment, "failed", 0)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let second = agent_ok(&app, token_b, "/agent/claim", claim_input()).await["assignment"].clone();
    assert_eq!(second["job"]["name"], "test");
    agent_ok(
        &app,
        token_b,
        &format!(
            "/agent/attempts/{}/complete",
            second["attempt_id"].as_str().unwrap()
        ),
        finish_input(&second, "succeeded", -1),
    )
    .await;
    let third = agent_ok(&app, token, "/agent/claim", claim_input()).await["assignment"].clone();
    assert_eq!(third["job"]["name"], "independent");
    agent_ok(
        &app,
        token,
        &format!(
            "/agent/attempts/{}/complete",
            third["attempt_id"].as_str().unwrap()
        ),
        finish_input(&third, "succeeded", -1),
    )
    .await;
    assert_eq!(
        ok(
            &app,
            &admin,
            "GET",
            &format!("/api/runs/{run}"),
            Value::Null
        )
        .await["status"],
        "succeeded"
    );
    let concurrent = fixture_run(&pool, &admin, &pipeline).await;
    let (left, right) = tokio::join!(
        agent_ok(&app, token, "/agent/claim", claim_input()),
        agent_ok(&app, token_b, "/agent/claim", claim_input())
    );
    assert_eq!(
        [&left, &right]
            .iter()
            .filter(|value| !value["assignment"].is_null())
            .count(),
        1
    );
    let executing: i64 =
        sqlx::query_scalar("SELECT count(*) FROM job_runs WHERE run_id=$1 AND status='running'")
            .bind(concurrent)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(executing, 1);
    let outsider = login(&pool, false).await;
    assert_eq!(
        request(
            &app,
            Some(&outsider),
            "GET",
            &format!("/api/attempts/{id}/logs"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/agents/{}", a["agent"]["id"].as_str().unwrap()),
        json!({"active":false,"draining":true}),
    )
    .await;
    assert_eq!(
        agent_request(&app, token, "/agent/claim", claim_input())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn running_cancellation_and_expired_leases_never_release_stale_work(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let admin = login(&pool, true).await;
    let (_project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo built")).await;
    let a = new_agent(&app, &admin, "agent-a").await;
    let token = a["token"].as_str().unwrap();
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let first = agent_ok(&app, token, "/agent/claim", claim_input()).await["assignment"].clone();
    let id = first["attempt_id"].as_str().unwrap();
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{run}/cancel"),
        Value::Null,
    )
    .await;
    let running = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{run}"),
        Value::Null,
    )
    .await;
    assert_eq!(running["status"], "running");
    assert_eq!(running["cancel_requested"], true);
    assert_eq!(
        agent_ok(
            &app,
            token,
            &format!("/agent/attempts/{id}/heartbeat"),
            json!({"fence":first["fence"]})
        )
        .await["cancel_requested"],
        true
    );
    agent_ok(
        &app,
        token,
        &format!("/agent/attempts/{id}/complete"),
        finish_input(&first, "succeeded", -1),
    )
    .await;
    assert_eq!(
        ok(
            &app,
            &admin,
            "GET",
            &format!("/api/runs/{run}"),
            Value::Null
        )
        .await["status"],
        "canceled"
    );
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let first = agent_ok(&app, token, "/agent/claim", claim_input()).await["assignment"].clone();
    let id = first["attempt_id"].as_str().unwrap();
    sqlx::query("UPDATE job_attempts SET lease_expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("/agent/attempts/{id}/heartbeat"),
            json!({"fence":first["fence"]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // The rejected heartbeat drops its transaction; SQLx sends rollback
    // asynchronously. Wait for that Run lock before the SKIP LOCKED reaper tick.
    let mut released = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM runs WHERE id=$1 FOR UPDATE")
        .bind(run)
        .execute(&mut *released)
        .await
        .unwrap();
    released.commit().await.unwrap();
    p4forge_server::agents::reap(&pool)
        .await
        .map_err(|error| error.1)
        .unwrap();
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("/agent/attempts/{id}/complete"),
            finish_input(&first, "succeeded", -1)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(agent_ok(&app, token, "/agent/claim", claim_input()).await["assignment"].is_null());
    let jobs = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{run}/jobs"),
        Value::Null,
    )
    .await;
    assert_eq!(jobs[0]["status"], "lost");
    assert_eq!(jobs[1]["status"], "skipped");
    assert_eq!(
        ok(
            &app,
            &admin,
            "GET",
            &format!("/api/runs/{run}"),
            Value::Null
        )
        .await["status"],
        "failed"
    );
}

struct Login {
    id: Uuid,
    cookies: String,
    csrf: String,
}
async fn login(pool: &PgPool, admin: bool) -> Login {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users(id,google_sub,email,name,is_admin) VALUES($1,$2,$3,'Test user',$4)",
    )
    .bind(id)
    .bind(id.to_string())
    .bind(format!("{id}@zenogrid.co.kr"))
    .bind(admin)
    .execute(pool)
    .await
    .unwrap();
    let token = random_token();
    let csrf = random_token();
    sqlx::query("INSERT INTO auth_sessions(token_hash,user_id,csrf_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')")
        .bind(token_hash(&token)).bind(id).bind(token_hash(&csrf)).execute(pool).await.unwrap();
    Login {
        id,
        cookies: format!("p4forge_session={token}; p4forge_csrf={csrf}"),
        csrf,
    }
}
fn app(pool: &PgPool, perforce: Perforce) -> Router {
    app_at(
        pool,
        perforce,
        std::env::temp_dir().join(format!("p4forge-artifacts-{}", Uuid::new_v4())),
    )
}
fn app_at(pool: &PgPool, perforce: Perforce, artifact_dir: std::path::PathBuf) -> Router {
    p4forge_server::router(state_at(pool, perforce, artifact_dir))
}
fn state_at(pool: &PgPool, perforce: Perforce, artifact_dir: std::path::PathBuf) -> AppState {
    let auth = AuthService::new(
        pool.clone(),
        AuthConfig {
            client_id: "test-client".into(),
            client_secret: "test-only".into(),
            public_url: "https://p4forge.zenogrid.co.kr:8444/".parse().unwrap(),
            bootstrap_admin: "".into(),
        },
    )
    .unwrap();
    AppState {
        credentials: None,
        artifact_dir,
        pool: pool.clone(),
        auth,
        perforce,
    }
}
async fn request(
    app: &Router,
    user: Option<&Login>,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(user) = user {
        builder = builder
            .header("cookie", &user.cookies)
            .header("x-csrf-token", &user.csrf);
    }
    builder = builder.header("content-type", "application/json");
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| json!({"body":String::from_utf8_lossy(&body)})),
    )
}
async fn ok(app: &Router, user: &Login, method: &str, path: &str, body: Value) -> Value {
    let (status, value) = request(app, Some(user), method, path, body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
fn toml(root: &str, script: &str) -> String {
    format!(
        "schema_version = 1\nsource_root_id = \"{root}\"\nmax_parallel_jobs = 2\n[[jobs]]\nname = \"build\"\nshell = \"sh\"\nrunner = {{ os = \"linux\" }}\nscript = [\"{script}\"]\n[[jobs]]\nname = \"test\"\nneeds = [\"build\"]\nshell = \"sh\"\nrunner = {{ os = \"linux\" }}\nscript = [\"test -f hello.txt\"]\n"
    )
}
async fn setup(app: &Router, admin: &Login, port: &str) -> (String, String, String) {
    let project = ok(app, admin, "POST", "/api/projects", json!({"name":"demo"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let connection = ok(
        app,
        admin,
        "POST",
        "/api/connections",
        json!({"name":"local","definition":{"port":port,"user":"p4forge"}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let root = ok(app, admin, "POST", &format!("/api/projects/{project}/roots"), json!({"name":"main","connection_id":connection,"definition":{"kind":"stream","stream":"//streams/main"}})).await["id"].as_str().unwrap().to_owned();
    let pipeline = ok(
        app,
        admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"main"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (project, root, pipeline)
}

#[sqlx::test(migrations = "../../migrations")]
async fn sessions_enforce_csrf_expiry_revocation_and_logout(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let user = login(&pool, true).await;
    assert_eq!(
        request(&app, None, "GET", "/api/me", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, Some(&user), "GET", "/api/me", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let wrong = Login {
        id: user.id,
        cookies: user.cookies.clone(),
        csrf: "wrong".into(),
    };
    assert_eq!(
        request(
            &app,
            Some(&wrong),
            "POST",
            "/api/projects",
            json!({"name":"forged"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE users SET active=false WHERE id=$1")
        .bind(user.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, Some(&user), "GET", "/api/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE users SET active=true WHERE id=$1")
        .bind(user.id)
        .execute(&pool)
        .await
        .unwrap();
    let expired = login(&pool, false).await;
    sqlx::query("UPDATE auth_sessions SET expires_at=now()-interval '1 second' WHERE user_id=$1")
        .bind(expired.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, Some(&expired), "GET", "/api/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    ok(&app, &user, "POST", "/auth/logout", Value::Null).await;
    assert_eq!(
        request(&app, Some(&user), "GET", "/api/me", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn oauth_state_is_browser_bound_one_use_and_pkce_protected(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/auth/google/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let destination: url::Url = response.headers()["location"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let params: std::collections::HashMap<_, _> = destination.query_pairs().into_owned().collect();
    assert_eq!(
        params["redirect_uri"],
        "https://p4forge.zenogrid.co.kr:8444/auth/google/callback"
    );
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["hd"], "zenogrid.co.kr");
    let state = &params["state"];
    let (nonce, verifier): (String, String) =
        sqlx::query_as("SELECT nonce,verifier FROM oauth_states WHERE state_hash=$1")
            .bind(token_hash(state))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(nonce, params["nonce"]);
    use base64::Engine;
    use sha2::Digest;
    assert_eq!(
        params["code_challenge"],
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()))
    );
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(
        cookie.contains("Secure") && cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax")
    );
    let path = format!("/auth/google/callback?state={state}&error=access_denied");
    assert_eq!(
        request(&app, None, "GET", &path, Value::Null).await.0,
        StatusCode::BAD_REQUEST
    );
    let fake = Login {
        id: Uuid::nil(),
        cookies: format!("p4forge_oauth_state={state}"),
        csrf: String::new(),
    };
    assert_eq!(
        request(&app, Some(&fake), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, Some(&fake), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn project_permissions_hide_inaccessible_data_and_separate_roles(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, _, pipeline) = setup(&app, &admin, "localhost:1666").await;
    assert_eq!(
        request(
            &app,
            Some(&outsider),
            "GET",
            &format!("/api/pipelines/{pipeline}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/projects/{project}/members"),
        json!({"user_id":viewer.id,"role":"viewer"}),
    )
    .await;
    let projects = ok(&app, &viewer, "GET", "/api/projects", Value::Null).await;
    assert_eq!(projects[0]["role"], "viewer");
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            &format!("/api/pipelines/{pipeline}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "POST",
            &format!("/api/projects/{project}/pipelines"),
            json!({"name":"forbidden"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, Some(&viewer), "GET", "/api/connections", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, Some(&viewer), "GET", "/api/users", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "POST",
            "/api/projects",
            json!({"name":"forbidden"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn revisions_publish_atomically_reject_conflicts_and_restore_as_new_versions(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let admin = login(&pool, true).await;
    let (_, root, pipeline) = setup(&app, &admin, "localhost:1666").await;
    let path = format!("/api/pipelines/{pipeline}");
    let definition = toml(&root, "cat hello.txt");
    let draft = ok(
        &app,
        &admin,
        "PUT",
        &format!("{path}/draft"),
        json!({"toml":definition,"generation":0,"base_version":0}),
    )
    .await;
    assert_eq!(draft["generation"], 1);
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "PUT",
            &format!("{path}/draft"),
            json!({"toml":definition,"generation":0,"base_version":0})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let publish = format!("{path}/publish");
    let (a, b) = tokio::join!(
        request(
            &app,
            Some(&admin),
            "POST",
            &publish,
            json!({"generation":1,"base_version":0})
        ),
        request(
            &app,
            Some(&admin),
            "POST",
            &publish,
            json!({"generation":1,"base_version":0})
        )
    );
    assert!(matches!(
        (a.0, b.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
    ));
    let foreign_project = ok(
        &app,
        &admin,
        "POST",
        "/api/projects",
        json!({"name":"other"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let foreign_pipeline = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{foreign_project}/pipelines"),
        json!({"name":"other"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/pipelines/{foreign_pipeline}/validate"),
            json!({"toml":definition})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(
        sqlx::query("UPDATE pipeline_revisions SET definition='{}' WHERE pipeline_id=$1")
            .bind(Uuid::parse_str(&pipeline).unwrap())
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM pipeline_revisions WHERE pipeline_id=$1")
            .bind(Uuid::parse_str(&pipeline).unwrap())
            .execute(&pool)
            .await
            .is_err()
    );
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{path}/draft"),
        json!({"toml":toml(&root,"echo changed"),"generation":2,"base_version":1}),
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &publish,
        json!({"generation":3,"base_version":1}),
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{path}/restore"),
        json!({"version":1,"generation":4,"base_version":2}),
    )
    .await;
    let detail = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(detail["pipeline"]["current_version"], 2);
    assert!(
        detail["draft_toml"]
            .as_str()
            .unwrap()
            .contains("cat hello.txt")
    );
    let revision = ok(
        &app,
        &admin,
        "POST",
        &publish,
        json!({"generation":5,"base_version":2}),
    )
    .await;
    assert_eq!(revision["version"], 3);
    let revisions = ok(
        &app,
        &admin,
        "GET",
        &format!("{path}/revisions"),
        Value::Null,
    )
    .await;
    assert_eq!(revisions.as_array().unwrap().len(), 3);
    assert_eq!(
        revisions[0]["definition_hash"],
        revisions[2]["definition_hash"]
    );
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires scripts/dev-perforce.sh and P4FORGE_TEST_P4_TICKETS_FILE"]
async fn real_perforce_run_snapshots_are_pinned_idempotent_and_cancelable(pool: PgPool) {
    let mut perforce = Perforce::new("p4");
    perforce.tickets_file = Some(
        std::env::var("P4FORGE_TEST_P4_TICKETS_FILE")
            .expect("test ticket path")
            .into(),
    );
    let port = std::env::var("P4FORGE_TEST_P4PORT").unwrap_or_else(|_| "127.0.0.1:16660".into());
    let app = app(&pool, perforce);
    let admin = login(&pool, true).await;
    let (_, root, pipeline) = setup(&app, &admin, &port).await;
    let path = format!("/api/pipelines/{pipeline}");
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{path}/draft"),
        json!({"toml":toml(&root,"cat hello.txt"),"generation":0,"base_version":0}),
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{path}/publish"),
        json!({"generation":1,"base_version":0}),
    )
    .await;
    let run_path = format!("{path}/runs");
    let input = json!({"changelist":2,"version":1,"request_id":Uuid::new_v4()});
    let (a, b) = tokio::join!(
        request(&app, Some(&admin), "POST", &run_path, input.clone()),
        request(&app, Some(&admin), "POST", &run_path, input.clone())
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(b.0, StatusCode::OK, "{}", b.1);
    assert_eq!(a.1["id"], b.1["id"]);
    assert_eq!(a.1["source_snapshot"]["changelist"], 2);
    assert_eq!(a.1["source_snapshot"]["client_spec"]["StreamAtChange"], "2");
    assert_eq!(a.1["source_snapshot"]["server_id"], "p4forge-dev");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert!(
        sqlx::query("UPDATE runs SET source_snapshot='{}'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE job_runs SET definition='{}'")
            .execute(&pool)
            .await
            .is_err()
    );
    let dependencies: i64 = sqlx::query_scalar("SELECT count(*) FROM job_dependencies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(dependencies, 1);
    let mut changed = input.clone();
    changed["changelist"] = json!(3);
    assert_eq!(
        request(&app, Some(&admin), "POST", &run_path, changed)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &run_path,
            json!({"changelist":999999,"version":1,"request_id":Uuid::new_v4()})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{path}/draft"),
        json!({"toml":toml(&root,"echo newer"),"generation":2,"base_version":1}),
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{path}/publish"),
        json!({"generation":3,"base_version":1}),
    )
    .await;
    let retried = ok(&app, &admin, "POST", &run_path, input).await;
    assert_eq!(retried["id"], a.1["id"]);
    assert_eq!(retried["version"], 1);
    let cancel = format!("/api/runs/{}/cancel", a.1["id"].as_str().unwrap());
    ok(&app, &admin, "POST", &cancel, Value::Null).await;
    let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM job_runs")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(statuses.iter().all(|status| status == "canceled"));
    assert_eq!(
        request(&app, Some(&admin), "POST", &cancel, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn publications_require_new_agent_completed_steps_and_verified_receipts(pool: PgPool) {
    let app = app(&pool, Perforce::new("/no-p4-for-this-test"));
    let admin = login(&pool, true).await;
    let (_project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let mut d = p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo generated")).unwrap();
    d.workspace = p4forge_domain::Workspace::Pipeline;
    d.max_parallel_jobs = 1;
    d.stages = vec!["build".into()];
    for j in &mut d.jobs {
        j.stage = "build".into();
    }
    d.publish_generated = Some(p4forge_domain::GeneratedPublication {
        user: "publisher".into(),
        paths: vec!["generated".into()],
    });
    publish_definition(&app, &admin, &pipeline, &d.to_toml().unwrap()).await;
    let created = new_agent(&app, &admin, "publisher-agent").await;
    let token = created["token"].as_str().unwrap();
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let mut claim = claim_input();
    claim["protocol_version"] = json!(2);
    assert!(agent_ok(&app, token, "/agent/claim", claim).await["assignment"].is_null());
    let mut claim = claim_input();
    claim["protocol_version"] = json!(3);
    let a = agent_ok(&app, token, "/agent/claim", claim).await["assignment"].clone();
    assert_eq!(a["publish_generated"]["paths"], json!(["generated"]));
    let base = format!("/agent/attempts/{}", a["attempt_id"].as_str().unwrap());
    let receipt =
        |status: &str, cl: i64| json!({"fence":a["fence"],"status":status,"changelist":cl});
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("{base}/publication"),
            receipt("unchanged", 2)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    for job in a["workspace_jobs"].as_array().unwrap() {
        for status in ["running", "succeeded"] {
            agent_ok(&app,token,&format!("{base}/step"),json!({"fence":a["fence"],"job_id":job["id"],"status":status,"exit_code":if status=="succeeded"{Some(0)}else{None},"reason":""})).await;
        }
    }
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("{base}/complete"),
            finish_input(&a, "succeeded", -1)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("{base}/publication"),
            receipt("submitted", 3)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("{base}/publication"),
            receipt("unchanged", 3)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for _ in 0..2 {
        agent_ok(
            &app,
            token,
            &format!("{base}/publication"),
            receipt("unchanged", 2),
        )
        .await;
    }
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("{base}/publication"),
            receipt("prepared", 3)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    agent_ok(
        &app,
        token,
        &format!("{base}/complete"),
        finish_input(&a, "succeeded", -1),
    )
    .await;
    let output = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{run}/outputs"),
        Value::Null,
    )
    .await;
    assert_eq!(output["publication"]["changelist"], 2);
    assert_eq!(output["publication"]["status"], "unchanged");
}

#[sqlx::test(migrations = "../../migrations")]
async fn downstream_versions_are_fixed_scoped_and_cannot_form_diamonds(pool: PgPool) {
    let app = app(&pool, Perforce::new("p4"));
    let admin = login(&pool, true).await;
    let (project, root, parent) = setup(&app, &admin, "127.0.0.1:16660").await;
    let child = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"child"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish_definition(&app, &admin, &child, &toml(&root, "echo child")).await;
    let mut d = p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo parent")).unwrap();
    d.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: child.parse().unwrap(),
        version: 1,
    }];
    publish_definition(&app, &admin, &parent, &d.to_toml().unwrap()).await;
    d.on_success[0].version = 2;
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/pipelines/{parent}/validate"),
            json!({"toml":d.to_toml().unwrap()})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    d.on_success[0].version = 1;
    publish_definition(&app, &admin, &child, &d.to_toml().unwrap()).await;
    let graph = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/pipelines/{child}/connections"),
        Value::Null,
    )
    .await;
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(graph["root"], format!("{child}:v2"));
    assert_eq!(graph["edges"][0]["to"], format!("{child}:v1"));
    // New parent -> child v2 -> child v1 repeats a pipeline identity and is rejected.
    d.on_success[0].version = 2;
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/pipelines/{parent}/validate"),
            json!({"toml":d.to_toml().unwrap()})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[sqlx::test(migrations = "../../migrations")]
#[cfg(unix)]
async fn submitted_receipts_verify_paths_and_preserve_renumbered_changes(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let cli = tmp.path().join("p4");
    let response = tmp.path().join("response");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\ncase \"$*\" in *'describe -s -O 3') cat '{}' ;; *) exit 1 ;; esac\n",
            response.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let app = app(&pool, Perforce::new(cli));
    let admin = login(&pool, true).await;
    let (_project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let mut d = p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo generated")).unwrap();
    d.workspace = p4forge_domain::Workspace::Pipeline;
    d.max_parallel_jobs = 1;
    d.stages = vec!["build".into()];
    for j in &mut d.jobs {
        j.stage = "build".into();
    }
    d.publish_generated = Some(p4forge_domain::GeneratedPublication {
        user: "publisher".into(),
        paths: vec!["generated".into()],
    });
    publish_definition(&app, &admin, &pipeline, &d.to_toml().unwrap()).await;
    let agent = new_agent(&app, &admin, "publisher").await;
    let token = agent["token"].as_str().unwrap();
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let mut claim = claim_input();
    claim["protocol_version"] = json!(3);
    let a = agent_ok(&app, token, "/agent/claim", claim).await["assignment"].clone();
    let attempt = Uuid::parse_str(a["attempt_id"].as_str().unwrap()).unwrap();
    let base = format!("/agent/attempts/{attempt}");
    for job in a["workspace_jobs"].as_array().unwrap() {
        for status in ["running", "succeeded"] {
            agent_ok(&app,token,&format!("{base}/step"),json!({"fence":a["fence"],"job_id":job["id"],"status":status,"exit_code":if status=="succeeded"{Some(0)}else{None},"reason":""})).await;
        }
    }
    let receipt = |status: &str| json!({"fence":a["fence"],"status":status,"changelist":3});
    agent_ok(
        &app,
        token,
        &format!("{base}/publication"),
        receipt("prepared"),
    )
    .await;
    let description = format!(
        "... change 99\n... status submitted\n... user publisher\n... client p4forge_publish_{}\n... desc P4Forge run={run} attempt={attempt} base=2\n",
        attempt.simple()
    );
    std::fs::write(
        &response,
        format!("{description}... depotFile0 //streams/main/private/secret\n"),
    )
    .unwrap();
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("{base}/publication"),
            receipt("submitted")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    std::fs::write(
        &response,
        format!("{description}... depotFile0 //streams/main/generated/data.bin\n"),
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(
            agent_ok(
                &app,
                token,
                &format!("{base}/publication"),
                receipt("submitted")
            )
            .await["changelist"],
            99
        );
    }
    agent_ok(
        &app,
        token,
        &format!("{base}/complete"),
        finish_input(&a, "succeeded", -1),
    )
    .await;
    let pair: (i64, i64) = sqlx::query_as(
        "SELECT reserved_changelist,changelist FROM run_publications WHERE run_id=$1",
    )
    .bind(run)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pair, (3, 99));
}

#[sqlx::test(migrations = "../../migrations")]
#[cfg(unix)]
async fn followups_survive_repeated_ticks_and_keep_the_frozen_version(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let cli = temp.path().join("p4");
    std::fs::write(&cli,r#"#!/bin/sh
for last; do :; done
case "$*" in
  *'login -s') printf '... User p4forge\n' ;;
  *'info -s') printf '... ServerID fixture\n... serverVersion fixture\n' ;;
  *'describe -s '*) printf '... change %s\n... status submitted\n' "$last" ;;
  *'stream -o '*) printf '... Stream //streams/main\n... Parent none\n... Paths0 share ...\n' ;;
  *'client -o -S '*) printf '... Client %s\n... Stream //streams/main\n... View0 //streams/main/... //%s/...\n' "$last" "$last" ;;
  *) exit 1 ;;
esac
"#).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = AppState {
        credentials: None,
        artifact_dir: std::env::temp_dir().join(format!("p4forge-artifacts-{}", Uuid::new_v4())),
        pool: pool.clone(),
        perforce: Perforce::new(cli),
        auth: AuthService::new(
            pool.clone(),
            AuthConfig {
                client_id: "test".into(),
                client_secret: "test".into(),
                public_url: "https://example.invalid/".parse().unwrap(),
                bootstrap_admin: "".into(),
            },
        )
        .unwrap(),
    };
    let app = p4forge_server::router(state.clone());
    let admin = login(&pool, true).await;
    let (project, root, parent) = setup(&app, &admin, "127.0.0.1:16660").await;
    let connection: Uuid =
        sqlx::query_scalar("SELECT connection_id FROM perforce_roots WHERE id=$1")
            .bind(root.parse::<Uuid>().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let view = ok(&app, &admin, "POST", &format!("/api/projects/{project}/roots"),
        json!({"name":"ServerBuildView","connection_id":connection,"definition":{"kind":"view","view":["//streams/main/Server/... Server/..."]}}))
        .await["id"].as_str().unwrap().to_owned();
    let child = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"child"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish_definition(&app, &admin, &child, &toml(&view, "echo first")).await;
    let mut d = p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo parent")).unwrap();
    d.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: child.parse().unwrap(),
        version: 1,
    }];
    publish_definition(&app, &admin, &parent, &d.to_toml().unwrap()).await;
    let other_connection = ok(
        &app,
        &admin,
        "POST",
        "/api/connections",
        json!({"name":"other","definition":{"port":"127.0.0.1:16661","user":"p4forge"}}),
    )
    .await;
    let other_root = ok(&app, &admin, "POST", &format!("/api/projects/{project}/roots"), json!({"name":"other","connection_id":other_connection["id"],"definition":{"kind":"stream","stream":"//streams/main"}})).await;
    let mut unrelated = d.clone();
    unrelated.source_root_id = other_root["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/pipelines/{parent}/validate"),
            json!({"toml":unrelated.to_toml().unwrap()})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let run = fixture_run(&pool, &admin, &parent).await;
    sqlx::query("UPDATE runs SET status='succeeded' WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    publish_definition(&app, &admin, &child, &toml(&view, "echo second")).await;
    for _ in 0..2 {
        p4forge_server::followups::tick(&state)
            .await
            .map_err(|error| error.1)
            .unwrap();
    }
    let result:Vec<(i64,i64)>=sqlx::query_as("SELECT r.version,(r.source_snapshot->>'changelist')::bigint FROM runs r JOIN run_followups f ON f.child_run_id=r.id WHERE f.parent_run_id=$1").bind(run).fetch_all(&pool).await.unwrap();
    assert_eq!(result, vec![(1, 2)]);
    let child_snapshot: Value = sqlx::query_scalar("SELECT r.source_snapshot FROM runs r JOIN run_followups f ON f.child_run_id=r.id WHERE f.parent_run_id=$1").bind(run).fetch_one(&pool).await.unwrap();
    assert_eq!(child_snapshot["root"]["kind"], "view");
    assert_eq!(
        child_snapshot["root"]["view"][0],
        "//streams/main/Server/... Server/..."
    );
    let canceled = fixture_run(&pool, &admin, &parent).await;
    sqlx::query("UPDATE runs SET status='canceled',cancel_requested=true WHERE id=$1")
        .bind(canceled)
        .execute(&pool)
        .await
        .unwrap();
    let revoked = fixture_run(&pool, &admin, &parent).await;
    sqlx::query("UPDATE runs SET status='succeeded' WHERE id=$1")
        .bind(revoked)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET active=false WHERE id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    p4forge_server::followups::tick(&state)
        .await
        .map_err(|error| error.1)
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM run_followups WHERE parent_run_id=$1")
            .bind(canceled)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    let status: String =
        sqlx::query_scalar("SELECT status FROM run_followups WHERE parent_run_id=$1")
            .bind(revoked)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "blocked");
}

#[sqlx::test(migrations = "../../migrations")]
async fn artifacts_are_fenced_verified_private_and_survive_router_restart(pool: PgPool) {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use p4forge_domain::{ArtifactSpec, Workspace, hash_bytes};
    let disk = tempfile::tempdir().unwrap();
    let app = app_at(&pool, Perforce::new("/unused"), disk.path().into());
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/projects/{project}/members"),
        json!({"user_id":viewer.id,"role":"viewer"}),
    )
    .await;
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo output")).unwrap();
    definition.workspace = Workspace::Pipeline;
    definition.stages = vec!["build".into(), "test".into()];
    definition.jobs[0].stage = "build".into();
    definition.jobs[1].stage = "test".into();
    definition.max_parallel_jobs = 1;
    definition.artifacts = vec![
        ArtifactSpec {
            name: "output.bin".into(),
            path: "bin/output.bin".into(),
        },
        ArtifactSpec {
            name: "empty.txt".into(),
            path: "empty.txt".into(),
        },
    ];
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    let created = new_agent(&app, &admin, "uploader").await;
    let other = new_agent(&app, &admin, "other-agent").await;
    let token = created["token"].as_str().unwrap();
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let mut claim = claim_input();
    claim["protocol_version"] = json!(4);
    assert!(agent_ok(&app, token, "/agent/claim", claim).await["assignment"].is_null());
    let mut claim = claim_input();
    claim["protocol_version"] = json!(5);
    let a = agent_ok(&app, token, "/agent/claim", claim).await["assignment"].clone();
    assert_eq!(a["artifacts"].as_array().unwrap().len(), 2);
    let attempt = a["attempt_id"].as_str().unwrap();
    let path = format!("/agent/attempts/{attempt}/artifacts");
    let manifest = json!({"fence":a["fence"],"files":[{"name":"output.bin","path":"bin/output.bin","size":7,"sha256":hash_bytes(b"payload")},{"name":"empty.txt","path":"empty.txt","size":0,"sha256":hash_bytes(b"")}]});
    assert_eq!(
        agent_request(&app, token, &path, manifest.clone()).await.0,
        StatusCode::CONFLICT
    );
    for step in a["workspace_jobs"].as_array().unwrap() {
        for status in ["running", "succeeded"] {
            agent_ok(&app,token,&format!("/agent/attempts/{attempt}/step"),json!({"fence":a["fence"],"job_id":step["id"],"status":status,"exit_code":if status=="succeeded"{Some(0)}else{None},"reason":""})).await;
        }
    }
    let complete = format!("/agent/attempts/{attempt}/complete");
    assert_eq!(
        agent_request(&app, token, &complete, finish_input(&a, "succeeded", -1))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut invalid = manifest.clone();
    invalid["files"][0]["path"] = json!(".env");
    assert_eq!(
        agent_request(&app, token, &path, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    let rows = agent_ok(&app, token, &path, manifest.clone()).await;
    assert_eq!(agent_ok(&app, token, &path, manifest.clone()).await, rows);
    let mut changed = manifest.clone();
    changed["files"][0]["sha256"] = json!(hash_bytes(b"changed"));
    assert_eq!(
        agent_request(&app, token, &path, changed).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        agent_request(
            &app,
            other["token"].as_str().unwrap(),
            &path,
            manifest.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let mut stale = manifest.clone();
    stale["fence"] = json!(0);
    assert_eq!(
        agent_request(&app, token, &path, stale).await.0,
        StatusCode::NOT_FOUND
    );
    let artifact = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "output.bin")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let empty = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "empty.txt")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let chunk_path = format!("{path}/{artifact}");
    let chunk = |offset: u64, data: &[u8]| json!({"fence":a["fence"],"offset":offset,"data":STANDARD.encode(data)});
    agent_ok(&app, token, &chunk_path, chunk(0, b"pay")).await;
    agent_ok(&app, token, &chunk_path, chunk(0, b"pay")).await;
    assert_eq!(
        agent_request(&app, token, &chunk_path, chunk(0, b"bad"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        agent_request(&app, token, &chunk_path, chunk(5, b"ad"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        agent_request(&app, token, &chunk_path, chunk(3, b"evil"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    // Failed final hash left uncommitted bytes on disk; retry truncates at the SQL offset.
    assert_eq!(
        agent_ok(&app, token, &chunk_path, chunk(3, b"load")).await["status"],
        "verified"
    );
    agent_ok(&app, token, &chunk_path, chunk(3, b"load")).await;
    assert_eq!(
        agent_request(&app, token, &complete, finish_input(&a, "succeeded", -1))
            .await
            .0,
        StatusCode::CONFLICT
    );
    agent_ok(&app, token, &format!("{path}/{empty}"), chunk(0, b"")).await;
    let list = format!("/api/runs/{run}/artifacts");
    let download = format!("{list}/{artifact}");
    assert!(
        ok(&app, &admin, "GET", &list, Value::Null)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        request(&app, Some(&admin), "GET", &download, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    agent_ok(&app, token, &complete, finish_input(&a, "succeeded", -1)).await;
    assert_eq!(
        agent_request(&app, token, &chunk_path, chunk(3, b"load"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let restarted = app_at(&pool, Perforce::new("/unused"), disk.path().into());
    assert_eq!(
        ok(&restarted, &viewer, "GET", &list, Value::Null)
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        request(&restarted, None, "GET", &download, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&restarted, Some(&outsider), "GET", &download, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let response = restarted
        .clone()
        .oneshot(
            Request::builder()
                .uri(&download)
                .header("cookie", &viewer.cookies)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"output.bin\""
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        &to_bytes(response.into_body(), 100).await.unwrap()[..],
        b"payload"
    );
    // Existing bytes can never authorize upload after cancellation or lease expiry.
    for canceled in [true, false] {
        let run = fixture_run(&pool, &admin, &pipeline).await;
        let mut claim = claim_input();
        claim["protocol_version"] = json!(5);
        let a = agent_ok(&app, token, "/agent/claim", claim).await["assignment"].clone();
        let id = Uuid::parse_str(a["attempt_id"].as_str().unwrap()).unwrap();
        if canceled {
            ok(
                &app,
                &admin,
                "POST",
                &format!("/api/runs/{run}/cancel"),
                Value::Null,
            )
            .await;
        } else {
            sqlx::query(
                "UPDATE job_attempts SET lease_expires_at=now()-interval '1 second' WHERE id=$1",
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
        let mut input = manifest.clone();
        input["fence"] = a["fence"].clone();
        assert_eq!(
            agent_request(
                &app,
                token,
                &format!("/agent/attempts/{id}/artifacts"),
                input
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        if canceled {
            agent_ok(
                &app,
                token,
                &format!("/agent/attempts/{id}/complete"),
                finish_input(&a, "canceled", -1),
            )
            .await;
        } else {
            p4forge_server::agents::reap(&pool)
                .await
                .map_err(|e| e.1)
                .unwrap();
        }
    }
}

#[cfg(target_os = "linux")]
#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires the dedicated Docker test image and development Perforce"]
async fn real_agent_artifacts_survive_workspace_cleanup(pool: PgPool) {
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let disk = tempfile::tempdir().unwrap();
    let mut p4 = Perforce::new("/usr/local/bin/p4");
    let tickets = std::env::var("P4FORGE_TEST_P4_TICKETS_FILE").unwrap();
    p4.tickets_file = Some(tickets.clone().into());
    let app = app_at(&pool, p4.clone(), disk.path().into());
    let admin = login(&pool, true).await;
    let (project, root, pipeline) =
        setup(&app, &admin, &std::env::var("P4FORGE_TEST_P4PORT").unwrap()).await;
    sqlx::query(
        "UPDATE perforce_connections SET definition=jsonb_set(definition,'{charset}','\"utf8\"')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo artifact")).unwrap();
    definition.workspace = p4forge_domain::Workspace::Pipeline;
    definition.max_parallel_jobs = 1;
    definition.stages = vec!["build".into()];
    definition.jobs.truncate(1);
    definition.jobs[0].stage = "build".into();
    definition.jobs[0].runner.arch = std::env::consts::ARCH.into();
    definition.jobs[0].script = vec![
        "test -f hello.txt".into(),
        "dd if=/dev/zero of=output.bin bs=1024 count=300".into(),
    ];
    definition.artifacts = vec![p4forge_domain::ArtifactSpec {
        name: "output.bin".into(),
        path: "output.bin".into(),
    }];
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    let created=ok(&app,&admin,"POST","/api/agents",json!({"name":"artifact-agent","pool":"default","os":"linux","arch":std::env::consts::ARCH})).await;
    let token = temp.path().join("token");
    std::fs::write(&token, created["token"].as_str().unwrap()).unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, serving).await.unwrap() });
    let run = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/pipelines/{pipeline}/runs"),
        json!({"changelist":2,"request_id":Uuid::new_v4()}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        tokio::process::Command::new(std::env::var("P4FORGE_TEST_AGENT_BIN").unwrap())
            .args([
                "--server",
                &format!("http://{address}/"),
                "--token-file",
                token.to_str().unwrap(),
                "--tickets-file",
                &tickets,
                "--work-dir",
                temp.path().join("work").to_str().unwrap(),
                "--state-dir",
                temp.path().join("state").to_str().unwrap(),
                "--allow-insecure-internal-http",
                "--once",
            ])
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let detail = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{run}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        detail["status"],
        "succeeded",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_dir(temp.path().join("work")).unwrap().count(),
        0
    );
    let child = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"artifact-consumer"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut consuming = definition.clone();
    consuming.artifacts.clear();
    consuming.artifact_inputs = vec!["output.bin".into()];
    consuming.jobs[0].script = vec![
        "test -f \"$P4FORGE_INPUTS/output.bin\"".into(),
        "test \"$(wc -c < \"$P4FORGE_INPUTS/output.bin\")\" -eq 307200".into(),
        "test ! -w \"$P4FORGE_INPUTS/output.bin\"".into(),
        "test ! -w \"$P4FORGE_INPUTS\"".into(),
        "echo consumed-verified-input".into(),
    ];
    publish_definition(&app, &admin, &child, &consuming.to_toml().unwrap()).await;
    for corrupt in [false, true] {
        if corrupt {
            let artifact: Uuid = sqlx::query_scalar("SELECT id FROM artifacts WHERE run_id=$1")
                .bind(run.parse::<Uuid>().unwrap())
                .fetch_one(&pool)
                .await
                .unwrap();
            std::fs::write(
                disk.path().join(format!("{artifact}.blob")),
                vec![1u8; 300 * 1024],
            )
            .unwrap();
        }
        let child_run = ok(
            &app,
            &admin,
            "POST",
            &format!("/api/pipelines/{child}/runs"),
            json!({"changelist":2,"request_id":Uuid::new_v4(),"source_run_id":run}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let result = tokio::time::timeout(
            Duration::from_secs(45),
            tokio::process::Command::new(std::env::var("P4FORGE_TEST_AGENT_BIN").unwrap())
                .args([
                    "--server",
                    &format!("http://{address}/"),
                    "--token-file",
                    token.to_str().unwrap(),
                    "--tickets-file",
                    &tickets,
                    "--work-dir",
                    temp.path().join("work").to_str().unwrap(),
                    "--state-dir",
                    temp.path().join("state").to_str().unwrap(),
                    "--allow-insecure-internal-http",
                    "--once",
                ])
                .env_clear()
                .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        let child_detail = ok(
            &app,
            &admin,
            "GET",
            &format!("/api/runs/{child_run}"),
            Value::Null,
        )
        .await;
        assert_eq!(
            child_detail["status"],
            if corrupt { "failed" } else { "succeeded" },
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            std::fs::read_dir(temp.path().join("work")).unwrap().count(),
            0
        );
    }
    // Restore only the deliberately corrupted test blob for the existing download check.
    let artifact: Uuid = sqlx::query_scalar("SELECT id FROM artifacts WHERE run_id=$1")
        .bind(run.parse::<Uuid>().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    std::fs::write(
        disk.path().join(format!("{artifact}.blob")),
        vec![0u8; 300 * 1024],
    )
    .unwrap();
    server.abort();
    let restarted = app_at(&pool, p4, disk.path().into());
    let rows = ok(
        &restarted,
        &admin,
        "GET",
        &format!("/api/runs/{run}/artifacts"),
        Value::Null,
    )
    .await;
    let expected = vec![0u8; 300 * 1024];
    assert_eq!(rows[0]["sha256"], p4forge_domain::hash_bytes(&expected));
    let response = restarted
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/runs/{run}/artifacts/{}",
                    rows[0]["id"].as_str().unwrap()
                ))
                .header("cookie", &admin.cookies)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(response.into_body(), 400 * 1024).await.unwrap()[..],
        &expected
    );
}

async fn retention_artifact(
    pool: &PgPool,
    user: &Login,
    pipeline: &str,
    agent: Uuid,
    days: i32,
    status: &str,
    disk: &std::path::Path,
) -> (Uuid, Uuid) {
    let run = fixture_run(pool, user, pipeline).await;
    let job: Uuid =
        sqlx::query_scalar("SELECT id FROM job_runs WHERE run_id=$1 ORDER BY position LIMIT 1")
            .bind(run)
            .fetch_one(pool)
            .await
            .unwrap();
    let attempt = Uuid::new_v4();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO job_attempts(id,job_id,agent_id,claim_request_id,status,lease_expires_at,finished_at) VALUES($1,$2,$3,$4,$5,now()+interval '1 hour',CASE WHEN $5='running' THEN NULL ELSE now()-($6::integer*interval '1 day') END)")
        .bind(attempt).bind(job).bind(agent).bind(Uuid::new_v4()).bind(status).bind(days).execute(pool).await.unwrap();
    sqlx::query("UPDATE runs SET status=$2,finished_at=CASE WHEN $2='running' THEN NULL ELSE now()-($3::integer*interval '1 day') END WHERE id=$1").bind(run).bind(status).bind(days).execute(pool).await.unwrap();
    sqlx::query("UPDATE job_runs SET status=$2 WHERE run_id=$1")
        .bind(run)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO artifact_manifests(attempt_id,run_id,definition_hash) VALUES($1,$2,'test')",
    )
    .bind(attempt)
    .bind(run)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO artifacts(id,attempt_id,run_id,name,path,size,sha256,uploaded_size,status,created_at) VALUES($1,$2,$3,'output.bin','output.bin',7,$4,7,$5,now()-($6::integer*interval '1 day'))")
        .bind(id).bind(attempt).bind(run).bind(p4forge_domain::hash_bytes(b"payload")).bind(if status=="failed" || status=="running" {"pending"}else{"verified"}).bind(days).execute(pool).await.unwrap();
    std::fs::write(disk.join(format!("{id}.blob")), b"payload").unwrap();
    (run, id)
}

#[sqlx::test(migrations = "../../migrations")]
async fn artifact_retention_preserves_recent_builds_and_active_uploads_and_is_admin_scoped(
    pool: PgPool,
) {
    let disk = tempfile::tempdir().unwrap();
    let state = state_at(&pool, Perforce::new("/unused"), disk.path().into());
    let app = p4forge_server::router(state.clone());
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/projects/{project}/members"),
        json!({"user_id":viewer.id,"role":"viewer"}),
    )
    .await;
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo test")).await;
    let created = new_agent(&app, &admin, "retention-fixture").await;
    let agent = Uuid::parse_str(created["agent"]["id"].as_str().unwrap()).unwrap();
    let mut kept = Vec::new();
    let mut expired = Vec::new();
    for days in [1, 40, 41, 42, 43, 44, 45] {
        let pair = retention_artifact(
            &pool,
            &admin,
            &pipeline,
            agent,
            days,
            "succeeded",
            disk.path(),
        )
        .await;
        if days >= 44 {
            expired.push(pair);
        } else {
            kept.push(pair);
        }
    }
    let second = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"second"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish_definition(&app, &admin, &second, &toml(&root, "echo test")).await;
    for days in [90, 91] {
        kept.push(
            retention_artifact(
                &pool,
                &admin,
                &second,
                agent,
                days,
                "succeeded",
                disk.path(),
            )
            .await,
        );
    }
    // More recent runs without artifacts must not displace this pipeline's usable builds.
    for _ in 0..6 {
        let run = fixture_run(&pool, &admin, &second).await;
        sqlx::query("UPDATE runs SET status='succeeded',finished_at=now() WHERE id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
    }
    expired
        .push(retention_artifact(&pool, &admin, &pipeline, agent, 2, "failed", disk.path()).await);
    expired.push(
        retention_artifact(&pool, &admin, &pipeline, agent, 2, "canceled", disk.path()).await,
    );
    kept.push(retention_artifact(&pool, &admin, &pipeline, agent, 0, "failed", disk.path()).await);
    kept.push(
        retention_artifact(&pool, &admin, &pipeline, agent, 100, "running", disk.path()).await,
    );
    let path = format!("/api/projects/{project}/artifact-retention");
    assert_eq!(
        ok(&app, &viewer, "GET", &path, Value::Null).await["policy"]["enabled"],
        false
    );
    assert_eq!(
        request(&app, None, "GET", &path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, Some(&outsider), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        p4forge_server::retention::tick(&state)
            .await
            .map_err(|e| e.1)
            .unwrap(),
        0
    );
    let input = json!({"generation":0,"enabled":true,"retention_days":30,"keep_successful_runs":5});
    assert_eq!(
        request(&app, Some(&viewer), "PUT", &path, input.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            Some(&outsider),
            "POST",
            &format!("{path}/preview"),
            input.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let mut invalid = input.clone();
    invalid["keep_successful_runs"] = json!(0);
    assert_eq!(
        request(&app, Some(&admin), "PUT", &path, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    let preview = ok(
        &app,
        &admin,
        "POST",
        &format!("{path}/preview"),
        input.clone(),
    )
    .await;
    assert_eq!(preview["count"], 4);
    assert_eq!(preview["bytes"], 28);
    assert!(expired.iter().all(|(_, id)| {
        preview["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["id"] == id.to_string())
    }));
    assert!(
        expired
            .iter()
            .all(|(_, id)| disk.path().join(format!("{id}.blob")).exists()),
        "preview must not delete"
    );
    assert_eq!(
        ok(&app, &admin, "PUT", &path, input.clone()).await["generation"],
        1
    );
    assert_eq!(
        request(&app, Some(&admin), "PUT", &path, input).await.0,
        StatusCode::CONFLICT
    );
    // A file descriptor opened before deletion remains a consistent download stream.
    let download = format!("/api/runs/{}/artifacts/{}", expired[0].0, expired[0].1);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&download)
                .header("cookie", &admin.cookies)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (first, second) = tokio::join!(
        p4forge_server::retention::tick(&state),
        p4forge_server::retention::tick(&state)
    );
    assert_eq!(
        first.map_err(|e| e.1).unwrap() + second.map_err(|e| e.1).unwrap(),
        4
    );
    assert_eq!(
        &to_bytes(response.into_body(), 100).await.unwrap()[..],
        b"payload"
    );
    for (_, id) in &kept {
        assert!(
            disk.path().join(format!("{id}.blob")).exists(),
            "protected artifact {id}"
        );
    }
    for (_, id) in &expired {
        assert!(!disk.path().join(format!("{id}.blob")).exists());
    }
    assert_eq!(
        request(&app, Some(&admin), "GET", &download, Value::Null)
            .await
            .0,
        StatusCode::GONE
    );
    let history = ok(
        &app,
        &viewer,
        "GET",
        &format!("/api/runs/{}/artifacts", expired[0].0),
        Value::Null,
    )
    .await;
    assert!(!history[0]["deleted_at"].is_null());
    assert_eq!(history[0]["sha256"], p4forge_domain::hash_bytes(b"payload"));
    assert_eq!(
        p4forge_server::retention::tick(&state)
            .await
            .map_err(|e| e.1)
            .unwrap(),
        0
    );
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='artifact.deleted'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audits, 4);
    let new_failed =
        retention_artifact(&pool, &admin, &pipeline, agent, 2, "failed", disk.path()).await;
    ok(
        &app,
        &admin,
        "PUT",
        &path,
        json!({"generation":1,"enabled":false,"retention_days":30,"keep_successful_runs":5}),
    )
    .await;
    assert_eq!(
        p4forge_server::retention::tick(&state)
            .await
            .map_err(|e| e.1)
            .unwrap(),
        0
    );
    assert!(
        disk.path().join(format!("{}.blob", new_failed.1)).exists(),
        "disabled policy must stop new deletions"
    );
    assert_eq!(
        ok(&app, &viewer, "GET", &path, Value::Null).await["summary"]["deleted_files"],
        4
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn artifact_retention_retries_storage_errors_and_resumes_committed_deletions(pool: PgPool) {
    let disk = tempfile::tempdir().unwrap();
    let state = state_at(&pool, Perforce::new("/unused"), disk.path().into());
    let app = p4forge_server::router(state.clone());
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo test")).await;
    let created = new_agent(&app, &admin, "retention-recovery").await;
    let agent = Uuid::parse_str(created["agent"]["id"].as_str().unwrap()).unwrap();
    let (_, failed) =
        retention_artifact(&pool, &admin, &pipeline, agent, 2, "failed", disk.path()).await;
    let (_, missing) =
        retention_artifact(&pool, &admin, &pipeline, agent, 2, "failed", disk.path()).await;
    // Simulate a committed intent, with a process interruption after one unlink.
    sqlx::query("UPDATE artifacts SET purge_requested_at=now(),purge_reason='incomplete',purge_generation=1 WHERE id=ANY($1)").bind(vec![failed,missing]).execute(&pool).await.unwrap();
    let failed_path = disk.path().join(format!("{failed}.blob"));
    std::fs::remove_file(&failed_path).unwrap();
    std::fs::create_dir(&failed_path).unwrap();
    std::fs::remove_file(disk.path().join(format!("{missing}.blob"))).unwrap();
    // A disabled policy must not abandon a previously committed deletion intent.
    assert_eq!(
        p4forge_server::retention::tick(&state)
            .await
            .map_err(|e| e.1)
            .unwrap(),
        1
    );
    let info = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/projects/{project}/artifact-retention"),
        Value::Null,
    )
    .await;
    assert_eq!(info["summary"]["failed_deletions"], 1);
    assert_eq!(info["summary"]["pending_files"], 1);
    let deleted: bool =
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM artifacts WHERE id=$1")
            .bind(failed)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!deleted);
    std::fs::remove_dir(failed_path).unwrap();
    let restarted = state_at(&pool, Perforce::new("/unused"), disk.path().into());
    assert_eq!(
        p4forge_server::retention::tick(&restarted)
            .await
            .map_err(|e| e.1)
            .unwrap(),
        1
    );
    assert_eq!(
        p4forge_server::retention::tick(&restarted)
            .await
            .map_err(|e| e.1)
            .unwrap(),
        0
    );
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='artifact.deleted'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audits, 2);
}

#[cfg(unix)]
#[sqlx::test(migrations = "../../migrations")]
async fn managed_tickets_are_scoped_renewed_and_gate_dispatch(pool: PgPool) {
    use p4forge_perforce::credentials::atomic_private_file;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let cli = temp.path().join("p4");
    let fail = temp.path().join("fail");
    let low = temp.path().join("low");
    let count = temp.path().join("renewals");
    let script = format!(
        r#"#!/bin/sh
if test -e '{fail}'; then echo 'secret diagnostic MUST NOT LEAK' >&2; exit 1; fi
user=''
while test $# -gt 0; do
 case "$1" in -u) shift; user="$1";; info) printf '... ServerID fixture\n'; exit 0;; login) shift; break;; esac
 shift
done
case "$1" in
 -s) test -s "$P4TICKETS" || exit 1; if test -e '{low}'; then printf '... TicketExpiration 100\n'; else printf '... TicketExpiration 43200\n'; fi;;
 -a) read password; test "$password" = 'fixture-password' || exit 1; echo renewed >> '{count}'; printf 'host:1666=%s:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n' "$user" > "$P4TICKETS"; rm -f '{low}';;
 *) exit 1;;
esac
"#,
        fail = fail.display(),
        low = low.display(),
        count = count.display()
    );
    std::fs::write(&cli, script).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let p4 = Perforce::new(cli);
    let app = app(&pool, p4.clone());
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo test")).unwrap();
    definition.workspace = p4forge_domain::Workspace::Pipeline;
    definition.max_parallel_jobs = 1;
    definition.stages = vec!["build".into()];
    for job in &mut definition.jobs {
        job.stage = "build".into();
    }
    definition.publish_generated = Some(p4forge_domain::GeneratedPublication {
        user: "publisher".into(),
        paths: vec!["generated".into()],
    });
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    let a = new_agent(&app, &admin, "managed").await;
    let other = new_agent(&app, &admin, "unbound").await;
    let token = a["token"].as_str().unwrap();
    let agent = a["agent"]["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let read_id = Uuid::new_v4();
    let publish_id = Uuid::new_v4();
    let password = temp.path().join("password");
    atomic_private_file(&password, b"fixture-password\n").unwrap();
    let policy = temp.path().join("policy.json");
    let entry = |id: Uuid, role: &str, user: &str| json!({"id":id,"project_id":project,"role":role,"connection":{"port":"127.0.0.1:16660","user":user},"server_id":"fixture","password_file":password,"agents":[agent]});
    let config = json!({"credentials":[entry(read_id,"read","p4forge"),entry(publish_id,"publish","publisher")]});
    atomic_private_file(&policy, &serde_json::to_vec(&config).unwrap()).unwrap();
    let directory = temp.path().join("managed");
    let broker = p4forge_server::credentials::Broker::load(&pool, &policy, &directory)
        .await
        .unwrap();
    let mut state = state_at(&pool, p4.clone(), temp.path().join("artifacts"));
    state.credentials = Some(broker.clone());
    let app = p4forge_server::router(state);
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let mut claim = claim_input();
    claim["protocol_version"] = json!(6);
    assert!(agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].is_null());
    broker.tick(&pool, &p4).await.unwrap();
    let grants = agent_ok(&app, token, "/agent/credentials", Value::Null).await;
    assert_eq!(grants.as_array().unwrap().len(), 2);
    assert!(!grants.to_string().contains("fixture-password"));
    assert!(grants[0]["tickets"].as_str().unwrap().contains("=p4forge:"));
    assert_eq!(
        agent_ok(
            &app,
            other["token"].as_str().unwrap(),
            "/agent/credentials",
            Value::Null
        )
        .await,
        json!([])
    );
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            "/api/perforce-credentials",
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, None, "POST", "/agent/credentials", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let meta = ok(
        &app,
        &admin,
        "GET",
        "/api/perforce-credentials",
        Value::Null,
    )
    .await
    .to_string();
    assert!(!meta.contains("aaaaaaaa") && !meta.contains("fixture-password"));
    let report = |g: &Value, ready: bool, remaining: u64| json!({"id":g["id"],"generation":g["generation"],"ready":ready,"remaining_seconds":remaining});
    let healthy = json!([
        report(&grants[0], true, 43000),
        report(&grants[1], true, 43000)
    ]);
    assert_eq!(
        agent_request(
            &app,
            other["token"].as_str().unwrap(),
            "/agent/credentials/status",
            healthy.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    agent_ok(&app, token, "/agent/credentials/status", healthy.clone()).await;
    let mut old = claim.clone();
    old["protocol_version"] = json!(5);
    assert!(agent_ok(&app, token, "/agent/claim", old).await["assignment"].is_null());
    for column in ["server_checked_at", "agent_checked_at"] {
        sqlx::query(&format!("UPDATE agent_perforce_credentials SET {column}=now()-interval '3 minutes' WHERE agent_id=$1")).bind(agent).execute(&pool).await.unwrap();
        assert!(agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].is_null());
        broker.tick(&pool, &p4).await.unwrap();
        agent_ok(&app, token, "/agent/credentials/status", healthy.clone()).await;
    }
    agent_ok(
        &app,
        token,
        "/agent/credentials/status",
        json!([report(&grants[0], true, 200)]),
    )
    .await;
    assert!(agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].is_null());
    agent_ok(&app, token, "/agent/credentials/status", healthy.clone()).await;
    // A publication needs its own account, even when checkout authentication works.
    agent_ok(
        &app,
        token,
        "/agent/credentials/status",
        json!([report(&grants[1], false, 0)]),
    )
    .await;
    assert!(agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].is_null());
    let diagnostics = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{run}/dispatch"),
        Value::Null,
    )
    .await;
    let managed = diagnostics["jobs"][0]["candidates"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == json!(agent))
        .unwrap();
    assert_eq!(managed["reasons"], json!(["publish_credentials"]));
    assert_eq!(managed["protocol_required"], 6);
    assert!(!diagnostics.to_string().contains("aaaaaaaa"));
    assert!(!diagnostics.to_string().contains("fixture-password"));
    agent_ok(&app, token, "/agent/credentials/status", healthy.clone()).await;
    std::fs::write(&fail, "").unwrap();
    broker.tick(&pool, &p4).await.unwrap();
    assert!(agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].is_null());
    let failed = agent_ok(&app, token, "/agent/credentials", Value::Null).await;
    assert!(failed[0]["tickets"].is_null());
    let meta = ok(
        &app,
        &admin,
        "GET",
        "/api/perforce-credentials",
        Value::Null,
    )
    .await
    .to_string();
    assert!(!meta.contains("MUST NOT LEAK"));
    std::fs::remove_file(&fail).unwrap();
    std::fs::write(&low, "").unwrap();
    broker.tick(&pool, &p4).await.unwrap();
    let fresh = agent_ok(&app, token, "/agent/credentials", Value::Null).await;
    // Old generation acknowledgement cannot reopen dispatch after rotation/recovery.
    assert_eq!(
        agent_request(&app, token, "/agent/credentials/status", healthy)
            .await
            .0,
        StatusCode::CONFLICT
    );
    agent_ok(
        &app,
        token,
        "/agent/credentials/status",
        json!([
            report(&fresh[0], true, 43000),
            report(&fresh[1], true, 43000)
        ]),
    )
    .await;
    assert!(std::fs::read_to_string(&count).unwrap().lines().count() >= 3);
    let assigned = agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].clone();
    assert_eq!(assigned["run_id"], json!(run));
    // An already-issued lease remains replayable during a later credential outage.
    std::fs::write(&fail, "").unwrap();
    broker.tick(&pool, &p4).await.unwrap();
    assert_eq!(
        agent_ok(&app, token, "/agent/claim", claim).await["assignment"]["attempt_id"],
        assigned["attempt_id"]
    );
    sqlx::query("UPDATE agents SET active=false WHERE id=$1")
        .bind(agent)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        agent_request(&app, token, "/agent/credentials", Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
#[cfg(unix)]
async fn change_triggers_are_atomic_filtered_restartable_and_authorized(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let cli = temp.path().join("p4");
    std::fs::write(temp.path().join("head"), "10").unwrap();
    std::fs::write(&cli, r#"#!/bin/sh
base="${0%/*}"
[ ! -f "$base/offline" ] || exit 1
for last; do :; done
case "$*" in
  *'login -s') printf '... User p4forge\n' ;;
  *'info -s')
    if [ -f "$base/changed-server" ]; then server=other; else server=fixture; fi
    printf '... ServerID %s\n... serverVersion fixture\n' "$server" ;;
  *'changes -s submitted -m 1'*)
    head=$(cat "$base/head")
    case "$*" in *' -r -e '*) change=$last;; *) change=$head;; esac
    [ "$change" -le "$head" ] || exit 0
    printf '... change %s\n... status submitted\n... user test\n' "$change" ;;
  *'describe -s '*)
    printf '... change %s\n... status submitted\n' "$last"
    case "$last" in
      11) printf '... depotFile0 //streams/main/Client/x\n... action0 edit\n';;
      12) printf '... depotFile0 //streams/main/Server/generated/x\n... action0 add\n';;
      13) printf '... depotFile0 //streams/main/Server/deleted.rs\n... action0 delete\n';;
      *) printf '... depotFile0 //streams/main/Server/generated/x\n... depotFile1 //streams/main/Server/lib.rs\n... action0 edit\n... action1 edit\n';;
    esac ;;
  *'stream -o '*)
    [ ! -f "$base/fail-snapshot" ] || exit 1
    printf '... Stream //streams/main\n... Parent none\n... Paths0 share ...\n' ;;
  *'client -o -S '*) printf '... Client %s\n... Stream //streams/main\n... View0 //streams/main/... //%s/...\n' "$last" "$last" ;;
  *) exit 1 ;;
esac
"#).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = state_at(&pool, Perforce::new(&cli), temp.path().join("artifacts"));
    let app = p4forge_server::router(state.clone());
    let admin = login(&pool, true).await;
    let outsider = login(&pool, false).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo first")).await;
    let path = format!("/api/pipelines/{pipeline}/trigger");
    let mut config = json!({"generation":0,"enabled":true,"version":1,"includes":["//streams/main/Server/..."],"excludes":["//streams/main/Server/generated/..."]});
    assert_eq!(
        request(&app, None, "GET", &path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, Some(&outsider), "PUT", &path, config.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'viewer')")
        .bind(project.parse::<Uuid>().unwrap())
        .bind(outsider.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, Some(&outsider), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, Some(&outsider), "PUT", &path, config.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let initial = ok(&app, &admin, "PUT", &path, config.clone()).await;
    assert_eq!(initial["trigger"]["cursor"], 10);
    assert!(initial["trigger"].get("source_binding").is_none());
    assert_eq!(
        request(&app, Some(&admin), "PUT", &path, config.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    config["generation"] = json!(1);
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["events"],
        json!([])
    );
    // New publication cannot silently switch the trigger's pinned definition.
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo second")).await;
    std::fs::write(temp.path().join("head"), "14").unwrap();
    std::fs::write(temp.path().join("fail-snapshot"), "").unwrap();
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    let failed = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(failed["trigger"]["cursor"], 12);
    assert!(failed["trigger"]["last_error"].is_string());
    assert_eq!(failed["events"].as_array().unwrap().len(), 2);
    assert!(
        failed["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["outcome"] == "filtered")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    std::fs::remove_file(temp.path().join("fail-snapshot")).unwrap();
    // An insertion failure must roll back the run as well as cursor/event/jobs.
    sqlx::raw_sql("CREATE FUNCTION trigger_test_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test fault'; END; $$; CREATE TRIGGER trigger_test_failure BEFORE INSERT ON job_runs FOR EACH ROW EXECUTE FUNCTION trigger_test_failure();").execute(&pool).await.unwrap();
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["trigger"]["cursor"],
        12
    );
    sqlx::raw_sql(
        "DROP TRIGGER trigger_test_failure ON job_runs; DROP FUNCTION trigger_test_failure();",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Fresh AppState models restart; simultaneous schedulers cannot duplicate a run.
    let restarted = state_at(&pool, Perforce::new(&cli), temp.path().join("artifacts"));
    let (a, b) = tokio::join!(
        p4forge_server::triggers::tick(&state),
        p4forge_server::triggers::tick(&restarted)
    );
    a.unwrap_or_else(|e| panic!("{}", e.1));
    b.unwrap_or_else(|e| panic!("{}", e.1));
    p4forge_server::triggers::tick(&restarted)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    let caught = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(caught["trigger"]["cursor"], 14);
    assert!(caught["trigger"]["last_error"].is_null());
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs WHERE version=1 AND definition->'jobs'->0->'script'->>0='echo first'").fetch_one(&pool).await.unwrap(), 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM job_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM job_dependencies")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    // Auth failure and identity replacement preserve the cursor; disable works offline.
    std::fs::write(temp.path().join("head"), "15").unwrap();
    std::fs::write(temp.path().join("offline"), "").unwrap();
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["trigger"]["cursor"],
        14
    );
    config["enabled"] = json!(false);
    ok(&app, &admin, "PUT", &path, config.clone()).await;
    std::fs::remove_file(temp.path().join("offline")).unwrap();
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["trigger"]["cursor"],
        14
    );
    config["enabled"] = json!(true);
    config["generation"] = json!(2);
    ok(&app, &admin, "PUT", &path, config.clone()).await;
    std::fs::write(temp.path().join("changed-server"), "").unwrap();
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    let changed = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(changed["trigger"]["cursor"], 14);
    assert!(
        changed["trigger"]["last_error"]
            .as_str()
            .unwrap()
            .contains("identity changed")
    );
    std::fs::remove_file(temp.path().join("changed-server")).unwrap();
    sqlx::query("UPDATE users SET active=false WHERE id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT cursor FROM pipeline_triggers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        14
    );
    sqlx::query("UPDATE project_members SET role='operator' WHERE user_id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET active=true,is_admin=false WHERE id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT cursor FROM pipeline_triggers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        14
    );
    sqlx::query("UPDATE project_members SET role='admin' WHERE project_id=$1 AND user_id=$2")
        .bind(project.parse::<Uuid>().unwrap())
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    // More than one batch is recovered in increasing order with no skipped CLs.
    std::fs::write(temp.path().join("head"), "35").unwrap();
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT cursor FROM pipeline_triggers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        30
    );
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pipeline_trigger_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 25);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        23
    );
    // Reset is explicit and skips backlog; normal save/re-enable keeps it.
    std::fs::write(temp.path().join("head"), "40").unwrap();
    config["generation"] = json!(3);
    config["reset_to_head"] = json!(true);
    ok(&app, &admin, "PUT", &path, config).await;
    p4forge_server::triggers::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT cursor FROM pipeline_triggers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        40
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        23
    );
}

#[sqlx::test(migrations = "../../migrations")]
#[cfg(unix)]
async fn artifact_inputs_pin_lineage_scope_downloads_and_protect_consumers(pool: PgPool) {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::os::unix::fs::PermissionsExt;
    let disk = tempfile::tempdir().unwrap();
    let cli = disk.path().join("p4");
    std::fs::write(&cli,r#"#!/bin/sh
for last; do :; done
case "$*" in
  *'login -s') printf '... User p4forge\n';;
  *'info -s') printf '... ServerID fixture\n... serverVersion fixture\n';;
  *'describe -s '*) printf '... change %s\n... status submitted\n' "$last";;
  *'stream -o '*) printf '... Stream //streams/main\n... Parent none\n... Paths0 share ...\n';;
  *'client -o -S '*) printf '... Client %s\n... Stream //streams/main\n... View0 //streams/main/... //%s/...\n' "$last" "$last";;
  *) exit 1;;
esac
"#).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = state_at(&pool, Perforce::new(&cli), disk.path().into());
    let app = p4forge_server::router(state.clone());
    let admin = login(&pool, true).await;
    let outsider = login(&pool, false).await;
    let (project, root, parent) = setup(&app, &admin, "127.0.0.1:16660").await;
    let child = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"consumer"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut consumer =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo consumer")).unwrap();
    consumer.workspace = p4forge_domain::Workspace::Pipeline;
    consumer.max_parallel_jobs = 1;
    consumer.stages = vec!["consume".into()];
    consumer.jobs.truncate(1);
    consumer.jobs[0].stage = "consume".into();
    consumer.artifact_inputs = vec!["output.bin".into()];
    publish_definition(&app, &admin, &child, &consumer.to_toml().unwrap()).await;
    let mut producer = consumer.clone();
    producer.artifact_inputs.clear();
    producer.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: child.parse().unwrap(),
        version: 1,
    }];
    // A published child cannot demand an artifact the immediate parent does not export.
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/pipelines/{parent}/validate"),
            json!({"toml":producer.to_toml().unwrap()})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    producer.artifacts = vec![p4forge_domain::ArtifactSpec {
        name: "output.bin".into(),
        path: "output.bin".into(),
    }];
    publish_definition(&app, &admin, &parent, &producer.to_toml().unwrap()).await;
    let created = new_agent(&app, &admin, "input-agent").await;
    let agent = created["agent"]["id"].as_str().unwrap().parse().unwrap();
    let token = created["token"].as_str().unwrap();
    let other = new_agent(&app, &admin, "other-input-agent").await;
    let (old, artifact) =
        retention_artifact(&pool, &admin, &parent, agent, 40, "succeeded", disk.path()).await;
    let (recent, recent_artifact) =
        retention_artifact(&pool, &admin, &parent, agent, 1, "succeeded", disk.path()).await;
    let retention = format!("/api/projects/{project}/artifact-retention");
    ok(
        &app,
        &admin,
        "PUT",
        &retention,
        json!({"generation":0,"enabled":true,"retention_days":30,"keep_successful_runs":1}),
    )
    .await;
    // A pending downstream promise protects its producer before any child exists.
    p4forge_server::retention::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert!(disk.path().join(format!("{artifact}.blob")).exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM run_artifact_inputs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    for _ in 0..2 {
        p4forge_server::followups::tick(&state)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1));
    }
    let child_run: Uuid =
        sqlx::query_scalar("SELECT child_run_id FROM run_followups WHERE parent_run_id=$1")
            .bind(old)
            .fetch_one(&pool)
            .await
            .unwrap();
    let inputs = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{child_run}/inputs"),
        Value::Null,
    )
    .await;
    assert_eq!(inputs[0]["source_run_id"], old.to_string());
    assert_eq!(inputs[0]["artifact_id"], artifact.to_string());
    assert_eq!(inputs[0]["sha256"], p4forge_domain::hash_bytes(b"payload"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM run_artifact_inputs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert!(
        sqlx::query("UPDATE run_artifact_inputs SET sha256=$1 WHERE run_id=$2")
            .bind("0".repeat(64))
            .bind(child_run)
            .execute(&pool)
            .await
            .is_err()
    );
    assert_eq!(
        request(
            &app,
            Some(&outsider),
            "GET",
            &format!("/api/runs/{child_run}/inputs"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let run_path = format!("/api/pipelines/{child}/runs");
    let mut manual = json!({"request_id":Uuid::new_v4(),"version":1,"changelist":2});
    assert_eq!(
        request(&app, Some(&admin), "POST", &run_path, manual.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    manual["source_run_id"] = json!(Uuid::new_v4());
    assert_eq!(
        request(&app, Some(&admin), "POST", &run_path, manual.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    manual["source_run_id"] = json!(old);
    manual["changelist"] = json!(3);
    assert_eq!(
        request(&app, Some(&admin), "POST", &run_path, manual.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    manual["changelist"] = json!(2);
    let replay = ok(&app, &admin, "POST", &run_path, manual.clone()).await;
    assert_eq!(
        ok(&app, &admin, "POST", &run_path, manual.clone()).await["id"],
        replay["id"]
    );
    manual["source_run_id"] = json!(recent);
    assert_eq!(
        request(&app, Some(&admin), "POST", &run_path, manual)
            .await
            .0,
        StatusCode::CONFLICT
    );
    // The old Agent cannot claim this job, including replay of a newer Agent's claim.
    let mut claim = claim_input();
    claim["protocol_version"] = json!(6);
    assert!(agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].is_null());
    claim["protocol_version"] = json!(7);
    let assignment =
        agent_ok(&app, token, "/agent/claim", claim.clone()).await["assignment"].clone();
    assert_eq!(assignment["run_id"], child_run.to_string());
    assert_eq!(assignment["artifact_inputs"], inputs);
    claim["protocol_version"] = json!(6);
    assert_eq!(
        agent_request(&app, token, "/agent/claim", claim).await.0,
        StatusCode::CONFLICT
    );
    let attempt = assignment["attempt_id"].as_str().unwrap();
    let input_path = format!("/agent/attempts/{attempt}/inputs/{artifact}");
    let read = json!({"fence":assignment["fence"],"offset":0});
    for _ in 0..2 {
        assert_eq!(
            STANDARD
                .decode(
                    agent_ok(&app, token, &input_path, read.clone()).await["data"]
                        .as_str()
                        .unwrap()
                )
                .unwrap(),
            b"payload"
        );
    }
    assert_eq!(
        agent_request(
            &app,
            other["token"].as_str().unwrap(),
            &input_path,
            read.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        agent_request(&app, token, &input_path, json!({"fence":0,"offset":0}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        agent_request(
            &app,
            token,
            &format!("/agent/attempts/{attempt}/inputs/{recent_artifact}"),
            read.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        agent_request(
            &app,
            token,
            &input_path,
            json!({"fence":assignment["fence"],"offset":8})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // The fresh router uses the same immutable bindings, never a newer producer.
    let restarted = app_at(&pool, Perforce::new(&cli), disk.path().into());
    assert_eq!(
        agent_ok(&restarted, token, &input_path, read.clone()).await["data"],
        STANDARD.encode(b"payload")
    );
    p4forge_server::retention::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert!(disk.path().join(format!("{artifact}.blob")).exists());
    // Cancel every consumer of the old artifact; a live attempt still pins it.
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{child_run}/cancel"),
        Value::Null,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{}/cancel", replay["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(
        agent_request(&app, token, &input_path, read.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    p4forge_server::retention::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert!(disk.path().join(format!("{artifact}.blob")).exists());
    agent_ok(
        &app,
        token,
        &format!("/agent/attempts/{attempt}/complete"),
        finish_input(&assignment, "canceled", -1),
    )
    .await;
    p4forge_server::retention::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert!(!disk.path().join(format!("{artifact}.blob")).exists());
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &run_path,
            json!({"request_id":Uuid::new_v4(),"version":1,"changelist":2,"source_run_id":old})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        agent_request(&app, token, &input_path, read).await.0,
        StatusCode::CONFLICT
    );
}

async fn deployment_decision(app: &Router, admin: &Login, run: &str, approve: bool) -> Value {
    let detail = ok(
        app,
        admin,
        "GET",
        &format!("/api/runs/{run}/deployment"),
        Value::Null,
    )
    .await;
    json!({"generation":detail["deployment"]["environment"]["generation"],"input_hash":detail["input_hash"],"approve":approve,"note":"Reviewed frozen inputs for isolated test environment"})
}
async fn finish_deployment_fixture(app: &Router, token: &str, assignment: &Value, healthy: bool) {
    let attempt = assignment["attempt_id"].as_str().unwrap();
    let jobs = assignment["workspace_jobs"].as_array().unwrap();
    for (index, job) in jobs.iter().enumerate() {
        agent_ok(app,token,&format!("/agent/attempts/{attempt}/step"),json!({"fence":assignment["fence"],"job_id":job["id"],"status":"running","exit_code":null,"reason":""})).await;
        let success = healthy || index + 1 < jobs.len();
        agent_ok(app,token,&format!("/agent/attempts/{attempt}/step"),json!({"fence":assignment["fence"],"job_id":job["id"],"status":if success{"succeeded"}else{"failed"},"exit_code":if success{0}else{1},"reason":""})).await;
    }
    agent_ok(app,token,&format!("/agent/attempts/{attempt}/complete"),json!({"fence":assignment["fence"],"status":if healthy{"succeeded"}else{"failed"},"exit_code":if healthy{0}else{1},"last_log_sequence":-1,"reason":""})).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn global_executor_bindings_require_project_authority_and_are_concurrent_safe(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let member = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, _, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/projects/{project}/members"),
        json!({"user_id":member.id,"role":"editor"}),
    )
    .await;
    let global_agent = new_agent(&app, &admin, "global-agent").await;
    let agent = new_agent(&app, &admin, "allowed-agent").await;
    let other = new_agent(&app, &admin, "replacement-agent").await;
    let env = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/environments"),
        json!({"name":"restricted","pipeline_id":pipeline}),
    )
    .await;
    assert!(env["executor_agent_id"].is_null());
    let path = format!("/api/environments/{}/executor", env["id"].as_str().unwrap());
    let candidates = format!("/api/projects/{project}/deployment-agents");
    assert_eq!(
        request(&app, Some(&outsider), "GET", &candidates, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let listed = ok(&app, &member, "GET", &candidates, Value::Null).await;
    assert_eq!(listed.as_array().unwrap().len(), 3);
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("token_hash").is_none() && v.get("project_id").is_none())
    );
    let binding = json!({"generation":env["generation"],"agent_id":agent["agent"]["id"],"note":"Only the designated deployment host"});
    assert_eq!(
        request(&app, Some(&member), "POST", &path, binding.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, Some(&outsider), "POST", &path, binding.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let mut invalid = binding.clone();
    invalid["agent_id"] = json!(Uuid::new_v4());
    assert_eq!(
        request(&app, Some(&admin), "POST", &path, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut invalid = binding.clone();
    invalid["note"] = json!("");
    assert_eq!(
        request(&app, Some(&admin), "POST", &path, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/agents/{}", agent["agent"]["id"].as_str().unwrap()),
        json!({"active":false,"draining":false}),
    )
    .await;
    assert_eq!(
        request(&app, Some(&admin), "POST", &path, binding.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    // Revoked credentials cannot be reactivated; bind a fresh Agent identity.
    let replacement_agent = global_agent;
    let mut binding = binding;
    binding["agent_id"] = replacement_agent["agent"]["id"].clone();
    // Project admins can configure existing Agents without global enrollment rights.
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/projects/{project}/members"),
        json!({"user_id":member.id,"role":"admin"}),
    )
    .await;
    let mut replacement = binding.clone();
    replacement["agent_id"] = other["agent"]["id"].clone();
    let (a, b) = tokio::join!(
        request(&app, Some(&member), "POST", &path, binding),
        request(&app, Some(&admin), "POST", &path, replacement)
    );
    assert!(
        (a.0 == StatusCode::OK && b.0 == StatusCode::CONFLICT)
            || (a.0 == StatusCode::CONFLICT && b.0 == StatusCode::OK)
    );
    let updated = if a.0 == StatusCode::OK { a.1 } else { b.1 };
    assert_eq!(
        updated["generation"],
        env["generation"].as_i64().unwrap() + 1
    );
    let events: Vec<Value> = sqlx::query_scalar(
        "SELECT details FROM audit_events WHERE action='deployment.executor_changed'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["agent_id"], updated["executor_agent_id"]);
    assert!(events[0]["previous_agent_id"].is_null());
}

#[sqlx::test(migrations = "../../migrations")]
async fn deployments_require_approval_serialize_environments_and_restore_frozen_success(
    pool: PgPool,
) {
    use std::os::unix::fs::PermissionsExt;
    let disk = tempfile::tempdir().unwrap();
    let cli = disk.path().join("p4");
    std::fs::write(&cli,r#"#!/bin/sh
for last; do :; done
case "$*" in
  *'login -s') printf '... User p4forge\n';;
  *'info -s') printf '... ServerID fixture\n... serverVersion fixture\n';;
  *'describe -s '*) printf '... change %s\n... status submitted\n' "$last";;
  *'stream -o '*) printf '... Stream //streams/main\n... Parent none\n... Paths0 share ...\n';;
  *'client -o -S '*) printf '... Client %s\n... Stream //streams/main\n... View0 //streams/main/... //%s/...\n' "$last" "$last";;
  *) exit 1;;
esac
"#).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = state_at(&pool, Perforce::new(&cli), disk.path().into());
    let app = p4forge_server::router(state.clone());
    let admin = login(&pool, true).await;
    let editor = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, root, parent) = setup(&app, &admin, "127.0.0.1:16660").await;
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/projects/{project}/members"),
        json!({"user_id":editor.id,"role":"editor"}),
    )
    .await;
    let pipeline = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"deployment"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let env = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/environments"),
        json!({"name":"staging","pipeline_id":pipeline}),
    )
    .await;
    let environment = env["id"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            Some(&editor),
            "POST",
            &format!("/api/projects/{project}/environments"),
            json!({"name":"forbidden","pipeline_id":pipeline})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            Some(&outsider),
            "GET",
            &format!("/api/projects/{project}/environments"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo deploy")).unwrap();
    definition.workspace = p4forge_domain::Workspace::Pipeline;
    definition.max_parallel_jobs = 1;
    definition.stages = vec!["deploy".into(), "health".into()];
    definition.jobs[0].stage = "deploy".into();
    definition.jobs[1].stage = "health".into();
    definition.artifacts = vec![p4forge_domain::ArtifactSpec {
        name: "output.bin".into(),
        path: "output.bin".into(),
    }];
    publish_definition(&app, &admin, &parent, &definition.to_toml().unwrap()).await;
    definition.artifacts.clear();
    definition.artifact_inputs = vec!["output.bin".into()];
    definition.deployment = Some(p4forge_domain::Deployment {
        environment_id: environment.parse().unwrap(),
        health_check: "test".into(),
    });
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/pipelines/{parent}/validate"),
            json!({"toml":definition.to_toml().unwrap()})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    let agent = new_agent(&app, &admin, "deploy-agent").await;
    let token = agent["token"].as_str().unwrap();
    let agent_id = agent["agent"]["id"].as_str().unwrap().parse().unwrap();
    let (source, artifact) = retention_artifact(
        &pool,
        &admin,
        &parent,
        agent_id,
        40,
        "succeeded",
        disk.path(),
    )
    .await;
    let _recent = retention_artifact(
        &pool,
        &admin,
        &parent,
        agent_id,
        1,
        "succeeded",
        disk.path(),
    )
    .await;
    let path = format!("/api/pipelines/{pipeline}/runs");
    let make =
        || json!({"version":1,"request_id":Uuid::new_v4(),"changelist":2,"source_run_id":source});
    let a = ok(&app, &admin, "POST", &path, make()).await;
    let b = ok(&app, &admin, "POST", &path, make()).await;
    let ar = a["id"].as_str().unwrap();
    let br = b["id"].as_str().unwrap();
    let claim = || {
        let mut v = claim_input();
        v["protocol_version"] = json!(7);
        v
    };
    assert!(agent_ok(&app, token, "/agent/claim", claim()).await["assignment"].is_null());
    // An existing/unconfigured environment fails closed, including approval.
    let pending = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{ar}/dispatch"),
        Value::Null,
    )
    .await;
    assert_eq!(pending["jobs"][0]["reason"], "awaiting_approval");
    assert_eq!(
        pending["run_issues"],
        json!(["awaiting_approval", "executor_missing"])
    );
    let unbound = deployment_decision(&app, &admin, ar, true).await;
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/runs/{ar}/deployment"),
            unbound.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let binding_path = format!("/api/environments/{environment}/executor");
    let binding =
        json!({"generation":env["generation"],"agent_id":agent_id,"note":"Dedicated deploy host"});
    ok(&app, &admin, "POST", &binding_path, binding.clone()).await;
    assert_eq!(
        request(&app, Some(&admin), "POST", &binding_path, binding)
            .await
            .0,
        StatusCode::CONFLICT
    );
    // Changing the executor invalidates an already-open approval form.
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/runs/{ar}/deployment"),
            unbound
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let da = deployment_decision(&app, &admin, ar, true).await;
    let db = deployment_decision(&app, &admin, br, true).await;
    // Fixture-only capability/revocation changes: neither permits new approval.
    sqlx::query("UPDATE agents SET pool='incompatible' WHERE id=$1")
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/runs/{ar}/deployment"),
            da.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE agents SET pool='default',active=false WHERE id=$1")
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/runs/{ar}/deployment"),
            da.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE agents SET active=true WHERE id=$1")
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            Some(&editor),
            "POST",
            &format!("/api/runs/{ar}/deployment"),
            da.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut wrong = da.clone();
    wrong["input_hash"] = json!("0".repeat(64));
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("/api/runs/{ar}/deployment"),
            wrong
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // Concurrent administrators cannot approve two holders of the same environment.
    let approval_a = format!("/api/runs/{ar}/deployment");
    let approval_b = format!("/api/runs/{br}/deployment");
    let (ra, rb) = tokio::join!(
        request(&app, Some(&admin), "POST", &approval_a, da),
        request(&app, Some(&admin), "POST", &approval_b, db)
    );
    assert!(
        (ra.0 == StatusCode::OK && rb.0 == StatusCode::CONFLICT)
            || (rb.0 == StatusCode::OK && ra.0 == StatusCode::CONFLICT)
    );
    let (winner, waiting) = if ra.0 == StatusCode::OK {
        (ar, br)
    } else {
        (br, ar)
    };
    let other = new_agent(&app, &admin, "other-deploy-executor").await;
    let other_token = other["token"].as_str().unwrap();
    // Same project, pool, OS and architecture does not grant deployment access.
    assert!(agent_ok(&app, other_token, "/agent/claim", claim()).await["assignment"].is_null());
    let dispatch = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{winner}/dispatch"),
        Value::Null,
    )
    .await;
    assert_eq!(dispatch["run_issues"], json!([]));
    assert_eq!(dispatch["jobs"][0]["candidates"]["registered"], 1);
    assert_eq!(
        dispatch["jobs"][0]["candidates"]["items"][0]["id"],
        agent["agent"]["id"]
    );
    let approved = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{winner}/deployment"),
        Value::Null,
    )
    .await;
    assert_eq!(
        approved["deployment"]["executor_agent_id"],
        agent["agent"]["id"]
    );
    assert_eq!(request(&app,Some(&admin),"POST",&binding_path,json!({"generation":approved["deployment"]["environment"]["generation"],"agent_id":other["agent"]["id"],"note":"Cannot redirect an approved run"})).await.0,StatusCode::CONFLICT);
    // A denied deployment at the head of the queue does not starve normal CI.
    let ci = fixture_run(&pool, &admin, &parent).await;
    let ci_claim = agent_ok(&app, other_token, "/agent/claim", claim()).await["assignment"].clone();
    assert!(!ci_claim.is_null());
    assert_eq!(ci_claim["run_id"], json!(ci));
    finish_deployment_fixture(&app, other_token, &ci_claim, false).await;
    // Loss of approver privileges prevents dispatch even after approval.
    sqlx::query("UPDATE users SET is_admin=false WHERE id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE project_members SET role='editor' WHERE user_id=$1 AND project_id=$2")
        .bind(admin.id)
        .bind(project.parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    assert!(agent_ok(&app, token, "/agent/claim", claim()).await["assignment"].is_null());
    let invalid = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{winner}/dispatch"),
        Value::Null,
    )
    .await;
    assert_eq!(invalid["jobs"][0]["reason"], "approval_invalid");
    sqlx::query("UPDATE users SET is_admin=true WHERE id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    let assignment = agent_ok(&app, token, "/agent/claim", claim()).await["assignment"].clone();
    assert_eq!(assignment["run_id"], winner);
    let permit_path = format!(
        "/agent/attempts/{}/deployment",
        assignment["attempt_id"].as_str().unwrap()
    );
    let permit_input = json!({"fence":assignment["fence"]});
    let permit = agent_ok(&app, token, &permit_path, permit_input.clone()).await;
    assert_eq!(permit["environment_id"], environment);
    assert_eq!(permit["pipeline_id"], pipeline);
    assert_eq!(permit["run_id"], winner);
    assert_eq!(permit["artifact_inputs"], assignment["artifact_inputs"]);
    assert_eq!(
        agent_request(&app, token, &permit_path, json!({"fence":-1}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        agent_request(
            &app,
            other["token"].as_str().unwrap(),
            &permit_path,
            permit_input.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Recheck the binding at the dedicated executor's final preflight too.
    sqlx::query("UPDATE deployment_environments SET executor_agent_id=$2 WHERE id=$1")
        .bind(environment.parse::<Uuid>().unwrap())
        .bind(
            other["agent"]["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        agent_request(&app, token, &permit_path, permit_input.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE deployment_environments SET executor_agent_id=$2 WHERE id=$1")
        .bind(environment.parse::<Uuid>().unwrap())
        .bind(agent_id)
        .execute(&pool)
        .await
        .unwrap();
    // Approval is rechecked even after a worker has already claimed the attempt.
    sqlx::query("UPDATE users SET is_admin=false WHERE id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        agent_request(&app, token, &permit_path, permit_input.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE users SET is_admin=true WHERE id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    // Cannot report success without completing the final health job.
    assert_eq!(agent_request(&app,token,&format!("/agent/attempts/{}/complete",assignment["attempt_id"].as_str().unwrap()),json!({"fence":assignment["fence"],"status":"succeeded","exit_code":0,"last_log_sequence":-1,"reason":""})).await.0,StatusCode::CONFLICT);
    finish_deployment_fixture(&app, token, &assignment, true).await;
    assert_eq!(
        agent_request(&app, token, &permit_path, permit_input)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let info = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{winner}/deployment"),
        Value::Null,
    )
    .await;
    assert_eq!(info["deployment"]["status"], "succeeded");
    assert_eq!(info["deployment"]["environment"]["current_run_id"], winner);
    assert!(info["deployment"]["environment"]["active_run_id"].is_null());
    // A canceled approval that never ran releases its lock without claiming healthy.
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{waiting}/cancel"),
        Value::Null,
    )
    .await;
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/projects/{project}/artifact-retention"),
        json!({"generation":0,"enabled":true,"retention_days":30,"keep_successful_runs":1}),
    )
    .await;
    p4forge_server::retention::tick(&state)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    assert!(
        disk.path().join(format!("{artifact}.blob")).exists(),
        "deployed release inputs stay pinned"
    );
    let failed = ok(&app, &admin, "POST", &path, make()).await;
    let failed = failed["id"].as_str().unwrap();
    let decision = deployment_decision(&app, &admin, failed, true).await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{failed}/deployment"),
        decision,
    )
    .await;
    let assignment = agent_ok(&app, token, "/agent/claim", claim()).await["assignment"].clone();
    finish_deployment_fixture(&app, token, &assignment, false).await;
    // Reconstructing the router preserves uncertain status and ownership.
    let app = p4forge_server::router(state.clone());
    let info = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{failed}/deployment"),
        Value::Null,
    )
    .await;
    assert_eq!(info["deployment"]["status"], "recovery_required");
    let recovery = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{failed}/dispatch"),
        Value::Null,
    )
    .await;
    assert_eq!(recovery["run_issues"], json!(["recovery_required"]));
    assert!(
        recovery["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|job| job["candidates"].is_null())
    );
    assert_eq!(info["deployment"]["environment"]["active_run_id"], failed);
    assert_eq!(info["deployment"]["environment"]["current_run_id"], winner);
    assert_eq!(info["deployment"]["environment"]["health"], "unknown");
    let rollback_path = format!("/api/environments/{environment}/rollback");
    let mut restore = json!({"generation":info["deployment"]["environment"]["generation"],"target_run_id":winner,"request_id":Uuid::new_v4(),"note":"Restore last known good release"});
    assert_eq!(
        request(&app, Some(&admin), "POST", &rollback_path, restore.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut recover = json!({"generation":info["deployment"]["environment"]["generation"],"run_id":failed,"stopped_confirmed":false,"note":"Isolated agent completed and stopped all processes"});
    let recover_path = format!("/api/environments/{environment}/recover");
    assert_eq!(
        request(&app, Some(&admin), "POST", &recover_path, recover.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    recover["stopped_confirmed"] = json!(true);
    ok(&app, &admin, "POST", &recover_path, recover.clone()).await;
    assert_eq!(
        request(&app, Some(&admin), "POST", &recover_path, recover)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let info = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{failed}/deployment"),
        Value::Null,
    )
    .await;
    restore["generation"] = info["deployment"]["environment"]["generation"].clone();
    // Later pipeline edits do not affect the restored revision or source snapshot.
    definition.jobs[0].script = vec!["echo changed-command".into()];
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    let restored = ok(&app, &admin, "POST", &rollback_path, restore.clone()).await;
    let rr = restored["id"].as_str().unwrap();
    assert_eq!(
        ok(&app, &admin, "POST", &rollback_path, restore).await["id"],
        restored["id"]
    );
    assert_eq!(restored["version"], 1);
    assert_eq!(
        &restored["input_hash"],
        if winner == ar {
            &a["input_hash"]
        } else {
            &b["input_hash"]
        }
    );
    assert!(agent_ok(&app, token, "/agent/claim", claim()).await["assignment"].is_null());
    let decision = deployment_decision(&app, &admin, rr, true).await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{rr}/deployment"),
        decision,
    )
    .await;
    let assignment = agent_ok(&app, token, "/agent/claim", claim()).await["assignment"].clone();
    assert_eq!(assignment["run_id"], rr);
    finish_deployment_fixture(&app, token, &assignment, true).await;
    let restored_info = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{rr}/deployment"),
        Value::Null,
    )
    .await;
    assert_eq!(restored_info["deployment"]["rollback_of"], winner);
    assert_eq!(
        restored_info["deployment"]["environment"]["previous_run_id"],
        winner
    );
    assert_eq!(
        restored_info["deployment"]["environment"]["health"],
        "healthy"
    );
    // Rejecting or canceling before dispatch never leaves a holder behind.
    for approve in [false, true] {
        let queued = ok(&app, &admin, "POST", &path, make()).await;
        let qr = queued["id"].as_str().unwrap();
        let decision = deployment_decision(&app, &admin, qr, approve).await;
        ok(
            &app,
            &admin,
            "POST",
            &format!("/api/runs/{qr}/deployment"),
            decision,
        )
        .await;
        if approve {
            ok(
                &app,
                &admin,
                "POST",
                &format!("/api/runs/{qr}/cancel"),
                Value::Null,
            )
            .await;
        }
        let detail = ok(
            &app,
            &admin,
            "GET",
            &format!("/api/runs/{qr}/deployment"),
            Value::Null,
        )
        .await;
        assert_eq!(
            detail["deployment"]["status"],
            if approve { "canceled" } else { "rejected" }
        );
        assert!(detail["deployment"]["environment"]["active_run_id"].is_null());
    }
    // A lost lease keeps the environment locked even though the Run is terminal.
    let lost = ok(&app, &admin, "POST", &path, make()).await;
    let lr = lost["id"].as_str().unwrap();
    let decision = deployment_decision(&app, &admin, lr, true).await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{lr}/deployment"),
        decision,
    )
    .await;
    let assignment = agent_ok(&app, token, "/agent/claim", claim()).await["assignment"].clone();
    sqlx::query("UPDATE job_attempts SET lease_expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(
            assignment["attempt_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .execute(&pool)
        .await
        .unwrap();
    p4forge_server::agents::reap(&pool)
        .await
        .unwrap_or_else(|e| panic!("{}", e.1));
    let info = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{lr}/deployment"),
        Value::Null,
    )
    .await;
    assert_eq!(info["deployment"]["status"], "recovery_required");
    assert_eq!(info["deployment"]["environment"]["active_run_id"], lr);
    ok(&app,&admin,"POST",&recover_path,json!({"generation":info["deployment"]["environment"]["generation"],"run_id":lr,"stopped_confirmed":true,"note":"Isolated fixture: no remote process exists"})).await;
    let canceled = ok(&app, &admin, "POST", &path, make()).await;
    let cr = canceled["id"].as_str().unwrap();
    let decision = deployment_decision(&app, &admin, cr, true).await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{cr}/deployment"),
        decision,
    )
    .await;
    let assignment = agent_ok(&app, token, "/agent/claim", claim()).await["assignment"].clone();
    ok(
        &app,
        &admin,
        "POST",
        &format!("/api/runs/{cr}/cancel"),
        Value::Null,
    )
    .await;
    let info = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{cr}/deployment"),
        Value::Null,
    )
    .await;
    assert_eq!(info["deployment"]["status"], "running");
    assert_eq!(info["deployment"]["environment"]["active_run_id"], cr);
    agent_ok(&app,token,&format!("/agent/attempts/{}/complete",assignment["attempt_id"].as_str().unwrap()),json!({"fence":assignment["fence"],"status":"canceled","exit_code":null,"last_log_sequence":-1,"reason":"cleanup finished"})).await;
    let info = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/runs/{cr}/deployment"),
        Value::Null,
    )
    .await;
    assert_eq!(info["deployment"]["status"], "recovery_required");
    assert_eq!(info["deployment"]["environment"]["active_run_id"], cr);
}

#[sqlx::test(migrations = "../../migrations")]
async fn docker_status_is_authenticated_scoped_validated_and_optional(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let first = new_agent(&app, &admin, "first").await;
    let second = new_agent(&app, &admin, "second").await;
    assert_eq!(first["agent"]["docker_state"], "unknown");
    assert!(first["agent"]["docker_checked_at"].is_null());
    let token = first["token"].as_str().unwrap();
    let payload = json!({"state":"available", "engine_os":"linux"});
    assert_eq!(
        agent_request(
            &app,
            &"x".repeat(43),
            "/agent/docker-status",
            payload.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, Some(&viewer), "GET", "/api/agents", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    for invalid in [
        json!({"state":"available","engine_os":null}),
        json!({"state":"cli_missing","engine_os":"linux"}),
    ] {
        assert_eq!(
            agent_request(&app, token, "/agent/docker-status", invalid)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    for invalid in [
        json!({"state":"installed","engine_os":null}),
        json!({"state":"available","engine_os":"macos"}),
        json!({"state":"available","engine_os":"linux","agent_id":second["agent"]["id"]}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agent/docker-status")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(invalid.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    agent_ok(&app, token, "/agent/docker-status", payload.clone()).await;
    let list = ok(&app, &admin, "GET", "/api/agents", Value::Null).await;
    let updated = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == first["agent"]["id"])
        .unwrap();
    assert_eq!(updated["docker_state"], "available");
    assert_eq!(updated["docker_engine_os"], "linux");
    assert!(updated["docker_checked_at"].is_string());
    assert!(
        updated["last_seen_at"].is_null(),
        "capability reports are not heartbeats"
    );
    let untouched = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == second["agent"]["id"])
        .unwrap();
    assert_eq!(untouched["docker_state"], "unknown");
    // Losing engine access clears the former engine OS; it does not change drain state.
    agent_ok(
        &app,
        token,
        "/agent/docker-status",
        json!({"state":"engine_unavailable","engine_os":null}),
    )
    .await;
    let list = ok(&app, &admin, "GET", "/api/agents", Value::Null).await;
    let updated = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == first["agent"]["id"])
        .unwrap();
    assert!(updated["docker_engine_os"].is_null());
    assert_eq!(updated["draining"], false);
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/agents/{}", first["agent"]["id"].as_str().unwrap()),
        json!({"active":false,"draining":true}),
    )
    .await;
    assert_eq!(
        agent_request(&app, token, "/agent/docker-status", payload)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_navigation_summary_is_project_scoped_and_uses_latest_run(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let path = format!("/api/projects/{project}/pipelines");
    let initial = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(initial[0]["id"], pipeline);
    assert!(initial[0]["latest_run_id"].is_null());
    assert_eq!(initial[0]["trigger_enabled"], false);
    assert_eq!(
        request(&app, Some(&viewer), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/projects/{project}/members"),
        json!({"user_id":viewer.id,"role":"viewer"}),
    )
    .await;
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo fixture")).await;
    let old = fixture_run(&pool, &admin, &pipeline).await;
    sqlx::query("UPDATE runs SET status='failed' WHERE id=$1")
        .bind(old)
        .execute(&pool)
        .await
        .unwrap();
    let latest = fixture_run(&pool, &admin, &pipeline).await;
    sqlx::query("INSERT INTO pipeline_triggers(pipeline_id,version,generation,enabled,includes,excludes,configured_by,source_binding,server_id,cursor) VALUES($1,1,1,true,'[]','[]',$2,'{}','fixture',2)")
        .bind(Uuid::parse_str(&pipeline).unwrap()).bind(admin.id).execute(&pool).await.unwrap();
    let list = ok(&app, &viewer, "GET", &path, Value::Null).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["project_id"], project);
    assert_eq!(list[0]["current_version"], 1);
    assert_eq!(list[0]["latest_run_id"], latest.to_string());
    assert_eq!(list[0]["latest_run_status"], "queued");
    assert!(list[0]["latest_run_at"].is_string());
    assert_eq!(list[0]["trigger_enabled"], true);
    let other = ok(
        &app,
        &admin,
        "POST",
        "/api/projects",
        json!({"name":"other"}),
    )
    .await;
    let other_path = format!("/api/projects/{}/pipelines", other["id"].as_str().unwrap());
    assert_eq!(
        ok(&app, &admin, "GET", &other_path, Value::Null).await,
        json!([])
    );
    assert_eq!(
        request(&app, Some(&viewer), "GET", &other_path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let module = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/navigation.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(module.status(), StatusCode::OK);
    assert!(
        module.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn deployment_navigation_summary_separates_pending_active_and_recovery(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo fixture")).await;
    let path = format!("/api/projects/{project}/environments");
    let env = ok(
        &app,
        &admin,
        "POST",
        &path,
        json!({"name":"staging","pipeline_id":pipeline}),
    )
    .await;
    let env_id = Uuid::parse_str(env["id"].as_str().unwrap()).unwrap();
    let initial = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(initial[0]["pending_approval_count"], 0);
    assert!(initial[0]["active_deployment_status"].is_null());
    let active = fixture_run(&pool, &admin, &pipeline).await;
    let pending = fixture_run(&pool, &admin, &pipeline).await;
    sqlx::query("INSERT INTO run_deployments(run_id,environment_id,status) VALUES($1,$3,'running'),($2,$3,'awaiting_approval')")
        .bind(active).bind(pending).bind(env_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE deployment_environments SET active_run_id=$2 WHERE id=$1")
        .bind(env_id)
        .bind(active)
        .execute(&pool)
        .await
        .unwrap();
    let summary = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(summary[0]["active_deployment_status"], "running");
    assert_eq!(summary[0]["pending_approval_count"], 1);
    assert_eq!(summary[0]["pending_approval_run_id"], pending.to_string());
    sqlx::query("UPDATE run_deployments SET status='recovery_required' WHERE run_id=$1")
        .bind(active)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE run_deployments SET status='rejected' WHERE run_id=$1")
        .bind(pending)
        .execute(&pool)
        .await
        .unwrap();
    let recovered = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(
        recovered[0]["active_deployment_status"],
        "recovery_required"
    );
    assert_eq!(recovered[0]["pending_approval_count"], 0);
    assert!(recovered[0]["pending_approval_run_id"].is_null());
}

#[sqlx::test(migrations = "../../migrations")]
async fn global_agents_serve_multiple_projects_with_the_same_identity(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let member = login(&pool, false).await;
    // Enrollment works before any project exists.
    let created = new_agent(&app, &admin, "shared-runner").await;
    let token = created["token"].as_str().unwrap();
    assert!(created["agent"].get("project_id").is_none());
    assert_eq!(
        request(
            &app,
            Some(&member),
            "POST",
            "/api/agents",
            json!({"name":"unauthorized","os":"linux","arch":"x86_64","pool":"default"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let second = ok(
        &app,
        &admin,
        "POST",
        "/api/projects",
        json!({"name":"another-project"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let connection =
        ok(&app, &admin, "GET", "/api/connections", Value::Null).await[0]["id"].clone();
    let second_root = ok(&app, &admin, "POST", &format!("/api/projects/{second}/roots"), json!({"name":"main","connection_id":connection,"definition":{"kind":"stream","stream":"//streams/main"}})).await["id"].as_str().unwrap().to_owned();
    let second_pipeline = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{second}/pipelines"),
        json!({"name":"main"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (project, root, pipeline) in [
        (&project, &root, &pipeline),
        (&second, &second_root, &second_pipeline),
    ] {
        let mut definition =
            p4forge_domain::PipelineDefinition::parse(&toml(root, "echo shared")).unwrap();
        definition.jobs.truncate(1);
        publish_definition(&app, &admin, pipeline, &definition.to_toml().unwrap()).await;
        let run = fixture_run(&pool, &admin, pipeline).await;
        let mut wrong = claim_input();
        wrong["os"] = json!("windows");
        assert_eq!(
            agent_request(&app, token, "/agent/claim", wrong).await.0,
            StatusCode::CONFLICT
        );
        let mut wrong = claim_input();
        wrong["arch"] = json!("aarch64");
        assert_eq!(
            agent_request(&app, token, "/agent/claim", wrong).await.0,
            StatusCode::CONFLICT
        );
        let assignment =
            agent_ok(&app, token, "/agent/claim", claim_input()).await["assignment"].clone();
        assert_eq!(assignment["run_id"], run.to_string());
        agent_ok(&app, token, &format!("/agent/attempts/{}/complete", assignment["attempt_id"].as_str().unwrap()), json!({"fence":assignment["fence"],"status":"succeeded","exit_code":0,"reason":"","last_log_sequence":-1})).await;
        let candidates = ok(
            &app,
            &admin,
            "GET",
            &format!("/api/projects/{project}/deployment-agents"),
            Value::Null,
        )
        .await;
        assert_eq!(candidates[0]["id"], created["agent"]["id"]);
        let environment = ok(
            &app,
            &admin,
            "POST",
            &format!("/api/projects/{project}/environments"),
            json!({"name":"staging","pipeline_id":pipeline}),
        )
        .await;
        let bound = ok(&app, &admin, "POST", &format!("/api/environments/{}/executor", environment["id"].as_str().unwrap()), json!({"generation":environment["generation"],"agent_id":created["agent"]["id"],"note":"Shared host, explicitly authorized for this environment"})).await;
        assert_eq!(bound["executor_agent_id"], created["agent"]["id"]);
    }
    let audit_project: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM audit_events WHERE action='agent.provisioned'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(audit_project.is_none());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job_attempts WHERE agent_id=$1 AND status='succeeded'",
    )
    .bind(
        created["agent"]["id"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap(),
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn docker_requirements_gate_dispatch_by_engine_freshness_and_protocol(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (_, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let created = new_agent(&app, &admin, "docker-runner").await;
    let token = created["token"].as_str().unwrap();
    let agent_id = created["agent"]["id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "docker info")).unwrap();
    definition.jobs[0].runner.docker = Some(p4forge_domain::DockerEngine::Linux);
    definition.jobs[1].needs.clear();
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    fixture_run(&pool, &admin, &pipeline).await;
    let claim = |version| {
        let mut v = claim_input();
        v["protocol_version"] = json!(version);
        v
    };
    // Older agents can select a native job in the same run without receiving new fields.
    let native = agent_ok(&app, token, "/agent/claim", claim(7)).await["assignment"].clone();
    assert_eq!(native["job"]["name"], "test");
    assert!(native["job"]["runner"].get("docker").is_none());
    agent_ok(&app, token, &format!("/agent/attempts/{}/complete",native["attempt_id"].as_str().unwrap()),json!({"fence":native["fence"],"status":"succeeded","exit_code":0,"reason":"","last_log_sequence":-1})).await;
    assert!(agent_ok(&app, token, "/agent/claim", claim(8)).await["assignment"].is_null());
    for report in [
        json!({"state":"cli_missing","engine_os":null}),
        json!({"state":"engine_unavailable","engine_os":null}),
        json!({"state":"check_failed","engine_os":null}),
        json!({"state":"available","engine_os":"windows"}),
    ] {
        agent_ok(&app, token, "/agent/docker-status", report).await;
        assert!(agent_ok(&app, token, "/agent/claim", claim(8)).await["assignment"].is_null());
    }
    agent_ok(
        &app,
        token,
        "/agent/docker-status",
        json!({"state":"available","engine_os":"linux"}),
    )
    .await;
    for seconds in [-151_i64, 60] {
        sqlx::query("UPDATE agents SET docker_checked_at=now()+$2*interval '1 second' WHERE id=$1")
            .bind(agent_id)
            .bind(seconds as f64)
            .execute(&pool)
            .await
            .unwrap();
        assert!(agent_ok(&app, token, "/agent/claim", claim(8)).await["assignment"].is_null());
    }
    agent_ok(
        &app,
        token,
        "/agent/docker-status",
        json!({"state":"available","engine_os":"linux"}),
    )
    .await;
    assert!(agent_ok(&app, token, "/agent/claim", claim(7)).await["assignment"].is_null());
    let request = claim(8);
    let docker = agent_ok(&app, token, "/agent/claim", request.clone()).await["assignment"].clone();
    assert_eq!(docker["job"]["name"], "build");
    assert_eq!(docker["job"]["runner"]["docker"], "linux");
    let mut old_retry = request.clone();
    old_retry["protocol_version"] = json!(7);
    assert_eq!(
        agent_request(&app, token, "/agent/claim", old_retry)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        agent_ok(&app, token, "/agent/claim", request).await["assignment"]["attempt_id"],
        docker["attempt_id"]
    );
}

#[sqlx::test(migrations = false)]
async fn global_agent_migration_preserves_credentials_and_capabilities(pool: PgPool) {
    for migration in sqlx::migrate!("../../migrations")
        .iter()
        .filter(|m| m.version < 17)
    {
        sqlx::raw_sql(&migration.sql).execute(&pool).await.unwrap();
    }
    let project = Uuid::new_v4();
    let agent = Uuid::new_v4();
    sqlx::query("INSERT INTO projects(id,name) VALUES($1,'legacy')")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents(id,name,project_id,pool,os,arch,token_hash,docker_state,docker_engine_os,docker_checked_at) VALUES($1,'retained',$2,'default','macos','aarch64',$3,'available','linux',now())").bind(agent).bind(project).bind(vec![42_u8;32]).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../../../migrations/0017_global_agents.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let retained: (Uuid, Vec<u8>, String, String, String) =
        sqlx::query_as("SELECT id,token_hash,os,arch,docker_engine_os FROM agents")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        retained,
        (
            agent,
            vec![42_u8; 32],
            "macos".into(),
            "aarch64".into(),
            "linux".into()
        )
    );
    let details: Value =
        sqlx::query_scalar("SELECT details FROM audit_events WHERE action='agent.made_global'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(details["previous_project_id"], project.to_string());
    assert_eq!(details["agent_id"], agent.to_string());
    let columns: i64 = sqlx::query_scalar("SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='agents' AND column_name='project_id'").fetch_one(&pool).await.unwrap();
    assert_eq!(columns, 0);
}

async fn workspace_pipeline(app: &Router, admin: &Login, name: &str) -> (Uuid, String) {
    let project = ok(app, admin, "POST", "/api/projects", json!({"name":name})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let connection = ok(
        app,
        admin,
        "POST",
        "/api/connections",
        json!({"name":name,"definition":{"port":"127.0.0.1:16660","user":"p4forge"}}),
    )
    .await["id"]
        .clone();
    let root = ok(app, admin, "POST", &format!("/api/projects/{project}/roots"), json!({"name":"main","connection_id":connection,"definition":{"kind":"stream","stream":"//streams/main"}})).await["id"].as_str().unwrap().to_owned();
    let pipeline = ok(
        app,
        admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"main"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish_definition(app, admin, &pipeline, &toml(&root, "echo test")).await;
    (project.parse().unwrap(), pipeline)
}

#[sqlx::test(migrations = "../../migrations")]
async fn pipeline_connections_follow_pinned_versions_and_scope_history_and_artifacts(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, root, parent) = setup(&app, &admin, "127.0.0.1:16660").await;
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'viewer')")
        .bind(project.parse::<Uuid>().unwrap())
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    let mut ids = Vec::new();
    for name in ["child", "leaf", "sibling", "unpublished"] {
        ids.push(
            ok(
                &app,
                &admin,
                "POST",
                &format!("/api/projects/{project}/pipelines"),
                json!({"name":name}),
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let (child, leaf, sibling, draft) = (&ids[0], &ids[1], &ids[2], &ids[3]);
    let (_private, hidden) = workspace_pipeline(&app, &admin, "private-project").await;
    let mut base =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo PRIVATE_SCRIPT_VALUE"))
            .unwrap();
    base.workspace = p4forge_domain::Workspace::Pipeline;
    base.max_parallel_jobs = 1;
    base.stages = vec!["build".into()];
    for job in &mut base.jobs {
        job.stage = "build".into();
    }
    let mut leaf_definition = base.clone();
    leaf_definition.artifact_inputs = vec!["report.json".into()];
    publish_definition(&app, &admin, leaf, &leaf_definition.to_toml().unwrap()).await;
    publish_definition(&app, &admin, sibling, &base.to_toml().unwrap()).await;
    let mut child_definition = base.clone();
    child_definition.artifact_inputs = vec!["release.json".into()];
    child_definition.artifacts = vec![p4forge_domain::ArtifactSpec {
        name: "report.json".into(),
        path: "private/report.json".into(),
    }];
    child_definition.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: leaf.parse().unwrap(),
        version: 1,
    }];
    publish_definition(&app, &admin, child, &child_definition.to_toml().unwrap()).await;
    let old_run = fixture_run(&pool, &admin, child).await;
    sqlx::query("UPDATE runs SET status='succeeded' WHERE id=$1")
        .bind(old_run)
        .execute(&pool)
        .await
        .unwrap();
    publish_definition(&app, &admin, &parent, &base.to_toml().unwrap()).await;
    let mut parent_definition = base.clone();
    parent_definition.artifacts = vec![p4forge_domain::ArtifactSpec {
        name: "release.json".into(),
        path: "private/release.json".into(),
    }];
    parent_definition.publish_generated = Some(p4forge_domain::GeneratedPublication {
        user: "private-publisher".into(),
        paths: vec!["generated".into()],
    });
    parent_definition.on_success = vec![
        p4forge_domain::PipelineTarget {
            pipeline_id: child.parse().unwrap(),
            version: 1,
        },
        p4forge_domain::PipelineTarget {
            pipeline_id: sibling.parse().unwrap(),
            version: 1,
        },
    ];
    publish_definition(&app, &admin, &parent, &parent_definition.to_toml().unwrap()).await;
    // Later child publication changes its graph and latest run. Parent still uses child v1.
    publish_definition(&app, &admin, child, &base.to_toml().unwrap()).await;
    let new_run = fixture_run(&pool, &admin, child).await;
    let path = format!("/api/pipelines/{parent}/connections");
    let graph = ok(&app, &viewer, "GET", &path, Value::Null).await;
    assert_eq!(graph["root"], format!("{parent}:v2"));
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 3);
    let node = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["pipeline_id"] == *child)
        .unwrap();
    assert_eq!(node["version"], 1);
    assert_eq!(node["current_version"], 2);
    assert_eq!(node["latest_run"]["id"], json!(old_run));
    assert_eq!(node["latest_run"]["status"], "succeeded");
    let edge = graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["to"] == format!("{child}:v1"))
        .unwrap();
    assert_eq!(edge["artifacts"], json!(["release.json"]));
    assert_eq!(edge["changelist_mode"], "published_or_input");
    let leaf_edge = graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["to"] == format!("{leaf}:v1"))
        .unwrap();
    assert_eq!(leaf_edge["artifacts"], json!(["report.json"]));
    assert_eq!(leaf_edge["changelist_mode"], "input");
    for secret in [
        "PRIVATE_SCRIPT_VALUE",
        "private-publisher",
        "private/release.json",
        "source_snapshot",
        "private-project",
        hidden.as_str(),
        &new_run.to_string(),
    ] {
        assert!(!graph.to_string().contains(secret), "{secret}");
    }
    let prior = ok(
        &app,
        &viewer,
        "GET",
        &format!("{path}?version=1"),
        Value::Null,
    )
    .await;
    assert_eq!(prior["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(prior["edges"], json!([]));
    let latest_child = ok(
        &app,
        &viewer,
        "GET",
        &format!("/api/pipelines/{child}/connections"),
        Value::Null,
    )
    .await;
    assert_eq!(latest_child["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(latest_child["nodes"][0]["latest_run"]["id"], json!(new_run));
    let unpublished = ok(
        &app,
        &viewer,
        "GET",
        &format!("/api/pipelines/{draft}/connections"),
        Value::Null,
    )
    .await;
    assert!(unpublished["root"].is_null());
    assert_eq!(unpublished["nodes"], json!([]));
    for (url, expected) in [
        (format!("{path}?version=0"), StatusCode::BAD_REQUEST),
        (format!("{path}?version=999"), StatusCode::NOT_FOUND),
        (
            format!("/api/pipelines/{hidden}/connections"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(
            request(&app, Some(&viewer), "GET", &url, Value::Null)
                .await
                .0,
            expected
        );
    }
    assert_eq!(
        request(&app, None, "GET", &path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, Some(&outsider), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let project_path = format!("/api/projects/{project}/graph");
    let whole = ok(&app, &viewer, "GET", &project_path, Value::Null).await;
    assert_eq!(whole["pipeline_count"], 5);
    assert_eq!(whole["view"], "published");
    assert_eq!(whole["nodes"].as_array().unwrap().len(), 6);
    assert_eq!(whole["edges"].as_array().unwrap().len(), 3);
    assert_eq!(whole["warnings"], json!([]));
    let nodes = whole["nodes"].as_array().unwrap();
    for (version, run) in [(1, old_run), (2, new_run)] {
        let node = nodes
            .iter()
            .find(|n| n["key"] == format!("{child}:v{version}"))
            .unwrap();
        assert_eq!(node["latest_run"]["id"], json!(run));
        assert_eq!(node["latest_run"]["version"], version);
    }
    assert!(
        nodes
            .iter()
            .any(|n| n["pipeline_id"] == *draft && n["version"] == 0)
    );
    assert!(!nodes.iter().any(|n| n["key"] == format!("{parent}:v1")));
    for secret in [
        "PRIVATE_SCRIPT_VALUE",
        "private-publisher",
        "private/release.json",
        "source_snapshot",
        "private-project",
        hidden.as_str(),
    ] {
        assert!(!whole.to_string().contains(secret), "{secret}");
    }
    let configured = ok(
        &app,
        &viewer,
        "GET",
        &format!("{project_path}?view=configuration"),
        Value::Null,
    )
    .await;
    assert_eq!(configured["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(configured["edges"], json!([]));
    assert!(
        configured["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["version"].is_null())
    );
    assert_eq!(
        request(&app, None, "GET", &project_path, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, Some(&outsider), "GET", &project_path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            &format!("/api/projects/{_private}/graph"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            &format!("{project_path}?view=unknown"),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("DELETE FROM project_members WHERE user_id=$1")
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, Some(&viewer), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, Some(&viewer), "GET", &project_path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM run_followups")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn project_graph_configuration_is_complete_read_only_and_separate_from_execution(
    pool: PgPool,
) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let project = ok(
        &app,
        &admin,
        "POST",
        "/api/projects",
        json!({"name":"configuration-graph"}),
    )
    .await;
    let id = project["id"].as_str().unwrap();
    let path = format!("/api/projects/{id}/graph");
    let empty = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(empty["pipeline_count"], 0);
    assert_eq!(empty["nodes"], json!([]));
    assert_eq!(empty["edges"], json!([]));
    ok(&app,&admin,"POST",&format!("/api/projects/{id}/configuration"),json!({"source_name":"root.p4forge.toml","toml":include_str!("../../../examples/root.p4forge.toml")})).await;
    let graph = ok(
        &app,
        &admin,
        "GET",
        &format!("{path}?view=configuration"),
        Value::Null,
    )
    .await;
    assert_eq!(graph["pipeline_count"], 7);
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 7);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 2);
    let nodes = graph["nodes"].as_array().unwrap();
    let generator = nodes
        .iter()
        .find(|n| n["name"] == "data-table-generate")
        .unwrap();
    let server = nodes.iter().find(|n| n["name"] == "server").unwrap();
    assert_eq!(server["job_count"], 4);
    assert_eq!(server["configuration"]["category"], "server");
    assert!(
        graph["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["from"] == generator["key"]
                && e["to"] == server["key"]
                && e["kind"] == "configuration")
    );
    assert!(
        nodes
            .iter()
            .all(|n| n["version"].is_null() && n["latest_run"].is_null())
    );
    for secret in [
        "ECR_ACCESS_KEY",
        "source_toml",
        "checkout_view",
        "script",
        "docker buildx",
    ] {
        assert!(!graph.to_string().contains(secret));
    }
    let published = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(published["nodes"].as_array().unwrap().len(), 7);
    assert!(
        published["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["version"] == 0)
    );
    assert_eq!(published["edges"], json!([]));
    // Broken imported dependencies are warnings, never silently omitted pipelines.
    sqlx::query("UPDATE configuration_drafts SET definition=jsonb_set(definition,'{needs}','[\"missing\"]') WHERE pipeline_id=$1")
        .bind(server["pipeline_id"].as_str().unwrap().parse::<Uuid>().unwrap()).execute(&pool).await.unwrap();
    let broken = ok(
        &app,
        &admin,
        "GET",
        &format!("{path}?view=configuration"),
        Value::Null,
    )
    .await;
    assert_eq!(broken["nodes"].as_array().unwrap().len(), 7);
    assert_eq!(broken["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    // Refuse an oversized project explicitly instead of claiming a partial view is complete.
    sqlx::query("INSERT INTO pipelines(id,project_id,name) SELECT gen_random_uuid(),$1,'extra-'||n FROM generate_series(1,494) n")
        .bind(id.parse::<Uuid>().unwrap()).execute(&pool).await.unwrap();
    assert_eq!(
        request(&app, Some(&admin), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn dispatch_is_scoped_read_only_and_uses_frozen_requirements_and_authenticated_protocol(
    pool: PgPool,
) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let definition = toml(&root, "echo test").replace(
        "runner = { os = \"linux\" }",
        "runner = { os = \"linux\", docker = \"linux\" }",
    );
    publish_definition(&app, &admin, &pipeline, &definition).await;
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let path = format!("/api/runs/{run}/dispatch");
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'viewer')")
        .bind(project.parse::<Uuid>().unwrap())
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    let agent = new_agent(&app, &admin, "private-agent-name").await;
    let id = agent["agent"]["id"].as_str().unwrap();
    let token = agent["token"].as_str().unwrap();
    agent_ok(
        &app,
        token,
        "/agent/docker-status",
        json!({"state":"available","engine_os":"linux"}),
    )
    .await;
    let claim = |version| {
        let mut input = claim_input();
        input["protocol_version"] = json!(version);
        input
    };
    assert!(agent_ok(&app, token, "/agent/claim", claim(7)).await["assignment"].is_null());
    let report = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(report["jobs"][0]["reason"], "agents_unavailable");
    assert_eq!(
        report["jobs"][0]["candidates"]["blockers"],
        json!({"protocol_old":1})
    );
    assert_eq!(
        report["jobs"][0]["candidates"]["items"][0]["protocol_version"],
        7
    );
    assert_eq!(report["jobs"][1]["reason"], "dependencies");
    assert_eq!(
        report["jobs"][1]["pending_dependencies"][0]["name"],
        "build"
    );
    let mut invalid = claim(99);
    invalid["os"] = json!("windows");
    assert_eq!(
        agent_request(&app, token, "/agent/claim", invalid).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["jobs"][0]["candidates"]["items"][0]["protocol_version"],
        7
    );
    for (who, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(&outsider), StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            request(&app, who, "GET", &path, Value::Null).await.0,
            expected
        );
    }
    let public = ok(&app, &viewer, "GET", &path, Value::Null).await;
    assert_eq!(public["admin_details"], false);
    assert_eq!(public["jobs"][0]["candidates"]["items"], json!([]));
    for private in [
        id,
        "private-agent-name",
        token,
        "127.0.0.1:16660",
        "echo test",
    ] {
        assert!(!public.to_string().contains(private));
    }
    // A successful idle claim reports capability, but diagnostics themselves never claim work.
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/agents/{id}"),
        json!({"active":true,"draining":true}),
    )
    .await;
    assert!(agent_ok(&app, token, "/agent/claim", claim(8)).await["assignment"].is_null());
    ok(
        &app,
        &admin,
        "PUT",
        &format!("/api/agents/{id}"),
        json!({"active":true,"draining":false}),
    )
    .await;
    publish_definition(
        &app,
        &admin,
        &pipeline,
        &definition.replace("docker = \"linux\"", "docker = \"windows\""),
    )
    .await;
    let ready = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(ready["jobs"][0]["runner"]["docker"], "linux");
    assert_eq!(ready["jobs"][0]["required_protocol"], 8);
    assert_eq!(ready["jobs"][0]["reason"], "awaiting_claim");
    assert_eq!(ready["jobs"][0]["candidates"]["precheck_passed"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM job_attempts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let assigned = agent_ok(&app, token, "/agent/claim", claim(8)).await;
    assert_eq!(assigned["assignment"]["run_id"], json!(run));
    let started = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(started["jobs"][0]["reason"], "not_waiting");
    assert!(started["jobs"][0]["candidates"].is_null());
    sqlx::query("DELETE FROM project_members WHERE user_id=$1")
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, Some(&viewer), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn dispatch_distinguishes_agent_health_capacity_and_terminal_runs(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (_, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let definition = toml(&root, "echo test")
        .replace("max_parallel_jobs = 2", "max_parallel_jobs = 1")
        .replace("needs = [\"build\"]", "needs = []")
        .replace(
            "runner = { os = \"linux\" }",
            "runner = { os = \"linux\", docker = \"linux\" }",
        );
    publish_definition(&app, &admin, &pipeline, &definition).await;
    let run = fixture_run(&pool, &admin, &pipeline).await;
    let path = format!("/api/runs/{run}/dispatch");
    let agent = new_agent(&app, &admin, "health-check").await;
    let id = agent["agent"]["id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let reset = "UPDATE agents SET active=true,draining=false,pool='default',perforce_managed=false,last_seen_at=now(),last_protocol_version=8,protocol_reported_at=now(),docker_state='available',docker_engine_os='linux',docker_checked_at=now() WHERE id=$1";
    for (change, blocker) in [
        ("active=false", "revoked"),
        ("last_seen_at=now()-interval '31 seconds'", "offline"),
        ("draining=true", "draining"),
        (
            "last_protocol_version=NULL,protocol_reported_at=NULL",
            "protocol_unknown",
        ),
        (
            "protocol_reported_at=now()-interval '151 seconds'",
            "protocol_unknown",
        ),
        (
            "docker_checked_at=now()-interval '151 seconds'",
            "docker_stale",
        ),
        (
            "docker_state='cli_missing',docker_engine_os=NULL",
            "docker_unavailable",
        ),
        ("docker_engine_os='windows'", "docker_engine"),
        ("perforce_managed=true", "read_credentials"),
    ] {
        sqlx::query(reset).bind(id).execute(&pool).await.unwrap();
        sqlx::query(&format!("UPDATE agents SET {change} WHERE id=$1"))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let report = ok(&app, &admin, "GET", &path, Value::Null).await;
        assert_eq!(
            report["jobs"][0]["candidates"]["blockers"],
            json!({blocker:1}),
            "{change}"
        );
        assert_eq!(report["jobs"][0]["candidates"]["precheck_passed"], 0);
        assert_eq!(
            report["jobs"][0]["reason"],
            if blocker == "revoked" {
                "no_agents"
            } else {
                "agents_unavailable"
            }
        );
    }
    sqlx::query(reset).bind(id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE agents SET pool='different' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["jobs"][0]["candidates"]["registered"],
        0
    );
    sqlx::query(reset).bind(id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE agents SET perforce_managed=true WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agent_perforce_credentials(agent_id,credential_id,role,connection,server_id,generation,server_state,agent_state,server_checked_at,agent_checked_at,server_expires_at,agent_expires_at) VALUES($1,$2,'read','{\"port\":\"127.0.0.1:16660\",\"user\":\"p4forge\"}','fixture',$3,'ready','ready',now(),now(),now()+interval '1 hour',now()+interval '1 hour')").bind(id).bind(Uuid::new_v4()).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["jobs"][0]["candidates"]["precheck_passed"],
        1
    );
    for change in [
        "server_id='other'",
        "agent_expires_at=now()+interval '4 minutes'",
        "server_checked_at=now()-interval '151 seconds'",
        "agent_state='error'",
    ] {
        sqlx::query("UPDATE agent_perforce_credentials SET server_id='fixture',agent_state='ready',server_checked_at=now(),agent_expires_at=now()+interval '1 hour' WHERE agent_id=$1").bind(id).execute(&pool).await.unwrap();
        sqlx::query(&format!(
            "UPDATE agent_perforce_credentials SET {change} WHERE agent_id=$1"
        ))
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            ok(&app, &admin, "GET", &path, Value::Null).await["jobs"][0]["candidates"]["blockers"],
            json!({"read_credentials":1})
        );
    }
    sqlx::query(reset).bind(id).execute(&pool).await.unwrap();
    // An expired attempt still occupies an agent until the scheduler reaps it.
    let build: Uuid =
        sqlx::query_scalar("SELECT id FROM job_runs WHERE run_id=$1 AND name='build'")
            .bind(run)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO job_attempts(id,job_id,agent_id,claim_request_id,status,lease_expires_at) VALUES($1,$2,$3,$4,'running',now()-interval '1 second')").bind(Uuid::new_v4()).bind(build).bind(id).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["jobs"][1]["candidates"]["blockers"],
        json!({"busy":1})
    );
    sqlx::query("UPDATE job_runs SET status='running' WHERE id=$1")
        .bind(build)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["jobs"][1]["reason"],
        "parallel_limit"
    );
    sqlx::query("UPDATE runs SET cancel_requested=true WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["jobs"][1]["reason"],
        "cancel_requested"
    );
    sqlx::query("UPDATE runs SET status='canceled' WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    let terminal = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(terminal["run_issues"], json!([]));
    for job in terminal["jobs"].as_array().unwrap() {
        assert_eq!(job["reason"], "not_waiting");
        assert!(job["candidates"].is_null());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn workspace_reads_scope_aggregates_search_and_paginate_without_skipping_ties(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, pipeline) = workspace_pipeline(&app, &admin, "visible-project").await;
    let (private, hidden_pipeline) = workspace_pipeline(&app, &admin, "private-project").await;
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'viewer')")
        .bind(project)
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    let original = fixture_run(&pool, &admin, &pipeline).await;
    let hidden = fixture_run(&pool, &admin, &hidden_pipeline).await;
    let ids: Vec<Uuid> = (0..105).map(|_| Uuid::new_v4()).collect();
    sqlx::query("INSERT INTO runs(id,pipeline_id,version,submitted_by,request_id,request_hash,definition,source_snapshot,input_hash,created_at) SELECT u.id,r.pipeline_id,r.version,r.submitted_by,u.id,'fixture',r.definition,r.source_snapshot,r.input_hash,now() FROM runs r CROSS JOIN unnest($2::uuid[]) AS u(id) WHERE r.id=$1").bind(original).bind(&ids).execute(&pool).await.unwrap();
    sqlx::query("UPDATE runs SET status='running' WHERE id=$1")
        .bind(original)
        .execute(&pool)
        .await
        .unwrap();
    let first = ok(
        &app,
        &viewer,
        "GET",
        "/api/workspace/runs?limit=17",
        Value::Null,
    )
    .await;
    assert_eq!(first["items"].as_array().unwrap().len(), 17);
    assert!(!first.to_string().contains("private-project"));
    assert!(first["items"][0].get("source_snapshot").is_none());
    assert!(first["items"][0].get("definition").is_none());
    // Insert a newer execution after page one. It must not shift or duplicate older pages.
    let newer = fixture_run(&pool, &admin, &pipeline).await;
    let mut rows = first["items"].as_array().unwrap().clone();
    let mut cursor = first["next_cursor"].as_str().map(str::to_owned);
    while let Some(value) = cursor {
        let page = ok(
            &app,
            &viewer,
            "GET",
            &format!("/api/workspace/runs?limit=17&cursor={value}"),
            Value::Null,
        )
        .await;
        rows.extend(page["items"].as_array().unwrap().clone());
        cursor = page["next_cursor"].as_str().map(str::to_owned);
    }
    let unique: std::collections::HashSet<_> =
        rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(rows.len(), 106);
    assert_eq!(unique.len(), 106);
    assert!(!unique.contains(newer.to_string().as_str()));
    assert!(!unique.contains(hidden.to_string().as_str()));
    for row in &rows {
        assert_eq!(row["project_id"], project.to_string());
    }
    let running = ok(
        &app,
        &viewer,
        "GET",
        "/api/workspace/runs?state=running&q=visible",
        Value::Null,
    )
    .await;
    assert_eq!(running["items"][0]["id"], original.to_string());
    assert_eq!(running["items"].as_array().unwrap().len(), 1);
    for url in [
        format!("/api/workspace/runs?project={private}"),
        "/api/workspace/runs?q=%25".into(),
        "/api/workspace/runs?q=private".into(),
    ] {
        assert!(
            ok(&app, &viewer, "GET", &url, Value::Null).await["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    for url in [
        "/api/workspace/runs?cursor=bad",
        "/api/workspace/runs?limit=0",
        "/api/workspace/runs?limit=101",
        "/api/workspace/runs?state=secret",
    ] {
        assert_eq!(
            request(&app, Some(&viewer), "GET", url, Value::Null)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let overview = ok(&app, &viewer, "GET", "/api/workspace/overview", Value::Null).await;
    assert_eq!(overview["summary"]["projects"], 1);
    assert_eq!(overview["summary"]["pipelines"], 1);
    assert_eq!(overview["summary"]["queued"], 106);
    assert_eq!(overview["summary"]["running"], 1);
    assert_eq!(overview["recent"].as_array().unwrap().len(), 8);
    assert!(!overview.to_string().contains("private-project"));
    assert_eq!(
        ok(&app, &admin, "GET", "/api/workspace/overview", Value::Null).await["summary"]["projects"],
        2
    );
    assert_eq!(
        ok(
            &app,
            &outsider,
            "GET",
            "/api/workspace/overview",
            Value::Null
        )
        .await["summary"]["projects"],
        0
    );
    for path in [
        "/api/workspace/overview",
        "/api/workspace/runs",
        "/api/operations",
    ] {
        assert_eq!(
            request(&app, None, "GET", path, Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        request(&app, Some(&viewer), "GET", "/api/operations", Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Previously issued cursors confer no access after membership is removed.
    sqlx::query("DELETE FROM project_members WHERE project_id=$1 AND user_id=$2")
        .bind(project)
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    let again = ok(
        &app,
        &viewer,
        "GET",
        &format!(
            "/api/workspace/runs?cursor={}",
            first["next_cursor"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert!(again["items"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn workspace_attention_and_operations_distinguish_approval_failures_and_stale_reports(
    pool: PgPool,
) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, pipeline) = workspace_pipeline(&app, &admin, "operations-fixture").await;
    let pipeline_id: Uuid = pipeline.parse().unwrap();
    let waiting = fixture_run(&pool, &admin, &pipeline).await;
    let recovery = fixture_run(&pool, &admin, &pipeline).await;
    let success = fixture_run(&pool, &admin, &pipeline).await;
    let old_failure = fixture_run(&pool, &admin, &pipeline).await;
    sqlx::query("UPDATE runs SET status='succeeded',finished_at=now() WHERE id=$1")
        .bind(success)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE runs SET status='failed',finished_at=now()-interval '25 hours' WHERE id=$1",
    )
    .bind(old_failure)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE runs SET status='failed',finished_at=now() WHERE id=$1")
        .bind(recovery)
        .execute(&pool)
        .await
        .unwrap();
    let env = Uuid::new_v4();
    sqlx::query("INSERT INTO deployment_environments(id,project_id,pipeline_id,name,active_run_id) VALUES($1,$2,$3,'staging',$4)").bind(env).bind(project).bind(pipeline_id).bind(recovery).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO run_deployments(run_id,environment_id,status) VALUES($1,$3,'awaiting_approval'),($2,$3,'recovery_required')").bind(waiting).bind(recovery).bind(env).execute(&pool).await.unwrap();
    let summary = ok(&app, &admin, "GET", "/api/workspace/overview", Value::Null).await;
    assert_eq!(summary["summary"]["queued"], 0);
    assert_eq!(summary["summary"]["awaiting_approval"], 1);
    assert_eq!(summary["summary"]["recovery_required"], 1);
    assert_eq!(summary["summary"]["succeeded_24h"], 1);
    assert_eq!(summary["summary"]["failed_24h"], 1);
    assert_eq!(summary["attention"][0]["id"], recovery.to_string());
    assert_eq!(summary["diagnostics"]["actions"]["total"], 2);
    assert_eq!(
        summary["diagnostics"]["actions"]["items"][0]["kind"],
        "recovery_required"
    );
    assert_eq!(
        summary["diagnostics"]["actions"]["items"][1]["kind"],
        "awaiting_approval"
    );
    assert_eq!(summary["diagnostics"]["queued"]["total"], 0);
    assert_eq!(summary["diagnostics"]["failed_24h"]["total"], 1);
    for (state, id) in [
        ("awaiting_approval", waiting),
        ("recovery_required", recovery),
    ] {
        let page = ok(
            &app,
            &admin,
            "GET",
            &format!("/api/workspace/runs?state={state}"),
            Value::Null,
        )
        .await;
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["id"], id.to_string());
    }
    let a = new_agent(&app, &admin, "native-without-docker").await;
    let b = new_agent(&app, &admin, "offline-managed").await;
    let aid: Uuid = a["agent"]["id"].as_str().unwrap().parse().unwrap();
    let bid: Uuid = b["agent"]["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE agents SET last_seen_at=now(),docker_state='cli_missing',docker_checked_at=now() WHERE id=$1").bind(aid).execute(&pool).await.unwrap();
    sqlx::query("UPDATE agents SET perforce_managed=true,last_seen_at=now()-interval '31 seconds',docker_state='available',docker_engine_os='linux',docker_checked_at=now()-interval '151 seconds' WHERE id=$1").bind(bid).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO pipeline_triggers(pipeline_id,version,generation,enabled,includes,excludes,configured_by,source_binding,server_id,cursor,checked_at) VALUES($1,1,1,true,'[]','[]',$2,'{}','fixture',0,now()-interval '151 seconds')").bind(pipeline_id).bind(admin.id).execute(&pool).await.unwrap();
    let ops = ok(&app, &admin, "GET", "/api/operations", Value::Null).await;
    assert_eq!(ops["counts"]["active_agents"], 2);
    assert_eq!(ops["counts"]["online_agents"], 1);
    assert_eq!(ops["counts"]["docker_available"], 0);
    assert_eq!(ops["issues"]["offline_agents"], 1);
    assert_eq!(ops["issues"]["credential_attention"], 1);
    assert_eq!(ops["issues"]["trigger_attention"], 1);
    assert_eq!(ops["issues"]["recovery_required"], 1);
    assert_eq!(ops["issues"]["total"], 4);
    assert!(
        !ops["issues"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["agent_id"] == aid.to_string())
    );
    // A live managed runner is healthy only when both ticket checks are fresh and valid.
    sqlx::query("UPDATE agents SET last_seen_at=now(),docker_checked_at=now() WHERE id=$1")
        .bind(bid)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agent_perforce_credentials(agent_id,credential_id,role,connection,server_id,generation,server_state,server_checked_at,server_expires_at,agent_state,agent_checked_at,agent_expires_at) VALUES($1,$2,'read','{}','fixture',$3,'ready',now(),now()+interval '1 hour','ready',now(),now()+interval '1 hour')")
        .bind(bid).bind(Uuid::new_v4()).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    let healthy = ok(&app, &admin, "GET", "/api/operations", Value::Null).await;
    assert_eq!(healthy["counts"]["docker_available"], 1);
    assert_eq!(healthy["issues"]["offline_agents"], 0);
    assert_eq!(healthy["issues"]["credential_attention"], 0);
    for change in [
        "agent_checked_at=now()-interval '151 seconds'",
        "server_checked_at=now()-interval '151 seconds'",
        "agent_expires_at=now()+interval '4 minutes'",
        "server_expires_at=now()+interval '4 minutes'",
        "agent_state='error'",
        "server_state='error'",
    ] {
        sqlx::query("UPDATE agent_perforce_credentials SET agent_checked_at=now(),server_checked_at=now(),agent_expires_at=now()+interval '1 hour',server_expires_at=now()+interval '1 hour',agent_state='ready',server_state='ready' WHERE agent_id=$1")
            .bind(bid).execute(&pool).await.unwrap();
        sqlx::query(&format!(
            "UPDATE agent_perforce_credentials SET {change} WHERE agent_id=$1"
        ))
        .bind(bid)
        .execute(&pool)
        .await
        .unwrap();
        let stale = ok(&app, &admin, "GET", "/api/operations", Value::Null).await;
        assert_eq!(stale["issues"]["credential_attention"], 1, "{change}");
    }
    // Revoked agents and disabled triggers no longer need operator attention.
    sqlx::query("UPDATE agents SET active=false WHERE id=$1")
        .bind(bid)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE pipeline_triggers SET enabled=false WHERE pipeline_id=$1")
        .bind(pipeline_id)
        .execute(&pool)
        .await
        .unwrap();
    let ops = ok(&app, &admin, "GET", "/api/operations", Value::Null).await;
    assert_eq!(ops["issues"]["total"], 1);
    assert_eq!(ops["counts"]["active_agents"], 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_chain_tracks_actual_lineage_and_uncreated_targets_without_guessing(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, root, parent) = setup(&app, &admin, "127.0.0.1:16660").await;
    let project_id: Uuid = project.parse().unwrap();
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'viewer')")
        .bind(project_id)
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    let mut pipelines = std::collections::BTreeMap::new();
    let base = p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo PRIVATE_CHAIN_SCRIPT"))
        .unwrap();
    for name in ["deploy", "never", "blocked", "pending", "verify", "failed"] {
        let pipeline = ok(
            &app,
            &admin,
            "POST",
            &format!("/api/projects/{project}/pipelines"),
            json!({"name":name}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut definition = base.clone();
        let downstream = match name {
            "verify" => Some("deploy"),
            "failed" => Some("never"),
            _ => None,
        };
        if let Some(child) = downstream {
            definition.on_success = vec![p4forge_domain::PipelineTarget {
                pipeline_id: pipelines[child],
                version: 1,
            }];
        }
        publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
        pipelines.insert(name, pipeline.parse::<Uuid>().unwrap());
    }
    let mut definition = base.clone();
    definition.on_success = ["verify", "failed", "blocked", "pending"]
        .into_iter()
        .map(|name| p4forge_domain::PipelineTarget {
            pipeline_id: pipelines[name],
            version: 1,
        })
        .collect();
    publish_definition(&app, &admin, &parent, &definition.to_toml().unwrap()).await;
    let parent_run = fixture_run(&pool, &admin, &parent).await;
    let verify = fixture_run(&pool, &admin, &pipelines["verify"].to_string()).await;
    let failed = fixture_run(&pool, &admin, &pipelines["failed"].to_string()).await;
    sqlx::query("UPDATE runs SET status='succeeded',finished_at=now() WHERE id=ANY($1)")
        .bind(vec![parent_run, verify])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE runs SET status='failed',finished_at=now() WHERE id=$1")
        .bind(failed)
        .execute(&pool)
        .await
        .unwrap();
    for (name, child, state, reason) in [
        ("verify", Some(verify), "created", ""),
        ("failed", Some(failed), "created", ""),
        ("blocked", None, "blocked", "Submitting user is inactive"),
        ("pending", None, "pending", ""),
    ] {
        sqlx::query("INSERT INTO run_followups(id,parent_run_id,pipeline_id,version,child_run_id,status,reason) VALUES($1,$2,$3,1,$4,$5,$6)").bind(Uuid::new_v4()).bind(parent_run).bind(pipelines[name]).bind(child).bind(state).bind(reason).execute(&pool).await.unwrap();
    }
    // Same CL and pipeline/version are not proof of an automatic relationship.
    let unrelated = fixture_run(&pool, &admin, &pipelines["verify"].to_string()).await;
    publish_definition(
        &app,
        &admin,
        &pipelines["verify"].to_string(),
        &base.to_toml().unwrap(),
    )
    .await;
    let latest = fixture_run(&pool, &admin, &pipelines["verify"].to_string()).await;
    let path = format!("/api/runs/{verify}/chain");
    let graph = ok(&app, &viewer, "GET", &path, Value::Null).await;
    assert_eq!(graph["root"], json!(parent_run));
    assert_eq!(graph["selected_run_id"], json!(verify));
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 7);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 6);
    let find = |name: &str| {
        graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == name)
            .unwrap()
    };
    assert_eq!(find("verify")["version"], 1);
    assert_eq!(find("verify")["run_id"], json!(verify));
    assert_eq!(find("deploy")["status"], "pending"); // success, before scheduler inserts a record
    assert_eq!(find("never")["status"], "not_scheduled");
    assert_eq!(find("blocked")["reason"], "Submitting user is inactive");
    assert_eq!(find("pending")["kind"], "followup");
    assert!(find("pending")["run_id"].is_null());
    for excluded in [
        unrelated.to_string(),
        latest.to_string(),
        "PRIVATE_CHAIN_SCRIPT".into(),
        "127.0.0.1:16660".into(),
        "request_hash".into(),
        "source_snapshot".into(),
    ] {
        assert!(!graph.to_string().contains(&excluded), "{excluded}");
    }
    assert_eq!(graph["ongoing"], true);
    // Once the scheduler records a child, the placeholder becomes that exact run.
    let deploy = fixture_run(&pool, &admin, &pipelines["deploy"].to_string()).await;
    sqlx::query("INSERT INTO run_followups(id,parent_run_id,pipeline_id,version,child_run_id,status) VALUES($1,$2,$3,1,$4,'created')").bind(Uuid::new_v4()).bind(verify).bind(pipelines["deploy"]).bind(deploy).execute(&pool).await.unwrap();
    let environment = Uuid::new_v4();
    sqlx::query("INSERT INTO deployment_environments(id,project_id,pipeline_id,name) VALUES($1,$2,$3,'chain-test')").bind(environment).bind(project_id).bind(pipelines["deploy"]).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO run_deployments(run_id,environment_id) VALUES($1,$2)")
        .bind(deploy)
        .bind(environment)
        .execute(&pool)
        .await
        .unwrap();
    let report = ok(
        &app,
        &viewer,
        "GET",
        &format!("/api/runs/{deploy}/chain"),
        Value::Null,
    )
    .await;
    let node = report["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["run_id"] == json!(deploy))
        .unwrap();
    assert_eq!(node["deployment_status"], "awaiting_approval");
    assert_eq!(report["root"], json!(parent_run));
    assert_eq!(report["nodes"].as_array().unwrap().len(), 7);
    let single = ok(
        &app,
        &viewer,
        "GET",
        &format!("/api/runs/{latest}/chain"),
        Value::Null,
    )
    .await;
    assert_eq!(single["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(single["edges"], json!([]));
    for (user, status) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(&outsider), StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            request(&app, user, "GET", &path, Value::Null).await.0,
            status
        );
    }
    let (_private, hidden) = workspace_pipeline(&app, &admin, "chain-private").await;
    let hidden_run = fixture_run(&pool, &admin, &hidden).await;
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            &format!("/api/runs/{hidden_run}/chain"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let counts_before: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM runs),(SELECT count(*) FROM run_followups)")
            .fetch_one(&pool)
            .await
            .unwrap();
    ok(&app, &viewer, "GET", &path, Value::Null).await;
    let counts_after: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM runs),(SELECT count(*) FROM run_followups)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(counts_before, counts_after);
    sqlx::query("DELETE FROM project_members WHERE user_id=$1")
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, Some(&viewer), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // Corrupt cross-project links never expose the other project's run metadata.
    sqlx::query(
        "UPDATE run_followups SET child_run_id=$1 WHERE parent_run_id=$2 AND pipeline_id=$3",
    )
    .bind(hidden_run)
    .bind(parent_run)
    .bind(pipelines["verify"])
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "GET",
            &format!("/api/runs/{parent_run}/chain"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "GET",
            &format!("/api/runs/{hidden_run}/chain"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn run_chain_waiting_cancellation_completion_and_cycles_are_explicit(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, root, parent) = setup(&app, &admin, "127.0.0.1:16660").await;
    let child = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"child"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo test")).unwrap();
    publish_definition(&app, &admin, &child, &definition.to_toml().unwrap()).await;
    definition.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: child.parse().unwrap(),
        version: 1,
    }];
    publish_definition(&app, &admin, &parent, &definition.to_toml().unwrap()).await;
    let run = fixture_run(&pool, &admin, &parent).await;
    let path = format!("/api/runs/{run}/chain");
    let child_state = |report: &Value| {
        report["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["kind"] == "followup")
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let waiting = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(child_state(&waiting), "awaiting_parent");
    assert_eq!(waiting["ongoing"], true);
    sqlx::query("UPDATE runs SET cancel_requested=true WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        child_state(&ok(&app, &admin, "GET", &path, Value::Null).await),
        "not_scheduled"
    );
    sqlx::query("UPDATE runs SET status='canceled' WHERE id=$1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    let canceled = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(canceled["ongoing"], false);
    // Even corrupt cycles cannot make a read recurse indefinitely.
    sqlx::query("INSERT INTO run_followups(id,parent_run_id,pipeline_id,version,child_run_id,status) VALUES($1,$2,$3,1,$2,'created')").bind(Uuid::new_v4()).bind(run).bind(child.parse::<Uuid>().unwrap()).execute(&pool).await.unwrap();
    assert_eq!(
        request(&app, Some(&admin), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn diagnostics_scope_limits_recorded_failures_and_current_followup_state(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let outsider = login(&pool, false).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let project_id: Uuid = project.parse().unwrap();
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo original")).await;
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'viewer')")
        .bind(project_id)
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    let target = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"consumer"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    publish_definition(&app, &admin, &target, &toml(&root, "echo consumer")).await;
    let (_, private_pipeline) = workspace_pipeline(&app, &admin, "private-diagnostics").await;
    let hidden = fixture_run(&pool, &admin, &private_pipeline).await;
    sqlx::query("UPDATE runs SET status='failed',finished_at=now() WHERE id=$1")
        .bind(hidden)
        .execute(&pool)
        .await
        .unwrap();
    let parent = fixture_run(&pool, &admin, &pipeline).await;
    let same_cl = fixture_run(&pool, &admin, &pipeline).await;
    sqlx::query("UPDATE runs SET status='succeeded',finished_at=now() WHERE id=ANY($1)")
        .bind(vec![parent, same_cl])
        .execute(&pool)
        .await
        .unwrap();
    let blocked = Uuid::new_v4();
    sqlx::query("INSERT INTO run_followups(id,parent_run_id,pipeline_id,version,status,reason) VALUES($1,$2,$3,1,'blocked','permission revoked')")
        .bind(blocked).bind(parent).bind(target.parse::<Uuid>().unwrap()).execute(&pool).await.unwrap();
    // A corrupt cross-project link must not expose the target name or its stored reason.
    sqlx::query("INSERT INTO run_followups(id,parent_run_id,pipeline_id,version,status,reason) VALUES($1,$2,$3,1,'blocked','PRIVATE REASON')")
        .bind(Uuid::new_v4()).bind(parent).bind(private_pipeline.parse::<Uuid>().unwrap()).execute(&pool).await.unwrap();
    let failed = fixture_run(&pool, &admin, &pipeline).await;
    sqlx::query("UPDATE runs SET status='failed',finished_at=now() WHERE id=$1")
        .bind(failed)
        .execute(&pool)
        .await
        .unwrap();
    let jobs: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM job_runs WHERE run_id=$1 ORDER BY position")
            .bind(failed)
            .fetch_all(&pool)
            .await
            .unwrap();
    let agent = new_agent(&app, &admin, "diagnostic-worker").await;
    let attempt = Uuid::new_v4();
    sqlx::query("INSERT INTO job_attempts(id,job_id,agent_id,claim_request_id,status,lease_expires_at,finished_at,exit_code,reason) VALUES($1,$2,$3,$4,'failed',now(),now(),99,'SUPERVISOR REASON')")
        .bind(attempt).bind(jobs[0]).bind(agent["agent"]["id"].as_str().unwrap().parse::<Uuid>().unwrap()).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE job_runs SET status='succeeded',workspace_attempt_id=$2 WHERE id=$1")
        .bind(jobs[0])
        .bind(attempt)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE job_runs SET status='lost',workspace_attempt_id=$2,step_reason='actual step failure',step_exit_code=42 WHERE id=$1").bind(jobs[1]).bind(attempt).execute(&pool).await.unwrap();
    let old = fixture_run(&pool, &admin, &pipeline).await;
    sqlx::query(
        "UPDATE runs SET status='failed',finished_at=now()-interval '25 hours' WHERE id=$1",
    )
    .bind(old)
    .execute(&pool)
    .await
    .unwrap();
    // The future current version must not replace the recorded failed run's version.
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo new version")).await;
    let endpoint = "/api/workspace/overview";
    let report = ok(&app, &viewer, "GET", endpoint, Value::Null).await;
    let d = &report["diagnostics"];
    assert_eq!(d["actions"]["total"], 1);
    assert_eq!(d["actions"]["items"][0]["key"], blocked.to_string());
    assert_eq!(d["actions"]["items"][0]["run_id"], parent.to_string());
    assert_eq!(d["actions"]["items"][0]["target_name"], "consumer");
    assert_eq!(d["failed_24h"]["total"], 1);
    assert_eq!(d["failed_24h"]["items"][0]["run_id"], failed.to_string());
    assert_eq!(d["failed_24h"]["items"][0]["version"], 1);
    assert_eq!(d["failed_24h"]["items"][0]["job_id"], jobs[1].to_string());
    assert_eq!(d["failed_24h"]["items"][0]["reason"], "actual step failure");
    assert_eq!(d["failed_24h"]["items"][0]["exit_code"], 42);
    assert_eq!(d["failed_24h"]["items"][0]["job_status"], "lost");
    assert!(!d.to_string().contains("SUPERVISOR"));
    assert!(!d.to_string().contains("private-diagnostics"));
    assert!(!d.to_string().contains("PRIVATE REASON"));
    assert!(!d.to_string().contains("agent_id"));
    assert!(!d.to_string().contains("definition"));
    assert_eq!(
        request(&app, None, "GET", endpoint, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    for group in ["actions", "queued", "failed_24h"] {
        assert_eq!(
            ok(&app, &outsider, "GET", endpoint, Value::Null).await["diagnostics"][group]["total"],
            0
        );
    }
    // Over the display limit, counts stay exact and registration ties sort by UUID.
    let ids: Vec<Uuid> = (0..11).map(|_| Uuid::new_v4()).collect();
    sqlx::query("INSERT INTO runs(id,pipeline_id,version,submitted_by,request_id,request_hash,definition,source_snapshot,input_hash,created_at) SELECT u.id,r.pipeline_id,r.version,r.submitted_by,u.id,'fixture',r.definition,r.source_snapshot,r.input_hash,now()-interval '1 hour' FROM runs r CROSS JOIN unnest($2::uuid[]) AS u(id) WHERE r.id=$1")
        .bind(parent).bind(&ids).execute(&pool).await.unwrap();
    let report = ok(&app, &viewer, "GET", endpoint, Value::Null).await;
    assert_eq!(report["diagnostics"]["queued"]["total"], 11);
    assert_eq!(
        report["diagnostics"]["queued"]["items"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let mut ordered = ids.clone();
    ordered.sort();
    assert_eq!(
        report["diagnostics"]["queued"]["items"][0]["run_id"],
        ordered[0].to_string()
    );
    sqlx::query("UPDATE runs SET status='failed',finished_at=now() WHERE id=ANY($1)")
        .bind(&ids)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE run_followups SET status='created',child_run_id=$2,reason='' WHERE id=$1")
        .bind(blocked)
        .bind(same_cl)
        .execute(&pool)
        .await
        .unwrap();
    let before: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM runs),(SELECT count(*) FROM job_attempts),(SELECT count(*) FROM audit_events)").fetch_one(&pool).await.unwrap();
    let report = ok(&app, &viewer, "GET", endpoint, Value::Null).await;
    assert_eq!(report["diagnostics"]["queued"]["total"], 0);
    assert_eq!(report["diagnostics"]["actions"]["total"], 0);
    assert_eq!(report["diagnostics"]["failed_24h"]["total"], 12);
    assert_eq!(
        report["diagnostics"]["failed_24h"]["items"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    let first = &report["diagnostics"]["failed_24h"]["items"][0];
    assert_eq!(first["run_id"], ordered.last().unwrap().to_string());
    assert!(
        first["job_id"].is_null(),
        "no recorded job must not invent a cause"
    );
    let after: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM runs),(SELECT count(*) FROM job_attempts),(SELECT count(*) FROM audit_events)").fetch_one(&pool).await.unwrap();
    assert_eq!(before, after, "overview is read-only");
    sqlx::query("DELETE FROM project_members WHERE project_id=$1 AND user_id=$2")
        .bind(project_id)
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    let revoked = ok(&app, &viewer, "GET", endpoint, Value::Null).await;
    for group in ["actions", "queued", "failed_24h"] {
        assert_eq!(revoked["diagnostics"][group]["total"], 0);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn version_review_scopes_snapshots_and_preserves_publish_conflicts(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let viewer = login(&pool, false).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let base = format!("/api/pipelines/{pipeline}");
    let empty = ok(&app, &admin, "GET", &format!("{base}/review"), Value::Null).await;
    assert_eq!(empty["can_publish"], false);
    assert_eq!(empty["before"]["version"], 0);
    assert!(empty["after"]["definition"].is_null());
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo one")).await;
    let run = fixture_run(&pool, &admin, &pipeline).await;
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo two")).await;
    let draft = ok(&app, &admin, "GET", &base, Value::Null).await["draft"].clone();
    ok(&app,&admin,"PUT",&format!("{base}/draft"),json!({"generation":draft["generation"],"base_version":draft["base_version"],"toml":toml(&root,"echo three")})).await;
    let review = ok(
        &app,
        &admin,
        "GET",
        &format!("{base}/review?from=1&to=draft"),
        Value::Null,
    )
    .await;
    assert_eq!(
        review["before"]["definition"]["jobs"][0]["script"],
        json!(["echo one"])
    );
    assert_eq!(
        review["after"]["definition"]["jobs"][0]["script"],
        json!(["echo three"])
    );
    assert_eq!(review["can_publish"], true);
    let versions = ok(
        &app,
        &admin,
        "GET",
        &format!("{base}/review?from=1&to=2"),
        Value::Null,
    )
    .await;
    assert_eq!(versions["can_publish"], false);
    assert_eq!(
        versions["after"]["definition"]["jobs"][0]["script"],
        json!(["echo two"])
    );
    let old: Value = sqlx::query_scalar("SELECT definition FROM runs WHERE id=$1")
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(old["jobs"][0]["script"], json!(["echo one"]));
    // The review does not grant publish authority or bypass a subsequent draft edit.
    let input = json!({"generation":review["after"]["generation"],"base_version":review["after"]["base_version"]});
    ok(&app,&admin,"PUT",&format!("{base}/draft"),json!({"generation":input["generation"],"base_version":input["base_version"],"toml":toml(&root,"echo four")})).await;
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "POST",
            &format!("{base}/publish"),
            input
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            &format!("{base}/review"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'viewer')")
        .bind(project.parse::<Uuid>().unwrap())
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        ok(&app, &viewer, "GET", &format!("{base}/review"), Value::Null).await["can_publish"],
        false
    );
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            &format!("{base}/preflight?version=1&changelist=2"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("DELETE FROM project_members WHERE user_id=$1")
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            Some(&viewer),
            "GET",
            &format!("{base}/review"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for query in ["from=-1", "to=0", "to=bad", "unknown=1"] {
        assert_eq!(
            request(
                &app,
                Some(&admin),
                "GET",
                &format!("{base}/review?{query}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "GET",
            &format!("{base}/review?to=99"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn preflight_observes_global_capabilities_without_granting_a_claim(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (_, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let definition = toml(&root, "echo test").replace(
        "runner = { os = \"linux\" }",
        "runner = { os = \"linux\", docker = \"linux\" }",
    );
    publish_definition(&app, &admin, &pipeline, &definition).await;
    let agent = new_agent(&app, &admin, "global-preview-agent").await;
    let id: Uuid = agent["agent"]["id"].as_str().unwrap().parse().unwrap();
    let path = format!("/api/pipelines/{pipeline}/preflight?version=1&changelist=2");
    let reset = "UPDATE agents SET active=true,draining=false,os='linux',arch='x86_64',pool='default',perforce_managed=true,last_seen_at=now(),last_protocol_version=8,protocol_reported_at=now(),docker_state='available',docker_engine_os='linux',docker_checked_at=now() WHERE id=$1";
    for (change, active, available, blocker) in [
        ("draining=false", 1, 1, None),
        ("active=false", 0, 0, None),
        ("os='windows'", 0, 0, None),
        ("arch='aarch64'", 0, 0, None),
        ("pool='other'", 0, 0, None),
        (
            "last_seen_at=now()-interval '31 seconds'",
            1,
            0,
            Some("offline"),
        ),
        ("docker_engine_os='windows'", 1, 0, Some("docker_engine")),
        (
            "docker_checked_at=now()-interval '151 seconds'",
            1,
            0,
            Some("docker_stale"),
        ),
        ("last_protocol_version=7", 1, 0, Some("protocol_old")),
    ] {
        sqlx::query(reset).bind(id).execute(&pool).await.unwrap();
        sqlx::query(&format!("UPDATE agents SET {change} WHERE id=$1"))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let report = ok(&app, &admin, "GET", &path, Value::Null).await;
        assert_eq!(report["can_queue"], true); // No capacity is a warning; queue can wait.
        assert_eq!(report["summary"]["jobs"][0]["active"], active);
        assert_eq!(report["summary"]["jobs"][0]["available"], available);
        if let Some(reason) = blocker {
            assert_eq!(report["summary"]["jobs"][0]["blockers"][reason], 1);
        }
        assert!(!report.to_string().contains("global-preview-agent"));
        assert!(!report.to_string().contains(&id.to_string()));
    }
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM runs),(SELECT count(*) FROM job_attempts),(SELECT count(*) FROM run_artifact_inputs)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    for query in [
        "version=0&changelist=2",
        "version=1&changelist=-1",
        "version=1&changelist=2&unknown=1",
    ] {
        assert_eq!(
            request(
                &app,
                Some(&admin),
                "GET",
                &format!("/api/pipelines/{pipeline}/preflight?{query}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn preflight_checks_artifact_source_scope_changelist_and_retention_without_pinning(
    pool: PgPool,
) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    publish_definition(&app, &admin, &pipeline, &toml(&root, "echo output")).await;
    let agent = new_agent(&app, &admin, "artifact-preview").await;
    let agent: Uuid = agent["agent"]["id"].as_str().unwrap().parse().unwrap();
    let disk = tempfile::tempdir().unwrap();
    let (source, artifact) =
        retention_artifact(&pool, &admin, &pipeline, agent, 1, "succeeded", disk.path()).await;
    let consumer = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"consumer"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo input")).unwrap();
    definition.workspace = p4forge_domain::Workspace::Pipeline;
    definition.max_parallel_jobs = 1;
    definition.stages = vec!["build".into(), "test".into()];
    definition.jobs[0].stage = "build".into();
    definition.jobs[1].stage = "test".into();
    definition.artifact_inputs = vec!["output.bin".into()];
    publish_definition(&app, &admin, &consumer, &definition.to_toml().unwrap()).await;
    let prefix = format!("/api/pipelines/{consumer}/preflight?version=1&changelist=2");
    assert_eq!(
        ok(&app, &admin, "GET", &prefix, Value::Null).await["can_queue"],
        false
    );
    let path = format!("{prefix}&source_run_id={source}");
    let report = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(report["can_queue"], true);
    assert_eq!(report["artifacts"][0]["id"], artifact.to_string());
    assert_eq!(report["artifacts"][0]["size"], 7);
    let wrong = ok(
        &app,
        &admin,
        "GET",
        &path.replace("changelist=2", "changelist=3"),
        Value::Null,
    )
    .await;
    assert_eq!(wrong["can_queue"], false);
    assert!(
        wrong["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "source_changelist")
    );
    sqlx::query("UPDATE artifacts SET purge_requested_at=now(),purge_reason='expired',purge_generation=1 WHERE id=$1")
        .bind(artifact)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["can_queue"],
        false
    );
    sqlx::query("UPDATE artifacts SET purge_requested_at=NULL,purge_reason=NULL,purge_generation=NULL WHERE id=$1")
        .bind(artifact)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE runs SET cancel_requested=true WHERE id=$1")
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        ok(&app, &admin, "GET", &path, Value::Null).await["can_queue"],
        false
    );
    let (_, other) = workspace_pipeline(&app, &admin, "outside-review").await;
    let foreign = fixture_run(&pool, &admin, &other).await;
    assert_eq!(
        request(
            &app,
            Some(&admin),
            "GET",
            &format!("{prefix}&source_run_id={foreign}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let unexpected = ok(
        &app,
        &admin,
        "GET",
        &format!(
            "/api/pipelines/{pipeline}/preflight?version=1&changelist=2&source_run_id={source}"
        ),
        Value::Null,
    )
    .await;
    assert_eq!(unexpected["can_queue"], false);
    let pins: i64 = sqlx::query_scalar("SELECT count(*) FROM run_artifact_inputs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pins, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn review_summarizes_pinned_followups_and_restricts_deployment_executor(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let child = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"pinned-child"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let env = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/environments"),
        json!({"name":"review-env","pipeline_id":child}),
    )
    .await;
    let env_id: Uuid = env["id"].as_str().unwrap().parse().unwrap();
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo deploy")).unwrap();
    definition.workspace = p4forge_domain::Workspace::Pipeline;
    definition.max_parallel_jobs = 1;
    definition.stages = vec!["deploy".into(), "health".into()];
    definition.jobs[0].stage = "deploy".into();
    definition.jobs[1].stage = "health".into();
    definition.artifact_inputs = vec!["release.zip".into()];
    definition.deployment = Some(p4forge_domain::Deployment {
        environment_id: env_id,
        health_check: "test".into(),
    });
    publish_definition(&app, &admin, &child, &definition.to_toml().unwrap()).await;
    let mut parent = p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo build")).unwrap();
    parent.workspace = p4forge_domain::Workspace::Pipeline;
    parent.max_parallel_jobs = 1;
    parent.stages = vec!["build".into(), "test".into()];
    parent.jobs[0].stage = "build".into();
    parent.jobs[1].stage = "test".into();
    parent.artifacts = vec![p4forge_domain::ArtifactSpec {
        name: "release.zip".into(),
        path: "release.zip".into(),
    }];
    parent.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: child.parse().unwrap(),
        version: 1,
    }];
    publish_definition(&app, &admin, &pipeline, &parent.to_toml().unwrap()).await;
    definition.jobs[0].script = vec!["echo newer".into()];
    publish_definition(&app, &admin, &child, &definition.to_toml().unwrap()).await;
    let report = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/pipelines/{pipeline}/review?from=0&to=1"),
        Value::Null,
    )
    .await;
    assert_eq!(report["summary"]["followups"][0]["version"], 1);
    assert_eq!(
        report["summary"]["followups"][0]["artifact_inputs"],
        json!(["release.zip"])
    );
    let agent = new_agent(&app, &admin, "executor-preview").await;
    let id: Uuid = agent["agent"]["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("UPDATE agents SET last_seen_at=now(),last_protocol_version=8,protocol_reported_at=now() WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    let path = format!("/api/pipelines/{child}/review?to=1");
    let unbound = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(unbound["summary"]["jobs"][0]["active"], 0);
    assert_eq!(
        unbound["summary"]["deployment"]["executor_configured"],
        false
    );
    sqlx::query("UPDATE deployment_environments SET executor_agent_id=$2 WHERE id=$1")
        .bind(env_id)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let bound = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(bound["summary"]["jobs"][0]["available"], 1);
    let run = fixture_run(&pool, &admin, &child).await;
    sqlx::query("INSERT INTO run_deployments(run_id,environment_id,status) VALUES($1,$2,'recovery_required')").bind(run).bind(env_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE deployment_environments SET active_run_id=$2 WHERE id=$1")
        .bind(env_id)
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    let before: Value =
        sqlx::query_scalar("SELECT to_jsonb(d) FROM deployment_environments d WHERE id=$1")
            .bind(env_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let locked = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert!(
        locked["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "environment_recovery")
    );
    let after: Value =
        sqlx::query_scalar("SELECT to_jsonb(d) FROM deployment_environments d WHERE id=$1")
            .bind(env_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before, after);
}

#[sqlx::test(migrations = "../../migrations")]
async fn visual_editor_preview_preserves_definition_without_writing_and_keeps_draft_conflicts(
    pool: PgPool,
) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let child = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{project}/pipelines"),
        json!({"name":"next"}),
    )
    .await;
    let child = child["id"].as_str().unwrap();
    publish_definition(&app, &admin, child, &toml(&root, "echo followup")).await;
    let mut definition =
        p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo build")).unwrap();
    definition.workspace = p4forge_domain::Workspace::Pipeline;
    definition.max_parallel_jobs = 1;
    definition.stages = vec!["build".into(), "test".into()];
    definition.jobs[0].stage = "build".into();
    definition.jobs[1].stage = "test".into();
    definition.jobs[0].script = vec!["echo first\necho second".into(), "echo third".into()];
    definition.jobs[0]
        .environment
        .insert("BUILD_MODE".into(), "release".into());
    for job in &mut definition.jobs {
        job.runner.docker = Some(p4forge_domain::DockerEngine::Linux);
    }
    definition.artifacts = vec![p4forge_domain::ArtifactSpec {
        name: "release".into(),
        path: "dist/release".into(),
    }];
    definition.publish_generated = Some(p4forge_domain::GeneratedPublication {
        user: "publisher".into(),
        paths: vec!["generated".into()],
    });
    definition.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: child.parse().unwrap(),
        version: 1,
    }];
    publish_definition(&app, &admin, &pipeline, &definition.to_toml().unwrap()).await;
    let path = format!("/api/pipelines/{pipeline}");
    let before = ok(&app, &admin, "GET", &path, Value::Null).await;
    let audit_before: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    definition.jobs[0].timeout_seconds = 600;
    let preview = ok(
        &app,
        &admin,
        "POST",
        &format!("{path}/definition-preview"),
        json!({"definition":definition}),
    )
    .await;
    let roundtrip =
        p4forge_domain::PipelineDefinition::parse(preview["toml"].as_str().unwrap()).unwrap();
    assert_eq!(roundtrip, definition);
    assert_eq!(preview["definition_hash"], definition.hash().unwrap());
    assert_eq!(before, ok(&app, &admin, "GET", &path, Value::Null).await);
    assert_eq!(
        audit_before,
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_events")
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let save = json!({"toml":preview["toml"],"generation":before["draft"]["generation"],"base_version":before["draft"]["base_version"]});
    ok(&app, &admin, "PUT", &format!("{path}/draft"), save.clone()).await;
    assert_eq!(
        request(&app, Some(&admin), "PUT", &format!("{path}/draft"), save)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let after = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(after["current_revision"], before["current_revision"]);
    assert_eq!(after["pipeline"], before["pipeline"]);
    assert_eq!(
        after["draft"]["definition"],
        serde_json::to_value(&definition).unwrap()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn visual_editor_preview_checks_roles_scope_references_and_graph(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let path = format!("/api/pipelines/{pipeline}/definition-preview");
    let definition = p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo build")).unwrap();
    let body = json!({"definition":definition});
    assert_eq!(
        request(&app, None, "POST", &path, body.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    let member = login(&pool, false).await;
    assert_eq!(
        request(&app, Some(&member), "POST", &path, body.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    for role in ["viewer", "operator", "editor"] {
        sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,$3) ON CONFLICT(project_id,user_id) DO UPDATE SET role=EXCLUDED.role")
            .bind(project.parse::<Uuid>().unwrap()).bind(member.id).bind(role).execute(&pool).await.unwrap();
        assert_eq!(
            request(&app, Some(&member), "POST", &path, body.clone())
                .await
                .0,
            if role == "editor" {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }
    let other_project = ok(
        &app,
        &admin,
        "POST",
        "/api/projects",
        json!({"name":"other-project"}),
    )
    .await;
    let roots = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/projects/{project}/roots"),
        Value::Null,
    )
    .await;
    let other_project = other_project["id"].as_str().unwrap();
    let other_root = ok(&app, &admin, "POST", &format!("/api/projects/{other_project}/roots"), json!({"name":"other-root","connection_id":roots[0]["connection_id"],"definition":{"kind":"stream","stream":"//streams/other"}})).await;
    let other_root = other_root["id"].as_str().unwrap().to_owned();
    let other_pipeline = ok(
        &app,
        &admin,
        "POST",
        &format!("/api/projects/{other_project}/pipelines"),
        json!({"name":"other-pipeline"}),
    )
    .await;
    let other_pipeline = other_pipeline["id"].as_str().unwrap().to_owned();
    publish_definition(
        &app,
        &admin,
        &other_pipeline,
        &toml(&other_root, "echo other"),
    )
    .await;
    let mut wrong_root = definition.clone();
    wrong_root.source_root_id = other_root.parse().unwrap();
    let mut unknown = definition.clone();
    unknown.jobs[1].needs = vec!["missing".into()];
    let mut cycle = definition.clone();
    cycle.jobs[0].needs = vec!["test".into()];
    let mut wrong_followup = definition.clone();
    wrong_followup.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: other_pipeline.parse().unwrap(),
        version: 1,
    }];
    let mut missing_version = definition.clone();
    missing_version.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: pipeline.parse().unwrap(),
        version: 99,
    }];
    for invalid in [wrong_root, unknown, cycle, wrong_followup, missing_version] {
        let (status, value) = request(
            &app,
            Some(&member),
            "POST",
            &path,
            json!({"definition":invalid}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    }
    let mut unknown_field = body.clone();
    unknown_field["definition"]["unsupported_future_field"] = json!(true);
    assert_eq!(
        request(&app, Some(&member), "POST", &path, unknown_field)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    sqlx::query("DELETE FROM project_members WHERE project_id=$1 AND user_id=$2")
        .bind(project.parse::<Uuid>().unwrap())
        .bind(member.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, Some(&member), "POST", &path, body).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn editor_diagnostics_locate_errors_and_keep_checks_read_only(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let source = toml(&root, "echo preserved");
    publish_definition(&app, &admin, &pipeline, &source).await;
    let detail = ok(
        &app,
        &admin,
        "GET",
        &format!("/api/pipelines/{pipeline}"),
        Value::Null,
    )
    .await;
    let draft = &detail["draft"];
    let path = format!("/api/pipelines/{pipeline}/diagnostics");
    let body = |definition: Value| json!({"definition":definition,"generation":draft["generation"],"base_version":draft["base_version"]});
    let original = draft["definition"].clone();
    let viewer = login(&pool, false).await;
    sqlx::query("INSERT INTO project_members(project_id,user_id,role) VALUES($1,$2,'viewer')")
        .bind(project.parse::<Uuid>().unwrap())
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    let report = ok(&app, &viewer, "GET", &path, Value::Null).await;
    assert_eq!(report["source"], "saved");
    assert_eq!(report["valid"], true);
    assert!(
        report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["category"] == "availability" && i["job_index"] == 0)
    );
    assert_eq!(
        request(&app, Some(&viewer), "POST", &path, body(original.clone()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, None, "GET", &path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let before:Value=sqlx::query_scalar("SELECT json_build_object('drafts',(SELECT md5(string_agg(to_jsonb(d)::text,',' ORDER BY pipeline_id)) FROM pipeline_drafts d),'runs',(SELECT count(*) FROM runs),'audit',(SELECT count(*) FROM audit_events),'versions',(SELECT count(*) FROM pipeline_revisions))").fetch_one(&pool).await.unwrap();
    let mut invalid = original.clone();
    invalid["jobs"][1]["timeout_seconds"] = json!(0);
    let report = ok(&app, &admin, "POST", &path, body(invalid)).await;
    assert_eq!(report["valid"], false);
    assert_eq!(report["issues"][0]["field"], "timeout_seconds");
    assert_eq!(report["issues"][0]["job_index"], 1);
    assert!(report["graph"].is_null());
    assert!(
        report["toml"]
            .as_str()
            .unwrap()
            .contains("timeout_seconds = 0")
    );
    assert_eq!(request(&app,Some(&admin),"PUT",&format!("/api/pipelines/{pipeline}/draft"),json!({"generation":draft["generation"],"base_version":draft["base_version"],"toml":report["toml"]})).await.0,StatusCode::BAD_REQUEST);
    let malformed = "# 비밀😀 PRIVATE_COMMENT\nschema_version = [";
    let syntax=ok(&app,&admin,"POST",&path,json!({"toml":malformed,"generation":draft["generation"],"base_version":draft["base_version"]})).await;
    assert_eq!(syntax["issues"][0]["category"], "syntax");
    assert_eq!(syntax["issues"][0]["line"], 2);
    assert_eq!(
        syntax["issues"][0]["offset"].as_u64().unwrap(),
        malformed
            .split_inclusive('\n')
            .next()
            .unwrap()
            .encode_utf16()
            .count() as u64
            + syntax["issues"][0]["column"].as_u64().unwrap()
            - 1
    );
    assert!(!syntax.to_string().contains("PRIVATE_COMMENT"));
    let oversized=ok(&app,&admin,"POST",&path,json!({"toml":"x".repeat(p4forge_domain::MAX_CONFIG_BYTES+1),"generation":draft["generation"],"base_version":draft["base_version"]})).await;
    assert_eq!(oversized["valid"], false);
    assert_eq!(oversized["issues"][0]["code"], "size");
    assert!(oversized["definition"].is_null());
    let mut wrong_shape = original.clone();
    wrong_shape["unsupported_future_field"] = json!(true);
    let shape = ok(&app, &admin, "POST", &path, body(wrong_shape)).await;
    assert_eq!(shape["issues"][0]["code"], "shape");
    assert!(shape["definition"].is_null());
    let mut stale = body(original.clone());
    stale["generation"] = json!(0);
    stale["base_version"] = json!(0);
    let stale = ok(&app, &admin, "POST", &path, stale).await;
    assert_eq!(stale["valid"], false);
    assert!(
        stale["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "draft_changed")
    );
    assert!(
        stale["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "published_changed")
    );
    let mut both = body(original);
    both["toml"] = json!(source);
    assert_eq!(
        request(&app, Some(&admin), "POST", &path, both).await.0,
        StatusCode::BAD_REQUEST
    );
    let after:Value=sqlx::query_scalar("SELECT json_build_object('drafts',(SELECT md5(string_agg(to_jsonb(d)::text,',' ORDER BY pipeline_id)) FROM pipeline_drafts d),'runs',(SELECT count(*) FROM runs),'audit',(SELECT count(*) FROM audit_events),'versions',(SELECT count(*) FROM pipeline_revisions))").fetch_one(&pool).await.unwrap();
    assert_eq!(before, after);
    sqlx::query("DELETE FROM project_members WHERE user_id=$1")
        .bind(viewer.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, Some(&viewer), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn editor_diagnostics_check_transitive_followups_scope_and_empty_drafts(pool: PgPool) {
    let app = app(&pool, Perforce::new("/unused"));
    let admin = login(&pool, true).await;
    let (project, root, pipeline) = setup(&app, &admin, "127.0.0.1:16660").await;
    let path = format!("/api/pipelines/{pipeline}/diagnostics");
    let empty = ok(&app, &admin, "GET", &path, Value::Null).await;
    assert_eq!(empty["valid"], false);
    assert!(
        empty["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "empty_draft")
    );
    let mut ids = Vec::new();
    for name in ["leaf", "left", "right"] {
        ids.push(
            ok(
                &app,
                &admin,
                "POST",
                &format!("/api/projects/{project}/pipelines"),
                json!({"name":name}),
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let base = p4forge_domain::PipelineDefinition::parse(&toml(&root, "echo build")).unwrap();
    publish_definition(&app, &admin, &ids[0], &base.to_toml().unwrap()).await;
    let mut branch = base.clone();
    branch.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: ids[0].parse().unwrap(),
        version: 1,
    }];
    for id in &ids[1..] {
        publish_definition(&app, &admin, id, &branch.to_toml().unwrap()).await;
    }
    let mut diamond = base.clone();
    diamond.on_success = ids[1..]
        .iter()
        .map(|id| p4forge_domain::PipelineTarget {
            pipeline_id: id.parse().unwrap(),
            version: 1,
        })
        .collect();
    let body = |d| json!({"definition":d,"generation":empty["generation"],"base_version":empty["base_version"]});
    let result = ok(&app, &admin, "POST", &path, body(json!(diamond))).await;
    assert_eq!(result["valid"], false);
    assert!(
        result["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "followup_validation"
                && i["message"].as_str().unwrap().contains("tree"))
    );
    let (_, foreign) = workspace_pipeline(&app, &admin, "private-diagnostics-project").await;
    let mut foreign_ref = base.clone();
    foreign_ref.source_root_id = Uuid::new_v4();
    foreign_ref.on_success = vec![p4forge_domain::PipelineTarget {
        pipeline_id: foreign.parse().unwrap(),
        version: 1,
    }];
    let result = ok(&app, &admin, "POST", &path, body(json!(foreign_ref))).await;
    assert_eq!(result["valid"], false);
    assert!(
        result["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "source_root")
    );
    assert!(
        result["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "followup_missing")
    );
    assert!(
        !result["issues"]
            .to_string()
            .contains("private-diagnostics-project")
    );
    let outsider = login(&pool, false).await;
    assert_eq!(
        request(&app, Some(&outsider), "GET", &path, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
