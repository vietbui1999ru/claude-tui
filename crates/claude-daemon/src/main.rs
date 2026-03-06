mod collector;
mod ipc;
mod storage;

use std::sync::Arc;

use claude_common::errors::StorageError;
use claude_common::protocol::CollectorStatus;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use collector::{Collector, CollectorConfig};
use ipc::IpcServer;
use storage::Storage;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("claude-daemon starting");

    // Resolve paths
    let db_path = claude_common::db_path();
    let socket_path = claude_common::socket_path();

    info!(db = %db_path.display(), socket = %socket_path.display(), "paths resolved");

    // Initialize storage -- fail explicitly if path is not valid UTF-8
    let db_str = db_path.to_str().expect("Database path must be valid UTF-8");
    let storage = match Storage::new(db_str) {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(e) => {
            error!("failed to initialize storage: {e}");
            std::process::exit(1);
        }
    };

    // Shared collector status
    let collector_status = Arc::new(Mutex::new(CollectorStatus::Offline));

    // Cancellation token for graceful shutdown
    let cancel = CancellationToken::new();

    let collector_config = CollectorConfig::default();

    // Spawn collector
    let collector = Collector::new(
        collector_config,
        Arc::clone(&storage),
        Arc::clone(&collector_status),
        cancel.clone(),
    );
    let collector_handle = tokio::spawn(async move {
        if let Err(e) = collector.run().await {
            error!("collector exited with error: {e}");
        }
    });

    // Start IPC server
    let ipc_server = IpcServer::new(
        socket_path.clone(),
        Arc::clone(&storage),
        collector_status,
        cancel.clone(),
    );

    // Handle SIGINT/SIGTERM for graceful shutdown
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        let mut sigint =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .expect("failed to register SIGINT handler");
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");

        tokio::select! {
            _ = sigint.recv() => {
                info!("received SIGINT, shutting down");
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
            }
        }

        cancel_signal.cancel();
    });

    // Run IPC server (blocks until cancelled or error)
    if let Err(e) = ipc_server.run().await {
        error!("IPC server exited with error: {e}");
    }

    // Wait for collector to finish
    let _ = collector_handle.await;

    // Clean up socket file
    if socket_path.exists() {
        std::fs::remove_file(&socket_path).ok();
    }

    // Storage is dropped here, ensuring SQLite WAL is properly checkpointed
    info!("claude-daemon shut down cleanly");
}
