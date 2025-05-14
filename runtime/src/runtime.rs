use crate::hostfuncs::{HostFunctions, WasmIOContext};
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::time::{timeout, Duration as TokioDuration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wasmtime::*;
use wasmtime_wasi::preview1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

// Global module counter for unique module IDs
static MODULE_COUNTER: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));

// Enhanced performance metrics struct with timing only
#[derive(Debug, Clone, Default)]
pub struct PerformanceMetrics {
    pub execution_id: String,       // Execution ID
    pub module_load_time_us: u64,   // Time to load precompiled module
    pub instantiation_time_us: u64, // Time to instantiate the module
    pub execution_time_us: u64,     // Time for function execution
    pub total_run_time_us: u64,     // Total time from load to execution complete
    pub timed_out: bool,            // Whether execution timed out
    pub cancelled: bool,            // Whether execution was cancelled
}

// Precompilation metrics
#[derive(Debug, Clone, Default)]
pub struct PrecompileMetrics {
    pub wasm_load_time_us: u64,        // Time to load .wasm file
    pub compilation_time_us: u64,      // Time to compile to .cwasm
    pub save_time_us: u64,             // Time to save .cwasm file
    pub total_precompile_time_us: u64, // Total precompilation time
    pub wasm_size_bytes: u64,          // Size of input .wasm file
    pub cwasm_size_bytes: u64,         // Size of output .cwasm file
}

// Running execution tracking
#[derive(Debug, Clone)]
pub struct RunningExecution {
    pub execution_id: String,
    pub module_id: String,
    pub start_time: Instant,
    pub cancellation_token: CancellationToken,
}

// Storage for precompiled modules
pub struct PrecompiledModule {
    pub module_id: String,
    pub cwasm_path: String,
    pub module: Module,
    pub creation_time: Instant,
}

/// Main runtime for wasmtime
pub struct Runtime {
    engine: Engine,
    linker: Linker<WasmIOContext>,
    precompiled_modules: tokio::sync::Mutex<HashMap<String, PrecompiledModule>>,
    running_executions: tokio::sync::Mutex<HashMap<String, RunningExecution>>,
    pub metrics: tokio::sync::Mutex<Vec<PerformanceMetrics>>,
    pub precompile_metrics: tokio::sync::Mutex<Vec<PrecompileMetrics>>,
    cache_dir: String,
    default_timeout_seconds: u64,
}

impl Runtime {
    /// Create a new runtime with timeout support
    pub fn new<P: AsRef<Path>>(
        cache_dir: P,
        optimize: bool,
        cache: bool,
        default_timeout_seconds: Option<u64>,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_backtrace(true);

        if !optimize {
            // Faster instantiation for development
            config.strategy(Strategy::Winch);
        } else {
            // Optimize for execution speed
            config.strategy(Strategy::Cranelift);
            config.cranelift_opt_level(OptLevel::Speed);
        }

        // Enable copy-on-write memory
        config.memory_init_cow(true);

        // Multi-threading
        config.parallel_compilation(true);
        config.async_support(true);

        let engine = Engine::new(&config)?;
        let mut linker: Linker<WasmIOContext> = Linker::new(&engine);

        // Register host functions
        HostFunctions::register(&mut linker)?;
        wasmtime_wasi::preview1::add_to_linker_async(
            &mut linker,
            |state: &mut WasmIOContext| -> &mut WasiP1Ctx { state.wasi_ctx() },
        )?;

        // Create cache directory if it doesn't exist
        let cache_path = cache_dir.as_ref();
        if !cache_path.exists() {
            fs::create_dir_all(&cache_path)?;
        }

        Ok(Self {
            engine,
            linker,
            precompiled_modules: tokio::sync::Mutex::new(HashMap::new()),
            running_executions: tokio::sync::Mutex::new(HashMap::new()),
            metrics: tokio::sync::Mutex::new(Vec::new()),
            precompile_metrics: tokio::sync::Mutex::new(Vec::new()),
            cache_dir: cache_path.to_string_lossy().to_string(),
            default_timeout_seconds: default_timeout_seconds.unwrap_or(300), // 5 minutes default
        })
    }

    /// Precompile a WebAssembly module and return a module ID
    pub async fn precompile_module<P: AsRef<Path>>(
        &self,
        wasm_path: P,
    ) -> Result<(String, PrecompileMetrics)> {
        let total_start = Instant::now();

        // Generate unique module ID
        let module_id = {
            let mut counter = MODULE_COUNTER.lock().unwrap();
            *counter += 1;
            format!("module_{}", counter)
        };

        // Load .wasm file
        let load_start = Instant::now();
        let wasm_bytes = fs::read(&wasm_path).context(format!(
            "Failed to read WebAssembly file: {:?}",
            wasm_path.as_ref()
        ))?;
        let load_time = load_start.elapsed();
        let wasm_size = wasm_bytes.len() as u64;

        // Compile to .cwasm
        let compile_start = Instant::now();
        let module = Module::from_binary(&self.engine, &wasm_bytes)
            .context("Failed to compile WebAssembly module")?;

        // Serialize to precompiled format
        let precompiled_bytes = module
            .serialize()
            .context("Failed to serialize compiled module")?;
        let compile_time = compile_start.elapsed();

        // Save .cwasm file
        let save_start = Instant::now();
        let cwasm_path = format!("{}/{}.cwasm", self.cache_dir, module_id);
        fs::write(&cwasm_path, &precompiled_bytes).context("Failed to write precompiled module")?;
        let save_time = save_start.elapsed();
        let cwasm_size = precompiled_bytes.len() as u64;

        // Store precompiled module
        let precompiled_module = PrecompiledModule {
            module_id: module_id.clone(),
            cwasm_path: cwasm_path.clone(),
            module,
            creation_time: Instant::now(),
        };

        self.precompiled_modules
            .lock()
            .await
            .insert(module_id.clone(), precompiled_module);

        let total_time = total_start.elapsed();

        let metrics = PrecompileMetrics {
            wasm_load_time_us: load_time.as_micros() as u64,
            compilation_time_us: compile_time.as_micros() as u64,
            save_time_us: save_time.as_micros() as u64,
            total_precompile_time_us: total_time.as_micros() as u64,
            wasm_size_bytes: wasm_size,
            cwasm_size_bytes: cwasm_size,
        };

        // Store metrics
        self.precompile_metrics.lock().await.push(metrics.clone());

        tracing::info!(
            "Precompiled module {} in {}μs (load: {}μs, compile: {}μs, save: {}μs)",
            module_id,
            metrics.total_precompile_time_us,
            metrics.wasm_load_time_us,
            metrics.compilation_time_us,
            metrics.save_time_us
        );

        Ok((module_id, metrics))
    }

    /// Run a module with timeout and cancellation support
    pub async fn run_module_with_timeout(
        &self,
        module_id: String,
        env_vars: HashMap<String, String>,
        args: Vec<String>,
        timeout_seconds: Option<u64>,
    ) -> Result<(PerformanceMetrics, i32)> {
        let execution_id = Uuid::new_v4().to_string();
        let cancellation_token = CancellationToken::new();
        let timeout_duration =
            TokioDuration::from_secs(timeout_seconds.unwrap_or(self.default_timeout_seconds));

        // Track this execution
        let running_execution = RunningExecution {
            execution_id: execution_id.clone(),
            module_id: module_id.clone(),
            start_time: Instant::now(),
            cancellation_token: cancellation_token.clone(),
        };

        self.running_executions
            .lock()
            .await
            .insert(execution_id.clone(), running_execution);

        // Run with timeout and cancellation
        let result = tokio::select! {
            // Run with timeout
            result = timeout(timeout_duration, self.run_module_internal(
                execution_id.clone(),
                module_id,
                env_vars,
                args,
                cancellation_token.clone()
            )) => {
                match result {
                    Ok(Ok((mut metrics, return_code))) => {
                        metrics.execution_id = execution_id.clone();
                        Ok((metrics, return_code))
                    }
                    Ok(Err(e)) => Err(e),
                    Err(_) => {
                        // Timeout occurred
                        let mut metrics = PerformanceMetrics::default();
                        metrics.execution_id = execution_id.clone();
                        metrics.timed_out = true;
                        self.metrics.lock().await.push(metrics.clone());
                        Err(anyhow::anyhow!("Execution timed out after {} seconds", timeout_duration.as_secs()))
                    }
                }
            }
            // Wait for cancellation
            _ = cancellation_token.cancelled() => {
                let mut metrics = PerformanceMetrics::default();
                metrics.execution_id = execution_id.clone();
                metrics.cancelled = true;
                self.metrics.lock().await.push(metrics.clone());
                Err(anyhow::anyhow!("Execution was cancelled"))
            }
        };

        // Remove from running executions
        self.running_executions.lock().await.remove(&execution_id);

        result
    }

    /// Internal method to run module (without timeout logic)
    async fn run_module_internal(
        &self,
        execution_id: String,
        module_id: String,
        env_vars: HashMap<String, String>,
        args: Vec<String>,
        cancellation_token: CancellationToken,
    ) -> Result<(PerformanceMetrics, i32)> {
        let total_start = Instant::now();

        // Check for cancellation before starting
        if cancellation_token.is_cancelled() {
            return Err(anyhow::anyhow!("Execution cancelled before start"));
        }

        // Load precompiled module
        let load_start = Instant::now();
        let module = self.load_precompiled_module(&module_id).await?;
        let load_time = load_start.elapsed();

        // Check for cancellation after loading
        if cancellation_token.is_cancelled() {
            return Err(anyhow::anyhow!("Execution cancelled during module load"));
        }

        // Prepare WASI context
        let mut wasi_builder = WasiCtxBuilder::new();
        wasi_builder.inherit_stdout();
        wasi_builder.inherit_stderr();
        wasi_builder.args(&args);

        let env_vars_refs: Vec<(&str, &str)> = env_vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        wasi_builder.envs(&env_vars_refs);
        let wasip1 = wasi_builder.build_p1();

        // Create I/O context and store
        let io_context = WasmIOContext::new(execution_id.clone(), env_vars, wasip1);
        let mut store = Store::new(&self.engine, io_context);

        // Check for cancellation before instantiation
        if cancellation_token.is_cancelled() {
            return Err(anyhow::anyhow!("Execution cancelled before instantiation"));
        }

        // Instantiate module
        let instantiation_start = Instant::now();
        let instance = self
            .linker
            .instantiate_async(&mut store, &module)
            .await
            .context("Failed to instantiate module")?;
        let instantiation_time = instantiation_start.elapsed();

        // Check for cancellation after instantiation
        if cancellation_token.is_cancelled() {
            return Err(anyhow::anyhow!("Execution cancelled after instantiation"));
        }

        // Find entry function
        let run_func = instance
            .get_typed_func::<(), i32>(&mut store, "_start")
            .or_else(|_| instance.get_typed_func::<(), i32>(&mut store, "run"))
            .context("Failed to find entry function")?;

        // Execute function with periodic cancellation checks
        let execution_start = Instant::now();

        // Create a future that executes the function
        let execution_future = run_func.call_async(&mut store, ());

        // Run the execution future with cancellation checks
        let result = tokio::select! {
            result = execution_future => {
                match result {
                    Ok(val) => val,
                    Err(err) => {
                        // Print the full error message with all the context
                        tracing::error!("Execution error: {}", err);
                        
                        // Print the full error chain
                        let mut source = err.source();
                        while let Some(cause) = source {
                            tracing::error!("Caused by: {}", cause);
                            source = cause.source();
                        }
                        
                        // Try to extract WasmBacktrace if available
                        if let Some(backtrace) = err.downcast_ref::<WasmBacktrace>() {
                            tracing::error!("WebAssembly backtrace: {:?}", backtrace);
                        }
                        
                        return Err(anyhow::anyhow!("Failed to execute function: {}", err));
                    }
                }
            },
            _ = cancellation_token.cancelled() => {
                return Err(anyhow::anyhow!("Execution cancelled during function execution"));
            }
        };

        let execution_time = execution_start.elapsed();
        let total_time = total_start.elapsed();

        let metrics = PerformanceMetrics {
            execution_id: execution_id.clone(),
            module_load_time_us: load_time.as_micros() as u64,
            instantiation_time_us: instantiation_time.as_micros() as u64,
            execution_time_us: execution_time.as_micros() as u64,
            total_run_time_us: total_time.as_micros() as u64,
            timed_out: false,
            cancelled: false,
        };

        // Store metrics
        self.metrics.lock().await.push(metrics.clone());

        tracing::info!(
            "Executed module {} (execution: {}) in {}μs (load: {}μs, instantiate: {}μs, execute: {}μs)",
            module_id,
            execution_id.clone(),
            metrics.total_run_time_us,
            metrics.module_load_time_us,
            metrics.instantiation_time_us,
            metrics.execution_time_us
        );

        Ok((metrics, result))
    }

    /// Convenience method that uses the default timeout
    pub async fn run_module(
        &self,
        module_id: String,
        env_vars: HashMap<String, String>,
        args: Vec<String>,
    ) -> Result<(PerformanceMetrics, i32)> {
        self.run_module_with_timeout(module_id, env_vars, args, None)
            .await
    }

    /// Load a precompiled module (from cache or disk)
    async fn load_precompiled_module(&self, module_id: &str) -> Result<Module> {
        // Check if module is in memory cache
        {
            let modules = self.precompiled_modules.lock().await;
            if let Some(precompiled) = modules.get(module_id) {
                tracing::debug!("Using cached module {}", module_id);
                return Ok(precompiled.module.clone());
            }
        }

        // Load from disk
        let cwasm_path = format!("{}/{}.cwasm", self.cache_dir, module_id);
        if !Path::new(&cwasm_path).exists() {
            anyhow::bail!("Precompiled module not found: {}", module_id);
        }

        tracing::debug!("Loading precompiled module from disk: {}", cwasm_path);
        let precompiled_bytes =
            fs::read(&cwasm_path).context("Failed to read precompiled module")?;

        // Deserialize module
        let module = unsafe {
            Module::deserialize(&self.engine, &precompiled_bytes)
                .context("Failed to deserialize precompiled module")?
        };

        // Cache the module
        let precompiled_module = PrecompiledModule {
            module_id: module_id.to_string(),
            cwasm_path,
            module: module.clone(),
            creation_time: Instant::now(),
        };

        self.precompiled_modules
            .lock()
            .await
            .insert(module_id.to_string(), precompiled_module);

        Ok(module)
    }

    /// Terminate a running execution
    pub async fn terminate_execution(&self, execution_id: &str) -> Result<()> {
        let running_executions = self.running_executions.lock().await;

        if let Some(execution) = running_executions.get(execution_id) {
            execution.cancellation_token.cancel();
            tracing::info!("Terminated execution: {}", execution_id);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Execution not found: {}", execution_id))
        }
    }

    /// Get all running executions
    pub async fn get_running_executions(&self) -> Vec<RunningExecution> {
        self.running_executions
            .lock()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Terminate all running executions for a specific module
    pub async fn terminate_module_executions(&self, module_id: &str) -> Result<usize> {
        let running_executions = self.running_executions.lock().await;
        let mut count = 0;

        for execution in running_executions.values() {
            if execution.module_id == module_id {
                execution.cancellation_token.cancel();
                count += 1;
            }
        }

        tracing::info!("Terminated {} executions for module: {}", count, module_id);
        Ok(count)
    }

    /// Set default timeout for the runtime
    pub fn set_default_timeout(&mut self, timeout_seconds: u64) {
        self.default_timeout_seconds = timeout_seconds;
    }

    /// Get all collected runtime metrics
    pub async fn get_metrics(&self) -> Vec<PerformanceMetrics> {
        self.metrics.lock().await.clone()
    }

    /// Get all collected precompilation metrics
    pub async fn get_precompile_metrics(&self) -> Vec<PrecompileMetrics> {
        self.precompile_metrics.lock().await.clone()
    }

    /// Export metrics to CSV files
    pub async fn export_metrics_to_csv<P: AsRef<Path>>(&self, base_path: P) -> Result<()> {
        let base = base_path.as_ref();

        // Export runtime metrics
        let runtime_metrics = self.metrics.lock().await;
        if !runtime_metrics.is_empty() {
            let runtime_path = base.with_extension("runtime.csv");
            let mut csv = String::from("execution_id,module_load_time_us,instantiation_time_us,execution_time_us,total_run_time_us,timed_out,cancelled\n");

            for metric in runtime_metrics.iter() {
                csv.push_str(&format!(
                    "{},{},{},{},{},{},{}\n",
                    metric.execution_id,
                    metric.module_load_time_us,
                    metric.instantiation_time_us,
                    metric.execution_time_us,
                    metric.total_run_time_us,
                    metric.timed_out as i32,
                    metric.cancelled as i32
                ));
            }

            fs::write(runtime_path, csv)?;
        }

        // Export precompilation metrics
        let precompile_metrics = self.precompile_metrics.lock().await;
        if !precompile_metrics.is_empty() {
            let precompile_path = base.with_extension("precompile.csv");
            let mut csv = String::from("wasm_load_time_us,compilation_time_us,save_time_us,total_precompile_time_us,wasm_size_bytes,cwasm_size_bytes\n");

            for metric in precompile_metrics.iter() {
                csv.push_str(&format!(
                    "{},{},{},{},{},{}\n",
                    metric.wasm_load_time_us,
                    metric.compilation_time_us,
                    metric.save_time_us,
                    metric.total_precompile_time_us,
                    metric.wasm_size_bytes,
                    metric.cwasm_size_bytes
                ));
            }

            fs::write(precompile_path, csv)?;
        }

        Ok(())
    }
}
