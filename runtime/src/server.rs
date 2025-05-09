// Updated server.rs
use crate::runtime::{PerformanceMetrics, Runtime};
use anyhow::Result;
use hyper::server::conn::Http;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixListener;

// Request and response types
#[derive(Debug, Deserialize)]
struct InitRequest {
    #[serde(default)]
    module: String, // Base64 encoded Wasm module (optional)

    #[serde(default)]
    module_path: String, // Path to Wasm module file (optional)

    #[serde(default)]
    disable_cache: bool, // Whether to disable module caching
}

#[derive(Debug, Serialize)]
struct InitResponse {
    instance_id: String,
    metrics: PerformanceMetricsResponse,
}

#[derive(Debug, Deserialize)]
struct RunRequest {
    instance_id: String,

    #[serde(default)]
    env: HashMap<String, String>, // Environment variables

    #[serde(default)]
    args: Vec<String>, // Command-line arguments for WASI
}

#[derive(Debug, Serialize)]
struct RunResponse {
    metrics: PerformanceMetricsResponse,
    result: i32,          // Function return value
    memory_usage_kb: u64, // Memory used by this run
}

#[derive(Debug, Deserialize)]
struct ColdStartRequest {
    #[serde(default)]
    module: String, // Base64 encoded Wasm module (optional)

    #[serde(default)]
    module_path: String, // Path to Wasm module file (optional)

    #[serde(default)]
    env: HashMap<String, String>, // Environment variables

    #[serde(default)]
    args: Vec<String>, // Command-line arguments for WASI

    #[serde(default)]
    disable_cache: bool, // Whether to disable module caching
}

#[derive(Debug, Serialize)]
struct ColdStartResponse {
    instance_id: String,
    metrics: PerformanceMetricsResponse,
    result: i32, // Function return value
}

#[derive(Debug, Deserialize)]
struct BenchmarkRequest {
    instance_id: String,
    iterations: usize,

    #[serde(default)]
    env: HashMap<String, String>, // Environment variables

    #[serde(default)]
    args: Vec<String>, // Command-line arguments for WASI
}

#[derive(Debug, Serialize)]
struct BenchmarkResponse {
    operations_per_second: f64,
    average_execution_time_us: f64,
    min_execution_time_us: f64,
    max_execution_time_us: f64,
    p50_execution_time_us: f64,
    p95_execution_time_us: f64,
    p99_execution_time_us: f64,
    memory_usage_kb: u64,
}

#[derive(Debug, Serialize)]
struct MemoryUsageResponse {
    instance_id: String,
    memory_usage_kb: u64,
}

#[derive(Debug, Serialize)]
struct MetricsResponse {
    metrics: Vec<PerformanceMetricsResponse>,
}

#[derive(Debug, Serialize)]
struct PerformanceMetricsResponse {
    module_load_time_us: u64,
    module_compile_time_us: u64,
    instantiation_time_us: u64,
    total_cold_start_time_us: u64,
    warm_start_time_us: u64,
    execution_time_us: u64,
    from_cache: bool,
    memory_usage_kb: u64,
}

impl From<PerformanceMetrics> for PerformanceMetricsResponse {
    fn from(metrics: PerformanceMetrics) -> Self {
        Self {
            module_load_time_us: metrics.module_load_time_us,
            module_compile_time_us: metrics.module_compile_time_us,
            instantiation_time_us: metrics.instantiation_time_us,
            total_cold_start_time_us: metrics.total_cold_start_time_us,
            warm_start_time_us: metrics.warm_start_time_us,
            execution_time_us: metrics.execution_time_us,
            from_cache: metrics.from_cache,
            memory_usage_kb: metrics.memory_usage_kb,
        }
    }
}

// HTTP handler
async fn handle_request(req: Request<Body>, runtime: Arc<Runtime>) -> Result<Response<Body>> {
    match (req.method().as_str(), req.uri().path()) {
        // Initialize a new instance
        ("POST", "/init") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let init_req: InitRequest = serde_json::from_slice(&body_bytes)?;

            // Process based on whether we have a module_path or a module
            let (instance_id, metrics) = if !init_req.module_path.is_empty() {
                // Create instance from file path
                runtime
                    .init_instance_from_file(&init_req.module_path)
                    .await?
            } else if !init_req.module.is_empty() {
                // Decode base64 module and create instance
                let module_bytes = base64::decode(&init_req.module)?;
                runtime.init_instance(&module_bytes).await?
            } else {
                anyhow::bail!("Either module_path or module must be provided");
            };

            // Return the instance ID and metrics
            let response = InitResponse {
                instance_id,
                metrics: metrics.into(),
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Run an instance
        ("POST", "/run") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let run_req: RunRequest = serde_json::from_slice(&body_bytes)?;

            // Run the instance with provided env vars and args
            let (metrics, result) = runtime
                .run_instance(run_req.instance_id.clone(), run_req.env, run_req.args)
                .await?;

            // Create response with metrics and execution result
            let response = RunResponse {
                metrics: metrics.clone().into(),
                result,
                memory_usage_kb: metrics.memory_usage_kb,
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Cold start (initialize + run)
        ("POST", "/coldstart") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let cold_req: ColdStartRequest = serde_json::from_slice(&body_bytes)?;

            // Process based on whether we have a module_path or a module
            let wasm_bytes = if !cold_req.module_path.is_empty() {
                // Read module from file path
                std::fs::read(&cold_req.module_path)?
            } else if !cold_req.module.is_empty() {
                // Decode base64 module
                base64::decode(&cold_req.module)?
            } else {
                anyhow::bail!("Either module_path or module must be provided");
            };

            // Perform cold start (init + run)
            let (instance_id, metrics, result) = runtime
                .cold_start(&wasm_bytes, cold_req.env, cold_req.args)
                .await?;

            // Return response
            let response = ColdStartResponse {
                instance_id,
                metrics: metrics.into(),
                result,
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Benchmark an instance
        ("POST", "/benchmark") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let benchmark_req: BenchmarkRequest = serde_json::from_slice(&body_bytes)?;

            // Run the benchmark
            let (ops_per_second, durations) = runtime
                .benchmark_throughput(
                    benchmark_req.instance_id.clone(),
                    benchmark_req.iterations,
                    benchmark_req.env,
                    benchmark_req.args,
                )
                .await?;

            // Calculate statistics in microseconds
            let mut us_durations: Vec<f64> = durations
                .iter()
                .map(|d| d.as_secs_f64() * 1_000_000.0)
                .collect();
            us_durations.sort_by(|a, b| a.partial_cmp(b).unwrap());

            let total_us: f64 = us_durations.iter().sum();
            let avg_us = total_us / us_durations.len() as f64;
            let min_us = us_durations.first().copied().unwrap_or(0.0);
            let max_us = us_durations.last().copied().unwrap_or(0.0);

            // Percentiles
            let p50_index = (us_durations.len() as f64 * 0.5) as usize;
            let p95_index = (us_durations.len() as f64 * 0.95) as usize;
            let p99_index = (us_durations.len() as f64 * 0.99) as usize;

            let p50_us = us_durations.get(p50_index).copied().unwrap_or(0.0);
            let p95_us = us_durations.get(p95_index).copied().unwrap_or(0.0);
            let p99_us = us_durations.get(p99_index).copied().unwrap_or(0.0);

            // Get memory overhead
            let memory_usage_kb = runtime.get_memory_overhead(&benchmark_req.instance_id).await?;

            // Return benchmark results
            let response = BenchmarkResponse {
                operations_per_second: ops_per_second,
                average_execution_time_us: avg_us,
                min_execution_time_us: min_us,
                max_execution_time_us: max_us,
                p50_execution_time_us: p50_us,
                p95_execution_time_us: p95_us,
                p99_execution_time_us: p99_us,
                memory_usage_kb,
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Get memory usage for an instance
        ("GET", path) if path.starts_with("/memory/") => {
            let instance_id = path.trim_start_matches("/memory/");

            // Get memory overhead
            let memory_usage_kb = runtime.get_memory_overhead(instance_id).await?;

            // Return memory usage
            let response = MemoryUsageResponse {
                instance_id: instance_id.to_string(),
                memory_usage_kb,
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Export metrics to CSV
        ("POST", "/export-metrics") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let export_req: serde_json::Value = serde_json::from_slice(&body_bytes)?;

            let path = export_req
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("metrics.csv");

            // Export metrics to CSV
            // runtime.export_metrics_to_csv(path)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(format!("Metrics exported to {}", path)))?)
        }

        // Get all metrics
        ("GET", "/metrics") => {
            // Add .await here to get the actual Vec<PerformanceMetrics>
            let metrics = runtime.get_metrics().await;
            let metrics_response: Vec<PerformanceMetricsResponse> =
                metrics.into_iter().map(Into::into).collect();
        
            let response = MetricsResponse {
                metrics: metrics_response,
            };
            let response_json = serde_json::to_string(&response)?;
        
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Terminate an instance
        ("DELETE", path) if path.starts_with("/instance/") => {
            let instance_id = path.trim_start_matches("/instance/");

            // Terminate the instance
            runtime.terminate_instance(instance_id).await?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::empty())?)
        }

        // Health check
        ("GET", "/health") => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("OK"))?),

        // 404 Not Found
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))?),
    }
}

// Error handling wrapper
async fn service_handler(
    req: Request<Body>,
    runtime: Arc<Runtime>,
) -> Result<Response<Body>, hyper::Error> {
    match handle_request(req, runtime).await {
        Ok(response) => Ok(response),
        Err(err) => {
            tracing::error!("Error handling request: {}", err);

            let response = Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(format!("Internal Server Error: {}", err)))
                .unwrap();

            Ok(response)
        }
    }
}

// Run HTTP server
pub async fn run_http_server(port: u16, runtime: Arc<Runtime>) -> Result<()> {
    let addr = ([0, 0, 0, 0], port).into();

    let service = make_service_fn(move |_| {
        let runtime = runtime.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req| {
                let runtime = runtime.clone();
                service_handler(req, runtime)
            }))
        }
    });

    let server = Server::bind(&addr).serve(service);

    tracing::info!("HTTP server listening on http://{}", addr);

    server.await?;

    Ok(())
}

// Run Unix socket server
pub async fn run_unix_socket_server<P: AsRef<Path>>(path: P, runtime: Arc<Runtime>) -> Result<()> {
    // Remove socket file if it already exists
    let path_ref = path.as_ref();
    if path_ref.exists() {
        std::fs::remove_file(path_ref)?;
    }

    // Create a Unix socket listener
    let unix_listener = UnixListener::bind(path_ref)?;
    tracing::info!("Unix socket server listening on {:?}", path_ref);

    loop {
        match unix_listener.accept().await {
            Ok((stream, _)) => {
                let runtime = runtime.clone();

                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let runtime = runtime.clone();
                        service_handler(req, runtime)
                    });

                    match Http::new().serve_connection(stream, service).await {
                        Ok(_) => {}
                        Err(err) => tracing::error!("Error serving connection: {}", err),
                    }
                });
            }
            Err(err) => {
                tracing::error!("Error accepting connection: {}", err);
            }
        }
    }
}
