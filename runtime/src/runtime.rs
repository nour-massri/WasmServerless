//runtime.rs
use crate::hostfuncs::{HostFunctions, WasmIOContext};
use crate::module_cache::ModuleCache;
use crate::module_cache::ModuleData;
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::System;
use wasmtime::*;
use wasmtime_wasi::preview1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

// Global instance counter
static INSTANCE_COUNTER: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));

// Performance metrics struct with microsecond precision
#[derive(Debug, Clone, Default)]
pub struct PerformanceMetrics {
    pub module_load_time_us: u64,      // Time to load module from disk
    pub module_compile_time_us: u64,   // Time to compile module (if not cached)
    pub instantiation_time_us: u64,    // Time to instantiate the module
    pub total_cold_start_time_us: u64, // Total time from request to ready
    pub warm_start_time_us: u64,       // Time to call function on warm instance
    pub execution_time_us: u64,        // Time for function execution
    pub from_cache: bool,              // Whether module was loaded from cache
    pub memory_usage_kb: u64,          // Memory used by the instance
}
// Memory snapshot for calculating overhead
#[derive(Debug, Clone)]
pub struct MemorySnapshot {
    pub total_memory_kb: u64,
    pub used_memory_kb: u64,
    pub process_memory_kb: u64,
}

/// Represents a running Wasm instance
pub struct Instance {
    pub id: String,
    pub store: Store<WasmIOContext>,
    pub instance_pre: wasmtime::InstancePre<WasmIOContext>,
    pub run_func: Option<TypedFunc<(), i32>>,
    pub metrics: PerformanceMetrics,
    pub memory_before: Option<MemorySnapshot>,
    pub memory_after: Option<MemorySnapshot>,
}

/// Main runtime for wasmtime
pub struct Runtime {
    engine: Engine,
    linker: Linker<WasmIOContext>,
    module_cache: ModuleCache,
    instances: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Instance>>>>,
    pub metrics: tokio::sync::Mutex<Vec<PerformanceMetrics>>,
    enable_cache: bool,
}

impl Runtime {
    /// Create a new runtime with the given cache directory
    pub fn new<P: AsRef<Path>>(cache_dir: P, optimize: bool, cache: bool) -> Result<Self> {
        let mut config = Config::new();

        if !optimize {
            // Faster init
            config.strategy(Strategy::Winch);
        } else {
            // Optimize for execution speed
            config.strategy(Strategy::Cranelift);
            config.cranelift_opt_level(OptLevel::Speed);
        }

        // Enable copy-on-write memory
        config.memory_init_cow(true);

        // Configure the cache properly - don't try to load from file
        // let cache_dir_str = cache_dir.as_ref().to_string_lossy().to_string();
        // config.cache_config(true, &cache_dir_str)?;

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

        Ok(Self {
            engine,
            linker,
            module_cache: ModuleCache::new(),
            instances: tokio::sync::Mutex::new(HashMap::new()),
            metrics: tokio::sync::Mutex::new(Vec::new()),
            enable_cache: cache,
        })
    }

    // Set cache enabled/disabled
    pub fn set_cache_enabled(&mut self, enabled: bool) {
        self.enable_cache = enabled;
    }

    // Take memory snapshot to calculate overhead
    fn take_memory_snapshot(&self) -> Result<MemorySnapshot> {
        let mut system = System::new_all();
        system.refresh_all();

        let pid = sysinfo::Pid::from(std::process::id() as usize);
        let process = system.process(pid).context("Failed to get process info")?;

        Ok(MemorySnapshot {
            total_memory_kb: system.total_memory() / 1024,
            used_memory_kb: system.used_memory() / 1024,
            process_memory_kb: process.memory() / 1024,
        })
    }

    /// Initialize an instance from a file path with the given environment variables and args
    pub async fn init_instance_from_file<P: AsRef<Path>>(
        &self,
        wasm_path: P,
    ) -> Result<(String, PerformanceMetrics)> {
        // Load the WebAssembly module from file
        let module_load_start = Instant::now();
        let wasm_bytes = fs::read(&wasm_path).context(format!(
            "Failed to read WebAssembly file: {:?}",
            wasm_path.as_ref()
        ))?;
        let module_load_time = module_load_start.elapsed();

        // Initialize instance with the loaded bytes
        let (instance_id, mut metrics) = self.init_instance(&wasm_bytes).await?;
        
        metrics.module_load_time_us = module_load_time.as_micros() as u64;

        // Store metrics for research
        self.metrics.lock().await.push(metrics.clone());

        // Return the instance ID and metrics
        Ok((instance_id, metrics))
    }

    /// Initialize a new instance with the given module bytes
    pub async fn init_instance(&self, module_bytes: &[u8]) -> Result<(String, PerformanceMetrics)> {
        let mut metrics = PerformanceMetrics::default();

        // Get a new sequential instance ID
        let instance_id = {
            let mut counter = INSTANCE_COUNTER.lock().unwrap();
            *counter += 1;
            counter.to_string()
        };

        // Take memory snapshot before compilation
        let memory_before = self.take_memory_snapshot()?;

        // Compile or get from cache
        let module_compile_start = Instant::now();
        let (module, _from_cache) = if self.enable_cache {
            match self.module_cache.get(module_bytes) {
                Some(module) => {
                    tracing::debug!("Using cached module");
                    metrics.from_cache = true;
                    (module, true)
                }
                None => {
                    // Compile and cache the module
                    tracing::debug!("Compiling new module");
                    let module = Module::from_binary(&self.engine, module_bytes)
                        .map_err(|e| anyhow::anyhow!("Failed to compile Wasm module {}", e))?;

                    self.module_cache.insert(
                        module_bytes.to_vec(),
                        ModuleData {
                            module: module.clone(),
                            creation_time: Instant::now(),
                            size_bytes: module_bytes.len(),
                        },
                    );
                    metrics.from_cache = false;
                    (module, false)
                }
            }
        } else {
            // Always compile if cache is disabled
            tracing::debug!("Cache disabled. Compiling module");
            let module = Module::from_binary(&self.engine, module_bytes)
                .map_err(|e| anyhow::anyhow!("Failed to compile Wasm module {}", e))?;
            metrics.from_cache = false;
            (module, false)
        };
        metrics.module_compile_time_us = module_compile_start.elapsed().as_micros() as u64;

        // Take memory snapshot after compilation to measure overhead
        let memory_after = self.take_memory_snapshot()?;
        metrics.memory_usage_kb = memory_after.process_memory_kb - memory_before.process_memory_kb;

        // Store the module ID in the instances map with placeholder
        let placeholder_instance = Instance {
            id: instance_id.clone(),
            store: Store::new(
                &self.engine,
                WasmIOContext::new(
                    instance_id.clone(),
                    HashMap::new(),
                    WasiCtxBuilder::new().build_p1(),
                ),
            ),

            instance_pre: self.linker.instantiate_pre(&module)?,
            run_func: None,
            metrics: metrics.clone(),
            memory_before: Some(memory_before),
            memory_after: Some(memory_after),
        };

        // Store the instance
        self.instances.lock().await.insert(
            instance_id.clone(),
            Arc::new(tokio::sync::Mutex::new(placeholder_instance)),
        );

        tracing::info!(
            "Initialized module {}, total instances: {}",
            instance_id,
            INSTANCE_COUNTER.lock().unwrap()
        );

        Ok((instance_id, metrics))
    }

    /// Run a function in the given instance with environment variables and args
    pub async fn run_instance(
        &self,
        instance_id: String,
        env_vars: HashMap<String, String>,
        args: Vec<String>,
    ) -> Result<(PerformanceMetrics, i32)> {
        // Add environment variables all at once
        // Convert env_vars to the format expected by envs()
        let memory_before = self.take_memory_snapshot()?;
        // Instantiate the module with the configured store
        let instantiation_start = Instant::now();

        // Get the instance
        let inst = {
            let m = self.instances.lock().await;
            m.get(&instance_id)
                .cloned()
                .context(format!("No instance: {}", instance_id))?
        };

        // Important: Get data from the instance and drop the mutex guard BEFORE any await points
        let instance_pre = {
            let guard = inst.lock().await;
            guard.instance_pre.clone() // Clone the pre-instance to avoid holding the guard
        };

        // Prepare WASI context with environment variables and args
        let mut wasi_builder = WasiCtxBuilder::new();
        // Add this to explicitly set up stdout/stderr
        wasi_builder.inherit_stdout();
        wasi_builder.inherit_stderr();

        // Add args all at once
        wasi_builder.args(&args);

        // Add environment variables all at once
        // Convert env_vars to the format expected by envs()
        let env_vars_refs: Vec<(&str, &str)> = env_vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        wasi_builder.envs(&env_vars_refs);
        let wasip1 = wasi_builder.build_p1();

        // Create the I/O context with WASI
        let io_context = WasmIOContext::new(instance_id.clone(), env_vars, wasip1);

        // Create a store with the I/O context
        let mut store = Store::new(&self.engine, io_context);

        // Complete instantiation from the pre-instance
        let instance = instance_pre
            .instantiate_async(&mut store)
            .await
            .context("Failed to instantiate Wasm module")?;
        let instantiation_time = instantiation_start.elapsed();

        // Try to pre-lookup entry function for faster invocation
        // Check both "_start" (WASI) and "_run" (our custom entry point)
        let run_func = instance
            .get_typed_func::<(), i32>(&mut store, "_start")
            .or_else(|_| instance.get_typed_func::<(), i32>(&mut store, "run"))
            .context("Failed to find entry function")?;

        // Execute the function and get the result
        let execution_start = Instant::now();
        // Now we can safely await without holding any MutexGuard
        let result = run_func
            .call_async(&mut store, ())
            .await
            .context("Failed to call function")?;
        tracing::info!("Function execution finished with result: {}", result);

        let execution_time = execution_start.elapsed();

        // Take memory snapshot after execution
        let memory_after = self.take_memory_snapshot()?;
        let memory_usage_kb = memory_after.process_memory_kb - memory_before.process_memory_kb;

        // Update metrics - must lock mutex again
        let metrics = {
            let mut guard = inst.lock().await;
            let mut metrics = guard.metrics.clone();
            metrics.instantiation_time_us = instantiation_time.as_micros() as u64;
            metrics.warm_start_time_us = instantiation_time.as_micros() as u64;
            metrics.execution_time_us = execution_time.as_micros() as u64;
            metrics.memory_usage_kb = memory_usage_kb;
            // Update instance metrics
            guard.metrics = metrics.clone();
            metrics
        };
        // Add to research metrics
        self.metrics.lock().await.push(metrics.clone());

        // Return both metrics and result
        Ok((metrics, result))
    }

    /// Run a cold start (init + run)
    pub async fn cold_start(
        &self,
        wasm_bytes: &[u8],
        env_vars: HashMap<String, String>,
        args: Vec<String>,
    ) -> Result<(String, PerformanceMetrics, i32)> {
        let cold_start_time = Instant::now();

        // Initialize the instance
        let (instance_id, mut init_metrics) = self.init_instance(wasm_bytes).await?;

        // Run the instance
        let (run_metrics, result) = self
            .run_instance(instance_id.clone(), env_vars, args)
            .await?;

        // Combine metrics for total cold start
        let combined_metrics = PerformanceMetrics {
            module_load_time_us: init_metrics.module_load_time_us,
            module_compile_time_us: init_metrics.module_compile_time_us,
            instantiation_time_us: run_metrics.instantiation_time_us,
            execution_time_us: run_metrics.execution_time_us,
            warm_start_time_us: run_metrics.warm_start_time_us,
            total_cold_start_time_us: cold_start_time.elapsed().as_micros() as u64,
            from_cache: init_metrics.from_cache,
            memory_usage_kb: run_metrics.memory_usage_kb,
        };

        // Store metrics
        self.metrics.lock().await.push(combined_metrics.clone());

        Ok((instance_id, combined_metrics, result))
    }

    /// Benchmark throughput by running the instance multiple times
    pub async fn benchmark_throughput(
        &self,
        instance_id: String,
        iterations: usize,
        env_vars: HashMap<String, String>,
        args: Vec<String>,
    ) -> Result<(f64, Vec<Duration>)> {
        let mut durations = Vec::with_capacity(iterations);

        for _ in 0..iterations {
            let start = Instant::now();
            let _ = self
                .run_instance(instance_id.clone(), env_vars.clone(), args.clone())
                .await?;
            durations.push(start.elapsed());
        }

        // Calculate average ops per second
        let total_duration: Duration = durations.iter().sum();
        let ops_per_second = iterations as f64 / total_duration.as_secs_f64();

        Ok((ops_per_second, durations))
    }

    /// Get memory overhead for an instance
    pub async fn get_memory_overhead(&self, instance_id: &str) -> Result<u64> {
        let instances = self.instances.lock().await;
        let instance = instances
            .get(instance_id)
            .context(format!("No instance: {}", instance_id))?;

        let guard = instance.lock().await;

        Ok(guard.metrics.memory_usage_kb)
    }

    /// Terminate and remove an instance
    pub async fn terminate_instance(&self, instance_id: &str) -> Result<()> {
        let mut instances = self.instances.lock().await;
        if instances.remove(instance_id).is_some() {
            tracing::info!(
                "Terminated instance {}, remaining instances: {}",
                instance_id,
                instances.len()
            );
            Ok(())
        } else {
            anyhow::bail!("Instance not found: {}", instance_id);
        }
    }

    /// Get all collected metrics for research
    pub async fn get_metrics(&self) -> Vec<PerformanceMetrics> {
        self.metrics.lock().await.clone()
    }

    /// Export metrics to a CSV file for analysis
    pub async fn export_metrics_to_csv<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let metrics = self.metrics.lock().await;
        if metrics.is_empty() {
            return Ok(());
        }

        let mut csv = String::from("module_load_time_us,module_compile_time_us,instantiation_time_us,total_cold_start_time_us,warm_start_time_us,execution_time_us,from_cache,memory_usage_kb\n");

        for metric in metrics.iter() {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{}\n",
                metric.module_load_time_us,
                metric.module_compile_time_us,
                metric.instantiation_time_us,
                metric.total_cold_start_time_us,
                metric.warm_start_time_us,
                metric.execution_time_us,
                metric.from_cache as i32,
                metric.memory_usage_kb
            ));
        }

        fs::write(path, csv)?;

        Ok(())
    }
}
