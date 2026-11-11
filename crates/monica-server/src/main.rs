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
    },
    Ingest {
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
        }
        Command::Serve { listen } => {
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
                .with_state(pool.clone());
            let listener = tokio::net::TcpListener::bind(listen).await?;
            tracing::info!(address=%listener.local_addr()?,"server ready; business API is not yet implemented");
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
