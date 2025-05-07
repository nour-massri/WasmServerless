use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tracing::{info, Level};

mod hostfuncs;
mod module_cache;
mod runtime;
mod server;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    info!("Starting Wasm Serverless Engine");

    // Create cache directory for wasmtime
    let cache_dir = tempfile::tempdir()?.into_path();
    info!("Using cache directory: {:?}", cache_dir);

    // Load config
    let socket_path =
        std::env::var("SOCKET_PATH").unwrap_or_else(|_| "/tmp/wasm-serverless.sock".to_string());

    let http_port = std::env::var("HTTP_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok());

    // Initialize wasmtime runtime
    let rt = runtime::Runtime::new("cache_dir")?;
    let rt = Arc::new(rt);

    // Start server
    let _server_handle = if let Some(port) = http_port {
        info!("Starting HTTP server on port {}", port);
        tokio::spawn(server::run_http_server(port, rt.clone()))
    } else {
        info!("Starting Unix socket server at {}", socket_path);
        let path = PathBuf::from(socket_path);
        // Remove existing socket file if any
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        tokio::spawn(server::run_unix_socket_server(path, rt.clone()))
    };

    // Wait for shutdown signal
    match signal::ctrl_c().await {
        Ok(()) => {
            info!("Shutdown signal received, stopping server");

            // Export metrics before shutdown
            info!("Exporting metrics to metrics.csv");
            rt.export_metrics_to_csv("metrics.csv")?;
        }
        Err(err) => {
            eprintln!("Error listening for shutdown signal: {}", err);
        }
    }

    Ok(())
}
