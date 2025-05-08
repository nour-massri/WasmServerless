use anyhow::Result;
use clap::{Command, Parser, ValueEnum}; // Added Command import here
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tracing::{info, Level};

mod hostfuncs;
mod module_cache;
mod runtime;
mod server;

#[derive(Debug, Clone, ValueEnum)]
enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<LogLevel> for Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => Level::TRACE,
            LogLevel::Debug => Level::DEBUG,
            LogLevel::Info => Level::INFO,
            LogLevel::Warn => Level::WARN,
            LogLevel::Error => Level::ERROR,
        }
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Log level to use
    #[arg(long, value_enum, default_value = "info")]
    log_level: LogLevel,

    /// Enable optimization for execution speed (may increase startup time)
    #[arg(long, default_value = "false")]
    optimize: bool,

    /// Enable module caching
    #[arg(long, default_value = "false")]
    cache: bool,

    /// Cache directory path
    #[arg(long, default_value = "")]
    cache_dir: String,

    /// Socket path for Unix socket communication
    #[arg(long, default_value = "/tmp/wasm-serverless.sock")]
    socket_path: String,

    /// HTTP port (if set, HTTP server will be used instead of Unix socket)
    #[arg(long)]
    http_port: Option<u16>,
}
#[tokio::main]
async fn main() -> Result<()> {
    // Parse command line arguments
    let args = Args::parse();

    // Initialize logging with selected level
    tracing_subscriber::fmt()
        .with_max_level(Level::from(args.log_level))
        .init();

    info!("Starting Wasm Serverless Engine");
    info!("Optimization: {}", args.optimize);
    info!("Caching: {}", args.cache);

    // Create or use specified cache directory
    let cache_dir = if args.cache_dir.is_empty() {
        let dir = tempfile::tempdir()?.into_path();
        info!("Using temporary cache directory: {:?}", dir);
        dir
    } else {
        let dir = PathBuf::from(&args.cache_dir);
        std::fs::create_dir_all(&dir)?;
        info!("Using specified cache directory: {:?}", dir);
        dir
    };

    // Initialize wasmtime runtime with parsed arguments
    let rt = runtime::Runtime::new(&cache_dir, args.optimize, args.cache)?;
    let rt = Arc::new(rt);

    // Start server
    let _server_handle = if let Some(port) = args.http_port {
        info!("Starting HTTP server on port {}", port);
        tokio::spawn(server::run_http_server(port, rt.clone()))
    } else {
        info!("Starting Unix socket server at {}", args.socket_path);
        let path = PathBuf::from(&args.socket_path);
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