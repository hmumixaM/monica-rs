//! Native supervisor control, deliberately separate from browser session auth.
use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use monica_core::scheduler::SchedulerHandle;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone)]
struct Control {
    runtime: SchedulerHandle,
    token_hash: [u8; 32],
    app: crate::auth::AppState,
    native_root: Option<std::path::PathBuf>,
}

pub fn routes(
    runtime: SchedulerHandle,
    token: &str,
    app: crate::auth::AppState,
) -> anyhow::Result<Router> {
    anyhow::ensure!(
        token.len() >= 32,
        "MONICA_CONTROL_TOKEN must contain at least 32 bytes"
    );
    let state = Control {
        runtime,
        app,
        native_root: std::env::var_os("MONICA_NATIVE_ROOT").map(std::path::PathBuf::from),
        token_hash: Sha256::digest(token.as_bytes()).into(),
    };
    Ok(Router::new()
        .route("/internal/runtime/status", get(status))
        .route("/internal/account", get(account))
        .route("/internal/source-run", post(native_run))
        .route("/internal/account/reset-password", post(reset_account))
        .route("/internal/source-page", post(source_page))
        .route(
            "/internal/source-state",
            post(source_state).get(source_status),
        )
        .route(
            "/internal/adapter-state",
            post(adapter_state).get(adapter_status),
        )
        .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
        .route("/internal/runtime/pause", post(pause))
        .route("/internal/runtime/resume", post(resume))
        .route("/internal/runtime/drain", post(drain))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state))
}

async fn authorize(State(state): State<Control>, request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if request.headers().contains_key("origin") || !bool::from(state.token_hash.ct_eq(&actual)) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail":"native control authorization required"})),
        )
            .into_response();
    }
    next.run(request).await
}

fn failed(error: anyhow::Error) -> Response {
    tracing::error!(%error, "runtime control failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"detail":"runtime control unavailable"})),
    )
        .into_response()
}

async fn status(State(state): State<Control>) -> Response {
    match state.runtime.status().await {
        Ok(status) => Json(status).into_response(),
        Err(error) => failed(error),
    }
}
async fn pause(State(state): State<Control>) -> Response {
    match state.runtime.pause().await {
        Ok(()) => status(State(state)).await,
        Err(error) => failed(error),
    }
}
async fn resume(State(state): State<Control>) -> Response {
    match state.runtime.resume().await {
        Ok(()) => status(State(state)).await,
        Err(error) => failed(error),
    }
}
async fn drain(State(state): State<Control>) -> Response {
    match state.runtime.drain().await {
        Ok(()) => status(State(state)).await,
        Err(error) => failed(error),
    }
}

#[derive(serde::Deserialize)]
struct SourcePage {
    provider: String,
    external_account: String,
    display_name: String,
    stream: String,
    records: Vec<serde_json::Value>,
    cursor: serde_json::Value,
    snapshot_id: Option<uuid::Uuid>,
    #[serde(default)]
    reset: bool,
    #[serde(default)]
    snapshot_complete: bool,
    #[serde(default = "complete_page")]
    page_complete: bool,
}
fn complete_page() -> bool {
    true
}
async fn source_page(State(state): State<Control>, Json(mut page): Json<SourcePage>) -> Response {
    async fn save(state: &Control, page: &mut SourcePage) -> anyhow::Result<serde_json::Value> {
        use monica_core::{Provider, connections, media::MediaStore, source_blobs};
        anyhow::ensure!(
            ["apple_contacts", "apple_messages"].contains(&page.provider.as_str()),
            "unsupported native source"
        );
        anyhow::ensure!(
            page.external_account == "local" && page.records.len() <= 1000,
            "invalid native page"
        );
        let provider = Provider::parse(&page.provider)?;
        let cid = connections::connection_id(provider, &page.external_account);
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,$2,$3,$4,'local',true,'connecting') ON CONFLICT(id) DO NOTHING")
   .bind(cid).bind(&page.provider).bind(&page.external_account).bind(&page.display_name).execute(&state.app.pool).await?;
        let store = MediaStore::new(
            state.app.pool.clone(),
            state.app.media_root.clone(),
            state.app.resources.clone(),
        )?;
        for record in &mut page.records {
            if let Some(files) = record["attachments"].as_array_mut() {
                for file in files {
                    if let Some(hash) = file["local_blob"].as_str().map(str::to_owned) {
                        monica_core::media::object_path(&hash)?;
                        let root = state
                            .native_root
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("native root not configured"))?;
                        let directory =
                            tokio::fs::canonicalize(root.join(&page.provider).join("attachments"))
                                .await?;
                        let input = tokio::fs::canonicalize(directory.join(&hash)).await?;
                        anyhow::ensure!(
                            input.starts_with(&directory),
                            "native attachment escapes root"
                        );
                        let blob =
                            source_blobs::archive_path(&store, &page.provider, cid, &input).await?;
                        anyhow::ensure!(blob.blob == hash, "native attachment digest mismatch");
                        file["blob"] = json!(blob.blob);
                        file["size"] = json!(blob.size);
                        file.as_object_mut().unwrap().remove("local_blob");
                    }
                }
            }
        }
        let mut tx = state.app.pool.begin().await?;
        connections::lock_connection(&mut tx, cid).await?;
        let enabled: bool =
            sqlx::query_scalar("SELECT enabled FROM crm.source_connections WHERE id=$1 FOR UPDATE")
                .bind(cid)
                .fetch_one(&mut *tx)
                .await?;
        anyhow::ensure!(enabled, "native connection paused");
        let snapshot = page
            .snapshot_id
            .map(|snapshot_id| connections::NativeSnapshot {
                snapshot_id,
                reset: page.reset,
                snapshot_complete: page.snapshot_complete,
            });
        let cursor = if page.page_complete {
            page.cursor.clone()
        } else {
            connections::cursor(&mut tx, provider, cid, &page.stream).await?
        };
        let changed = connections::commit_native_page(
            &mut tx,
            provider,
            cid,
            &page.stream,
            &page.records,
            &cursor,
            snapshot.as_ref(),
        )
        .await?;
        sqlx::query("UPDATE crm.source_connections SET last_success_at=now(),state='connected',sync_pending=true,error=NULL WHERE id=$1").bind(cid).execute(&mut *tx).await?;
        tx.commit().await?;
        connections::deliver(&state.app.pool, provider, cid, 1000).await?;
        Ok(json!({"connection_id":cid,"saved":changed,"cursor":page.cursor}))
    }
    match save(&state, &mut page).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => failed(error),
    }
}
#[derive(serde::Deserialize)]
struct SourceState {
    provider: String,
    enabled: bool,
}
async fn source_state(State(state): State<Control>, Json(value): Json<SourceState>) -> Response {
    if !["apple_contacts", "apple_messages"].contains(&value.provider.as_str()) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    async fn update(state: &Control, value: &SourceState) -> anyhow::Result<()> {
        let provider = monica_core::Provider::parse(&value.provider)?;
        let id = monica_core::connections::connection_id(provider, "local");
        let mut tx = state.app.pool.begin().await?;
        monica_core::connections::lock_connection(&mut tx, id).await?;
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled,state) VALUES($1,$2,'local',$3,'local',$4,CASE WHEN $4 THEN 'connecting' ELSE 'paused' END) ON CONFLICT(id) DO UPDATE SET enabled=$4,state=CASE WHEN $4 THEN 'connecting' ELSE 'paused' END,generation=crm.source_connections.generation+1,lease_owner=NULL,lease_token=NULL,lease_until=NULL,next_sync_at=now()")
            .bind(id).bind(&value.provider).bind(if value.provider=="apple_contacts"{"本机 Contacts"}else{"本机 Messages"}).bind(value.enabled).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    match update(&state, &value).await {
        Ok(()) => Json(json!({"enabled":value.enabled})).into_response(),
        Err(error) => failed(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use monica_core::{
        Provider, connections,
        scheduler::{Scheduler, SchedulerConfig},
    };
    use tower::ServiceExt;
    async fn request(
        app: &Router,
        path: &str,
        body: serde_json::Value,
        origin: bool,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header(
                "authorization",
                "Bearer synthetic-control-token-at-least-32bytes",
            )
            .header("content-type", "application/json");
        if origin {
            builder = builder.header("origin", "http://127.0.0.1");
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }
    #[sqlx::test(migrations = "../../migrations")]
    async fn native_page_durable_ack_pause_fence_and_origin_boundary(pool: sqlx::PgPool) {
        let root = tempfile::tempdir().unwrap();
        let state = crate::auth::AppState::new(
            pool.clone(),
            "synthetic-signing-secret-at-least-32bytes",
            "http://127.0.0.1:17494".into(),
            root.path().into(),
        )
        .await
        .unwrap();
        let runtime = Scheduler::start(
            pool.clone(),
            pool.clone(),
            state.resources.clone(),
            SchedulerConfig {
                start_paused: true,
                partitions: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let app = routes(
            runtime.clone(),
            "synthetic-control-token-at-least-32bytes",
            state,
        )
        .unwrap();
        let record = json!({"external_id":"native-message","kind":"message","body":"Synthetic native message","thread_id":"chat","occurred_at":"2026-11-11T00:00:00Z","included":true,"participants":[]});
        let page = json!({"provider":"apple_messages","external_account":"local","display_name":"本机 Messages","stream":"messages","records":[record],"cursor":{"rowid":1},"page_complete":true});
        assert_eq!(
            request(&app, "/internal/source-page", page.clone(), true)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, result) = request(&app, "/internal/source-page", page.clone(), false).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["cursor"], json!({"rowid":1}));
        let (status, result) = request(&app, "/internal/source-page", page.clone(), false).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["saved"], 0);
        let cid = connections::connection_id(Provider::AppleMessages, "local");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM apple_messages.connection_revision_refs WHERE connection_id=$1").bind(cid).fetch_one(&pool).await.unwrap(),1);
        assert_eq!(
            request(
                &app,
                "/internal/source-state",
                json!({"provider":"apple_messages","enabled":false}),
                false
            )
            .await
            .0,
            StatusCode::OK
        );
        let mut next = page.clone();
        next["records"][0]["body"] = json!("must not commit while disabled");
        next["cursor"] = json!({"rowid":2});
        assert_eq!(
            request(&app, "/internal/source-page", next.clone(), false)
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let cursor:serde_json::Value=sqlx::query_scalar("SELECT cursor FROM apple_messages.connection_cursors WHERE connection_id=$1 AND stream='messages'").bind(cid).fetch_one(&pool).await.unwrap();
        assert_eq!(cursor, json!({"rowid":1}));
        assert_eq!(
            request(
                &app,
                "/internal/source-state",
                json!({"provider":"apple_messages","enabled":true}),
                false
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            request(&app, "/internal/source-page", next, false).await.0,
            StatusCode::OK
        );
        runtime.shutdown().await.unwrap();
    }
}

#[derive(serde::Deserialize)]
struct AdapterState {
    provider: String,
    enabled: bool,
    #[serde(default)]
    paused: bool,
}
async fn adapter_state(State(state): State<Control>, Json(value): Json<AdapterState>) -> Response {
    if !["wechat", "instagram", "linkedin", "chatlog"].contains(&value.provider.as_str()) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    match sqlx::query(
        "UPDATE crm.adapter_controls SET enabled=$2,paused=$3,updated_at=now() WHERE provider=$1",
    )
    .bind(&value.provider)
    .bind(value.enabled)
    .bind(value.paused)
    .execute(&state.app.pool)
    .await
    {
        Ok(row) if row.rows_affected() == 1 => {
            Json(json!({"provider":value.provider,"enabled":value.enabled,"paused":value.paused}))
                .into_response()
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => failed(error.into()),
    }
}

async fn adapter_status(State(state): State<Control>) -> Response {
    match sqlx::query_scalar::<_,serde_json::Value>("SELECT jsonb_build_object('provider',provider,'enabled',enabled,'paused',paused) FROM crm.adapter_controls ORDER BY provider").fetch_all(&state.app.pool).await{
  Ok(rows)=>Json(rows).into_response(),Err(error)=>failed(error.into())
 }
}

async fn source_status(State(state): State<Control>) -> Response {
    match sqlx::query_scalar::<_,serde_json::Value>("SELECT jsonb_build_object('provider',provider,'enabled',enabled,'state',state) FROM crm.source_connections WHERE provider IN ('apple_contacts','apple_messages') AND external_account='local' ORDER BY provider").fetch_all(&state.app.pool).await{
  Ok(rows)=>Json(rows).into_response(),Err(error)=>failed(error.into())
 }
}

async fn account(
    State(s): State<Control>,
) -> Result<Json<serde_json::Value>, crate::auth::ApiError> {
    let value: Option<String> = sqlx::query_scalar("SELECT email FROM crm.app_users LIMIT 1")
        .fetch_optional(&s.app.pool)
        .await?;
    Ok(Json(json!({"email":value})))
}
async fn reset_account(
    State(s): State<Control>,
    Json(body): Json<crate::auth::ResetPassword>,
) -> Result<Json<serde_json::Value>, crate::auth::ApiError> {
    crate::auth::reset_password(&s.app, body, None).await
}

#[derive(serde::Deserialize)]
struct NativeRun {
    provider: String,
    run_id: uuid::Uuid,
    state: String,
}
async fn native_run(
    State(s): State<Control>,
    Json(body): Json<NativeRun>,
) -> Result<Json<serde_json::Value>, crate::auth::ApiError> {
    if !["apple_contacts", "apple_messages"].contains(&body.provider.as_str())
        || !["collecting", "ready", "failed"].contains(&body.state.as_str())
    {
        return Err(crate::auth::ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Invalid native source run",
        ));
    }
    let provider = monica_core::Provider::parse(&body.provider).expect("validated provider");
    let cid = monica_core::connections::connection_id(provider, "local");
    let id = format!("native:{}:{}", body.provider, body.run_id);
    let mut tx = s.app.pool.begin().await?;
    sqlx::query("INSERT INTO crm.import_activity(id,provider,connection_id,stream,kind,state,phase,received_at,heartbeat_at,completed_at,failures) VALUES($1,$2,$3,'native','collection',$4,'native_read',now(),now(),CASE WHEN $4<>'collecting' THEN now() END,CASE WHEN $4='failed' THEN 1 ELSE 0 END) ON CONFLICT(id) DO UPDATE SET state=excluded.state,heartbeat_at=now(),updated_at=now(),completed_at=excluded.completed_at,failures=greatest(crm.import_activity.failures,excluded.failures)")
        .bind(&id).bind(&body.provider).bind(cid).bind(&body.state).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO crm.import_run_events(run_id,provider,connection_id,state,phase) VALUES($1,$2,$3,$4,'native_read')").bind(id).bind(body.provider).bind(cid).bind(body.state).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"recorded":true})))
}
