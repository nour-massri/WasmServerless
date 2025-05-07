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
struct CreateRequest {
    #[serde(default)]
    module: String, // Base64 encoded Wasm module (optional)

    #[serde(default)]
    module_path: String, // Path to Wasm module file (optional)

    #[serde(default)]
    env: HashMap<String, String>, // Environment variables

    #[serde(default)]
    args: Vec<String>, // Command-line arguments for WASI
}

#[derive(Debug, Serialize)]
struct CreateResponse {
    instance_id: String,
    metrics: PerformanceMetricsResponse,
}

#[derive(Debug, Deserialize)]
struct RunRequest {
    instance_id: String,
}

#[derive(Debug, Serialize)]
struct RunResponse {
    metrics: PerformanceMetricsResponse,
    result: i32,  // Added result field to store the function return value
}

#[derive(Debug, Deserialize)]
struct BenchmarkRequest {
    instance_id: String,
    iterations: usize,
}

#[derive(Debug, Serialize)]
struct BenchmarkResponse {
    operations_per_second: f64,
    average_execution_time_ms: f64,
    min_execution_time_ms: f64,
    max_execution_time_ms: f64,
    p50_execution_time_ms: f64,
    p95_execution_time_ms: f64,
    p99_execution_time_ms: f64,
}

#[derive(Debug, Serialize)]
struct MetricsResponse {
    metrics: Vec<PerformanceMetricsResponse>,
}

#[derive(Debug, Serialize)]
struct PerformanceMetricsResponse {
    module_load_time_ms: u128,
    module_compile_time_ms: u128,
    instantiation_time_ms: u128,
    total_cold_start_time_ms: u128,
    warm_start_time_ms: u128,
    execution_time_ms: u128,
    from_cache: bool,
}

impl From<PerformanceMetrics> for PerformanceMetricsResponse {
    fn from(metrics: PerformanceMetrics) -> Self {
        Self {
            module_load_time_ms: metrics.module_load_time.as_millis(),
            module_compile_time_ms: metrics.module_compile_time.as_millis(),
            instantiation_time_ms: metrics.instantiation_time.as_millis(),
            total_cold_start_time_ms: metrics.total_cold_start_time.as_millis(),
            warm_start_time_ms: metrics.warm_start_time.as_millis(),
            execution_time_ms: metrics.execution_time.as_millis(),
            from_cache: metrics.from_cache,
        }
    }
}

// HTTP handler
async fn handle_request(req: Request<Body>, runtime: Arc<Runtime>) -> Result<Response<Body>> {
    match (req.method().as_str(), req.uri().path()) {
        // Create a new instance
        ("POST", "/create") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let create_req: CreateRequest = serde_json::from_slice(&body_bytes)?;

            // Process based on whether we have a module_path or a module
            let (instance_id, metrics) = if !create_req.module_path.is_empty() {
                // Create instance from file path
                runtime
                    .create_instance_from_file(
                        &create_req.module_path,
                        create_req.env,
                        create_req.args,
                    )
                    .await?
            } else if !create_req.module.is_empty() {
                // Decode base64 module and create instance
                let module_bytes = base64::decode(&create_req.module)?;
                runtime
                    .create_instance(&module_bytes, create_req.env, create_req.args)
                    .await?
            } else {
                anyhow::bail!("Either module_path or module must be provided");
            };

            // Return the instance ID and metrics
            let response = CreateResponse {
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

            // Run the instance - now returns both metrics and result
            let (metrics, result) = runtime.run_instance(&run_req.instance_id).await?;

            // Create response with both metrics and execution result
            let response = RunResponse {
                metrics: metrics.into(),
                result: result,
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
                .benchmark_throughput(&benchmark_req.instance_id, benchmark_req.iterations)
                .await?;

            // Calculate statistics
            let mut ms_durations: Vec<f64> =
                durations.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
            ms_durations.sort_by(|a, b| a.partial_cmp(b).unwrap());

            let total_ms: f64 = ms_durations.iter().sum();
            let avg_ms = total_ms / ms_durations.len() as f64;
            let min_ms = ms_durations.first().copied().unwrap_or(0.0);
            let max_ms = ms_durations.last().copied().unwrap_or(0.0);

            // Percentiles
            let p50_index = (ms_durations.len() as f64 * 0.5) as usize;
            let p95_index = (ms_durations.len() as f64 * 0.95) as usize;
            let p99_index = (ms_durations.len() as f64 * 0.99) as usize;

            let p50_ms = ms_durations.get(p50_index).copied().unwrap_or(0.0);
            let p95_ms = ms_durations.get(p95_index).copied().unwrap_or(0.0);
            let p99_ms = ms_durations.get(p99_index).copied().unwrap_or(0.0);

            // Return benchmark results
            let response = BenchmarkResponse {
                operations_per_second: ops_per_second,
                average_execution_time_ms: avg_ms,
                min_execution_time_ms: min_ms,
                max_execution_time_ms: max_ms,
                p50_execution_time_ms: p50_ms,
                p95_execution_time_ms: p95_ms,
                p99_execution_time_ms: p99_ms,
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
            runtime.export_metrics_to_csv(path)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(format!("Metrics exported to {}", path)))?)
        }

        // Get all metrics
        ("GET", "/metrics") => {
            let metrics = runtime.get_metrics();
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
