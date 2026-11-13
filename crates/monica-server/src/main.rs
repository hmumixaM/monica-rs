mod ai;
mod auth;
mod cache;
mod cdc;
mod collector;
mod connections;
mod faces;
mod imports;
mod linking;
mod media;
mod messages;
mod pages;
mod profiles;
mod runtime;
mod timeline;
mod web;
use anyhow::Result;
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use clap::{Parser, Subcommand};
use monica_core::{
    resources::DatabasePools,
    scheduler::{Scheduler, SchedulerConfig},
};
use serde_json::json;
use sqlx::PgPool;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "Monica Rust isolated development runtime")]
struct Args {
    #[arg(long, env = "MONICA_DATABASE_URL", hide_env_values = true)]
    database_url: String,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Migrate,
    /// Explicitly initialize optional WAL audit (requires wal_level=logical).
    CdcInitialize,
    Serve {
        #[arg(long, default_value = "127.0.0.1:17494")]
        listen: SocketAddr,
        #[arg(long, env = "MONICA_SESSION_SECRET", hide_env_values = true)]
        session_secret: String,
        #[arg(long, env = "MONICA_ORIGIN")]
        origin: Option<String>,
        #[arg(
            long,
            env = "MONICA_MEDIA_ROOT",
            default_value = ".local-validation/media"
        )]
        media_root: PathBuf,
        #[arg(long, env = "MONICA_WEB_ROOT")]
        web_root: Option<PathBuf>,
        #[arg(long, env = "MONICA_IMPORT_HMAC_SECRET", hide_env_values = true)]
        import_hmac_secret: Option<String>,
        #[arg(long, env = "MONICA_CONTROL_TOKEN", hide_env_values = true)]
        control_token: Option<String>,
        #[arg(long, env = "MONICA_START_PAUSED", default_value_t = false)]
        start_paused: bool,
    },
    Ingest {
        file: PathBuf,
    },
    AppendMessages {
        provider: String,
        file: PathBuf,
    },
    Drain,
}
#[derive(Clone)]
struct Readiness {
    pool: PgPool,
    runtime: monica_core::scheduler::SchedulerHandle,
}
async fn ready(State(state): State<Readiness>) -> axum::response::Response {
    if state.runtime.is_stopped() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Background runtime stopped"})),
        )
            .into_response();
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sqlx::query("SELECT 1 FROM crm.jobs LIMIT 0").execute(&state.pool),
    )
    .await
    {
        Ok(Ok(_)) => Json(json!({"status":"ready"})).into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Database is not ready"})),
        )
            .into_response(),
    }
}
fn main() -> Result<()> {
    // Decoders have their own process limits and do not need runtime workers.
    // Starting those workers first races Linux's address-space limit with their
    // stack allocation and can reject an otherwise valid upload.
    if monica_core::media::probe_entry()? {
        return Ok(());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let pools = DatabasePools::connect(&args.database_url, Default::default()).await?;
    let pool = pools.interactive.clone();
    match args.command {
        Command::Migrate => monica_core::migrate(&pool).await?,
        Command::CdcInitialize => monica_core::cdc::initialize(&pool).await?,
        Command::Ingest { file } => {
            let records: Vec<monica_core::Record> =
                serde_json::from_slice(&tokio::fs::read(file).await?)?;
            for record in records {
                monica_core::ingest(&pool, &record).await?;
            }
        }
        Command::Drain => {
            while let Some(lease) = monica_core::claim(&pool).await? {
                monica_core::project(&pool, &lease).await?;
            }
            while let Some(lease) = monica_core::messages::claim(&pool).await? {
                let snapshot = monica_core::messages::snapshot(&pool, &lease).await?;
                monica_core::messages::finish(&pool, &lease, &snapshot).await?;
            }
        }
        Command::AppendMessages { provider, file } => {
            let provider = serde_json::from_value(serde_json::Value::String(provider))?;
            append_file(&pool, provider, file).await?;
        }
        Command::Serve {
            listen,
            session_secret,
            origin,
            media_root,
            web_root,
            import_hmac_secret,
            control_token,
            start_paused,
        } => {
            let mut state = auth::AppState::new(
                pool.clone(),
                &session_secret,
                origin.unwrap_or_else(|| format!("http://{listen}")),
                media_root,
            )
            .await?;
            state.import_hmac_secret = import_hmac_secret.filter(|secret| !secret.is_empty());
            if let Some(secret) = &state.import_hmac_secret {
                anyhow::ensure!(
                    secret.len() >= 32,
                    "MONICA_IMPORT_HMAC_SECRET must contain at least 32 bytes"
                );
            }
            monica_core::ai::configure_from_env(&pool).await?;
            monica_core::adapter::configure_from_env(&pool).await?;
            let resources = state.resources.clone();
            let runtime = Scheduler::start_with_sources(
                pools.background.clone(),
                pools.control.clone(),
                resources.clone(),
                SchedulerConfig {
                    start_paused,
                    ..SchedulerConfig::default()
                },
                state.media_root.clone(),
                state.credentials.clone(),
            )
            .await?;
            let mut app = Router::new()
                .route(
                    "/health",
                    get(|| async { Json(json!({"status":"ok","service":"api"})) }),
                )
                .route(
                    "/api/v1/health",
                    get(|| async { Json(json!({"status":"ok","service":"api"})) }),
                )
                .route("/api/v1/ready", get(ready))
                .with_state(Readiness {
                    pool: pool.clone(),
                    runtime: runtime.clone(),
                })
                .merge(auth::routes(state.clone()));
            if let Some(token) = control_token.filter(|token| !token.is_empty()) {
                app = app.merge(runtime::routes(runtime.clone(), &token, state.clone())?);
            }
            if let Some(root) = web_root {
                app = app.merge(web::routes(root).await?);
            }
            app = app.layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let resources = resources.clone();
                    async move {
                        let interactive = request.uri().path().starts_with("/api/v1/")
                            && !request.uri().path().starts_with("/api/v1/imports/");
                        let start = std::time::Instant::now();
                        let response = next.run(request).await;
                        if interactive {
                            resources.record_interactive(start.elapsed());
                        }
                        response
                    }
                },
            ));
            let listener = tokio::net::TcpListener::bind(listen).await?;
            tracing::info!(address=%listener.local_addr()?,"server ready; authentication enabled, business API in progress");
            let stopping = runtime.clone();
            let served = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async move {
                    tokio::select! {
                        _ = shutdown_signal() => {},
                        _ = async {
                            while !stopping.is_stopped() {
                                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                            }
                            tracing::error!("background supervisor stopped; exiting for managed recovery");
                        } => {},
                    }
                    if let Err(error) = stopping.shutdown().await { tracing::error!(%error,"background shutdown failed"); }
                })
                .await;
            runtime.shutdown().await?;
            served?;
        }
    }
    pools.close().await;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

async fn append_file(pool: &PgPool, provider: monica_core::Provider, path: PathBuf) -> Result<()> {
    use monica_core::messages::{self, BatchBudget, MAX_BYTES, Message};
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::new(tokio::fs::File::open(path).await?);
    let mut line = Vec::new();
    let mut records: Vec<Message> = Vec::new();
    let mut bytes = 2;
    let mut budget = BatchBudget::default();
    let mut added = 0;
    loop {
        let chunk = reader.fill_buf().await?;
        let eof = chunk.is_empty();
        let n = chunk
            .iter()
            .position(|x| *x == b'\n')
            .map(|x| x + 1)
            .unwrap_or(chunk.len());
        anyhow::ensure!(
            line.len() + n <= MAX_BYTES,
            "message line exceeds byte limit"
        );
        line.extend_from_slice(&chunk[..n]);
        reader.consume(n);
        if !eof && !line.ends_with(b"\n") {
            continue;
        }
        if !line.iter().all(u8::is_ascii_whitespace) {
            let record: Message = serde_json::from_slice(&line)?;
            let size = serde_json::to_vec(&record)?.len() + 1;
            anyhow::ensure!(size + 2 <= MAX_BYTES, "encoded message exceeds byte limit");
            if !records.is_empty() && (records.len() >= budget.records || bytes + size > MAX_BYTES)
            {
                let start = std::time::Instant::now();
                added += messages::append(pool, provider, &records).await?;
                budget.observe(start.elapsed());
                records.clear();
                bytes = 2;
                tokio::task::yield_now().await;
            }
            records.push(record);
            bytes += size;
        }
        line.clear();
        if eof {
            break;
        }
    }
    if !records.is_empty() {
        added += messages::append(pool, provider, &records).await?;
    }
    tracing::info!(inserted = added, "message batches committed");
    Ok(())
}
