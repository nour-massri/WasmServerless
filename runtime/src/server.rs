use crate::runtime::{PerformanceMetrics, PrecompileMetrics, Runtime};
use anyhow::Result;
use hyper::server::conn::Http;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::net::UnixListener;

// Request and response types
#[derive(Debug, Deserialize)]
struct InitRequest {
    wasm_path: String, // Path to .wasm file to precompile
}

#[derive(Debug, Serialize)]
struct InitResponse {
    module_id: String,
    metrics: PrecompileMetricsResponse,
}

#[derive(Debug, Deserialize)]
struct RunRequest {
    module_id: String,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    args: Vec<String>,
    timeout_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
struct RunResponse {
    execution_id: String,
    metrics: PerformanceMetricsResponse,
    result: i32,
}

#[derive(Debug, Deserialize)]
struct BenchmarkRequest {
    module_id: String,
    iterations: usize,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    args: Vec<String>,
    timeout_seconds: Option<u64>,
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
}

#[derive(Debug, Serialize)]
struct RunningExecutionResponse {
    execution_id: String,
    module_id: String,
    duration_seconds: f64,
}

#[derive(Debug, Serialize)]
struct MetricsResponse {
    runtime_metrics: Vec<PerformanceMetricsResponse>,
    precompile_metrics: Vec<PrecompileMetricsResponse>,
}

#[derive(Debug, Serialize)]
struct TerminateResponse {
    success: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct PerformanceMetricsResponse {
    execution_id: String,
    module_load_time_us: u64,
    instantiation_time_us: u64,
    execution_time_us: u64,
    total_run_time_us: u64,
    timed_out: bool,
    cancelled: bool,
}

#[derive(Debug, Serialize)]
struct PrecompileMetricsResponse {
    wasm_load_time_us: u64,
    compilation_time_us: u64,
    save_time_us: u64,
    total_precompile_time_us: u64,
    wasm_size_bytes: u64,
    cwasm_size_bytes: u64,
}

impl From<PerformanceMetrics> for PerformanceMetricsResponse {
    fn from(metrics: PerformanceMetrics) -> Self {
        Self {
            execution_id: metrics.execution_id,
            module_load_time_us: metrics.module_load_time_us,
            instantiation_time_us: metrics.instantiation_time_us,
            execution_time_us: metrics.execution_time_us,
            total_run_time_us: metrics.total_run_time_us,
            timed_out: metrics.timed_out,
            cancelled: metrics.cancelled,
        }
    }
}

impl From<PrecompileMetrics> for PrecompileMetricsResponse {
    fn from(metrics: PrecompileMetrics) -> Self {
        Self {
            wasm_load_time_us: metrics.wasm_load_time_us,
            compilation_time_us: metrics.compilation_time_us,
            save_time_us: metrics.save_time_us,
            total_precompile_time_us: metrics.total_precompile_time_us,
            wasm_size_bytes: metrics.wasm_size_bytes,
            cwasm_size_bytes: metrics.cwasm_size_bytes,
        }
    }
}
#[derive(Debug, Deserialize)]
struct CpuBurstBenchmarkRequest {
    module_id: String,
    num_executions: usize,
    batch_size: usize,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct IoBenchmarkRequest {
    module_id: String,
    num_executions: usize,
    concurrent_limit: usize,
    requests_per_execution: usize,
}

#[derive(Debug, Serialize)]
struct BurstBenchmarkResponse {
    pub total_executions: usize,
    pub successful_executions: usize,
    pub failed_executions: usize,
    pub total_time_ms: u64,
    pub average_execution_time_us: f64,
    pub cold_start_time_us: f64,
    pub average_cold_start_time_us: f64,
    pub scaling_time_ms: u64,
    pub peak_concurrency: usize,
    pub operations_per_second: f64,
    pub p50_latency_us: f64,
    pub p95_latency_us: f64,
    pub p99_latency_us: f64,
}

#[derive(Debug, Serialize)]
struct NetworkBenchmarkResponse {
    pub total_requests: usize,
    pub successful_requests: usize,
    pub failed_requests: usize,
    pub total_time_ms: u64,
    pub average_execution_time_us: f64,
    pub average_connection_setup_time_us: f64,
    pub end_to_end_latency_us: f64,
    pub network_overhead_percentage: f64,
    pub concurrent_connections_peak: usize,
    pub requests_per_second: f64,
    pub p50_latency_us: f64,
    pub p95_latency_us: f64,
    pub p99_latency_us: f64,
}

// HTTP handler
async fn handle_request(req: Request<Body>, runtime: Arc<Runtime>) -> Result<Response<Body>> {
    match (req.method().as_str(), req.uri().path()) {
        // Precompile a WebAssembly module
        ("POST", "/init") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let init_req: InitRequest = serde_json::from_slice(&body_bytes)?;

            // Precompile the module
            let (module_id, metrics) = runtime.precompile_module(&init_req.wasm_path).await?;

            // Return the module ID and precompilation metrics
            let response = InitResponse {
                module_id,
                metrics: metrics.into(),
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Run a precompiled module
        ("POST", "/run") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let run_req: RunRequest = serde_json::from_slice(&body_bytes)?;

            // Run the module with timeout support
            let (metrics, result) = runtime
                .run_module_with_timeout(
                    run_req.module_id.clone(),
                    run_req.env,
                    run_req.args,
                    run_req.timeout_seconds,
                )
                .await?;

            // Create response with metrics and execution result
            let response = RunResponse {
                execution_id: metrics.execution_id.clone(),
                metrics: metrics.into(),
                result,
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // // CPU burst benchmark
        // ("POST", "/benchmark/cpu-burst") => {
        //     let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
        //     let benchmark_req: CpuBurstBenchmarkRequest = serde_json::from_slice(&body_bytes)?;

        //     let metrics = runtime
        //         .benchmark_cpu_burst(
        //             benchmark_req.module_id,
        //             benchmark_req.num_executions,
        //             benchmark_req.batch_size,
        //             benchmark_req.args,
        //         )
        //         .await?;

        //     let response = BurstBenchmarkResponse {
        //         total_executions: metrics.total_executions,
        //         successful_executions: metrics.successful_executions,
        //         failed_executions: metrics.failed_executions,
        //         total_time_ms: metrics.total_time_ms,
        //         average_execution_time_us: metrics.average_execution_time_us,
        //         cold_start_time_us: metrics.cold_start_time_us,
        //         average_cold_start_time_us: metrics.average_cold_start_time_us,
        //         scaling_time_ms: metrics.scaling_time_ms,
        //         peak_concurrency: metrics.peak_concurrency,
        //         operations_per_second: metrics.operations_per_second,
        //         p50_latency_us: metrics.p50_latency_us,
        //         p95_latency_us: metrics.p95_latency_us,
        //         p99_latency_us: metrics.p99_latency_us,
        //     };

        //     let response_json = serde_json::to_string(&response)?;

        //     Ok(Response::builder()
        //         .status(StatusCode::OK)
        //         .header("Content-Type", "application/json")
        //         .body(Body::from(response_json))?)
        // }

        // // I/O heavy benchmark
        // ("POST", "/benchmark/io-heavy") => {
        //     let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
        //     let benchmark_req: IoBenchmarkRequest = serde_json::from_slice(&body_bytes)?;

        //     let metrics = runtime
        //         .benchmark_io_heavy(
        //             benchmark_req.module_id,
        //             benchmark_req.num_executions,
        //             benchmark_req.concurrent_limit,
        //             benchmark_req.requests_per_execution,
        //         )
        //         .await?;

        //     let response = NetworkBenchmarkResponse {
        //         total_requests: metrics.total_requests,
        //         successful_requests: metrics.successful_requests,
        //         failed_requests: metrics.failed_requests,
        //         total_time_ms: metrics.total_time_ms,
        //         average_execution_time_us: metrics.average_execution_time_us,
        //         average_connection_setup_time_us: metrics.average_connection_setup_time_us,
        //         end_to_end_latency_us: metrics.end_to_end_latency_us,
        //         network_overhead_percentage: metrics.network_overhead_percentage,
        //         concurrent_connections_peak: metrics.concurrent_connections_peak,
        //         requests_per_second: metrics.requests_per_second,
        //         p50_latency_us: metrics.p50_latency_us,
        //         p95_latency_us: metrics.p95_latency_us,
        //         p99_latency_us: metrics.p99_latency_us,
        //     };

        //     let response_json = serde_json::to_string(&response)?;

        //     Ok(Response::builder()
        //         .status(StatusCode::OK)
        //         .header("Content-Type", "application/json")
        //         .body(Body::from(response_json))?)
        // }

        // Benchmark a precompiled module
        ("POST", "/benchmark") => {
            let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
            let benchmark_req: BenchmarkRequest = serde_json::from_slice(&body_bytes)?;

            // Run the benchmark
            let (ops_per_second, durations) = runtime
                .benchmark_throughput(
                    benchmark_req.module_id.clone(),
                    benchmark_req.iterations,
                    benchmark_req.env,
                    benchmark_req.args,
                    benchmark_req.timeout_seconds,
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

            // Return benchmark results
            let response = BenchmarkResponse {
                operations_per_second: ops_per_second,
                average_execution_time_us: avg_us,
                min_execution_time_us: min_us,
                max_execution_time_us: max_us,
                p50_execution_time_us: p50_us,
                p95_execution_time_us: p95_us,
                p99_execution_time_us: p99_us,
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Get running executions
        ("GET", "/executions") => {
            let running_executions = runtime.get_running_executions().await;

            let response: Vec<RunningExecutionResponse> = running_executions
                .into_iter()
                .map(|exec| RunningExecutionResponse {
                    execution_id: exec.execution_id,
                    module_id: exec.module_id,
                    duration_seconds: exec.start_time.elapsed().as_secs_f64(),
                })
                .collect();

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
                .unwrap_or("metrics");

            // Export both runtime and precompilation metrics to CSV
            runtime.export_metrics_to_csv(path).await?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::from(format!(
                    "Metrics exported to {}.runtime.csv and {}.precompile.csv",
                    path, path
                )))?)
        }

        // Get all metrics
        ("GET", "/metrics") => {
            let runtime_metrics = runtime.get_metrics().await;
            let precompile_metrics = runtime.get_precompile_metrics().await;

            let runtime_response: Vec<PerformanceMetricsResponse> =
                runtime_metrics.into_iter().map(Into::into).collect();
            let precompile_response: Vec<PrecompileMetricsResponse> =
                precompile_metrics.into_iter().map(Into::into).collect();

            let response = MetricsResponse {
                runtime_metrics: runtime_response,
                precompile_metrics: precompile_response,
            };
            let response_json = serde_json::to_string(&response)?;

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(response_json))?)
        }

        // Terminate specific execution
        ("DELETE", path) if path.starts_with("/execution/") => {
            let execution_id = path.trim_start_matches("/execution/");

            match runtime.terminate_execution(execution_id).await {
                Ok(_) => {
                    let response = TerminateResponse {
                        success: true,
                        message: format!("Execution {} terminated", execution_id),
                    };
                    let response_json = serde_json::to_string(&response)?;

                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "application/json")
                        .body(Body::from(response_json))?)
                }
                Err(e) => {
                    let response = TerminateResponse {
                        success: false,
                        message: format!("Failed to terminate execution: {}", e),
                    };
                    let response_json = serde_json::to_string(&response)?;

                    Ok(Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .header("Content-Type", "application/json")
                        .body(Body::from(response_json))?)
                }
            }
        }

        // Terminate all executions for a module
        ("DELETE", path) if path.starts_with("/module/") && path.ends_with("/executions") => {
            let module_id = path
                .trim_start_matches("/module/")
                .trim_end_matches("/executions");

            match runtime.terminate_module_executions(module_id).await {
                Ok(count) => {
                    let response = TerminateResponse {
                        success: true,
                        message: format!(
                            "Terminated {} executions for module {}",
                            count, module_id
                        ),
                    };
                    let response_json = serde_json::to_string(&response)?;

                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "application/json")
                        .body(Body::from(response_json))?)
                }
                Err(e) => {
                    let response = TerminateResponse {
                        success: false,
                        message: format!("Failed to terminate executions: {}", e),
                    };
                    let response_json = serde_json::to_string(&response)?;

                    Ok(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header("Content-Type", "application/json")
                        .body(Body::from(response_json))?)
                }
            }
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
