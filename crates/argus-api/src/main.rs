// Rust guideline compliant 2026-02-21
//! argus-api: lightweight query and control service for Argus supervisors.
//!
//! Connects to a running supervisor's WebSocket, persists events to
//! SQLite, and exposes a query API. Proxies control commands (pause,
//! resume, approvals, restore) to the supervisor.

mod db;
mod ingest;
mod proxy;
mod routes;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{Level, event};
use tracing_subscriber::EnvFilter;

/// argus-api: query and control service for Argus supervisors.
#[derive(Debug, Parser)]
#[command(name = "argus-api", version, about)]
struct Cli {
    /// Supervisor address to connect to.
    #[arg(long, default_value = "127.0.0.1:9090")]
    supervisor: String,

    /// Listen address for the API server.
    #[arg(long, default_value = "127.0.0.1:8000")]
    listen: SocketAddr,

    /// Path to the SQLite database file.
    #[arg(long, default_value = "argus-events.db")]
    db: PathBuf,

    /// Optional JSONL file to append all events to.
    #[arg(long)]
    jsonl: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    event!(
        name: "argus_api.start",
        Level::INFO,
        supervisor = %cli.supervisor,
        listen = %cli.listen,
        db = %cli.db.display(),
        "starting argus-api",
    );

    let store = Arc::new(db::EventStore::open(&cli.db)?);

    let ingest_store = Arc::clone(&store);
    let supervisor_url = format!("ws://{}/ws", cli.supervisor);
    let supervisor_base = format!("http://{}", cli.supervisor);

    // Ingest task: connect to supervisor WebSocket, persist events.
    let ingest_handle = tokio::spawn(async move {
        ingest::run(&supervisor_url, &ingest_store).await;
    });

    // API server.
    let app = routes::router(Arc::clone(&store), supervisor_base);

    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .context("failed to bind API listener")?;

    event!(
        name: "argus_api.listening",
        Level::INFO,
        listen = %cli.listen,
        "API server listening on {{listen}}",
    );

    axum::serve(listener, app)
        .await
        .context("API server failed")?;

    ingest_handle.abort();
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
