mod auth;
mod media;
mod profiles;
use anyhow::Result;
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use clap::{Parser, Subcommand};
use serde_json::json;
use sqlx::{PgPool, postgres::PgPoolOptions};
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
async fn ready(State(pool): State<PgPool>) -> StatusCode {
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sqlx::query("SELECT 1 FROM crm.jobs LIMIT 0").execute(&pool),
    )
    .await
    {
        Ok(Ok(_)) => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args = Args::parse();
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect(&args.database_url)
        .await?;
    match args.command {
        Command::Migrate => monica_core::migrate(&pool).await?,
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
        } => {
            let state = auth::AppState::new(
                pool.clone(),
                &session_secret,
                origin.unwrap_or_else(|| format!("http://{listen}")),
                media_root,
            )
            .await?;
            let app = Router::new()
                .route(
                    "/health",
                    get(|| async {
                        Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION")}))
                    }),
                )
                .route(
                    "/api/v1/health",
                    get(|| async { Json(json!({"status":"ok"})) }),
                )
                .route("/api/v1/ready", get(ready))
                .with_state(pool.clone())
                .merge(auth::routes(state));
            let listener = tokio::net::TcpListener::bind(listen).await?;
            tracing::info!(address=%listener.local_addr()?,"server ready; authentication enabled, business API in progress");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
    }
    pool.close().await;
    Ok(())
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
