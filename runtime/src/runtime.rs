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

// Enhanced performance metrics struct with microsecond precision and detailed memory measurements
#[derive(Debug, Clone, Default)]
pub struct PerformanceMetrics {
    pub module_load_time_us: u64,      // Time to load module from disk
    pub module_compile_time_us: u64,   // Time to compile module (if not cached)
    pub instantiation_time_us: u64,    // Time to instantiate the module
    pub total_cold_start_time_us: u64, // Total time from request to ready (load + compile + instantiation)
    pub warm_start_time_us: u64,       // Time to instantiate the module (warm start)
    pub execution_time_us: u64,        // Time for function execution
    pub from_cache: bool,              // Whether module was loaded from cache
    pub memory_usage_kb: u64,          // Total memory used by the instance
    pub module_memory_kb: u64,         // Memory used by the module itself
    pub instantiation_memory_kb: u64,  // Memory overhead from instantiation
    pub execution_memory_kb: u64,      // Memory allocated during execution
}

// Memory snapshot for calculating overhead
#[derive(Debug, Clone)]
pub struct MemorySnapshot {
    pub total_memory_kb: u64,
    pub used_memory_kb: u64,
    pub process_memory_kb: u64,
    pub timestamp: Instant,
}

/// Represents a running Wasm instance
pub struct Instance {
    pub id: String,
    pub store: Store<WasmIOContext>,
    pub instance_pre: wasmtime::InstancePre<WasmIOContext>,
    pub run_func: Option<TypedFunc<(), i32>>,
    pub metrics: PerformanceMetrics,
    pub memory_before_load: Option<MemorySnapshot>,
    pub memory_after_load: Option<MemorySnapshot>,
    pub memory_after_compile: Option<MemorySnapshot>,
    pub memory_before_instantiation: Option<MemorySnapshot>,
    pub memory_after_instantiation: Option<MemorySnapshot>,
    pub memory_after_execution: Option<MemorySnapshot>,
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
    pub fn new<P: AsRef<Path>>(_cache_dir: P, optimize: bool, cache: bool) -> Result<Self> {
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

    // Take memory snapshot to calculate overhead - outside timing measurements
    fn take_memory_snapshot(&self) -> Result<MemorySnapshot> {
        let mut system = System::new_all();
        system.refresh_all();

        let pid = sysinfo::Pid::from(std::process::id() as usize);
        let process = system.process(pid).context("Failed to get process info")?;

        Ok(MemorySnapshot {
            total_memory_kb: system.total_memory() / 1024,
            used_memory_kb: system.used_memory() / 1024,
            process_memory_kb: process.memory() / 1024,
            timestamp: Instant::now(),
        })
    }

    /// Initialize an instance from a file path with accurate memory measurements
    pub async fn init_instance_from_file<P: AsRef<Path>>(
        &self,
        wasm_path: P,
    ) -> Result<(String, PerformanceMetrics)> {
        // Take memory snapshot before any operations
        let memory_before_load = self.take_memory_snapshot()?;

        // Load the WebAssembly module from file - measure time separately from memory
        let module_load_start = Instant::now();
        let wasm_bytes = fs::read(&wasm_path).context(format!(
            "Failed to read WebAssembly file: {:?}",
            wasm_path.as_ref()
        ))?;
        let module_load_time = module_load_start.elapsed();

        // Take memory snapshot after loading
        let memory_after_load = self.take_memory_snapshot()?;
        let load_memory_kb = memory_after_load.process_memory_kb - memory_before_load.process_memory_kb;

        // Compile or get from cache - measure time separately
        let module_compile_start = Instant::now();
        let (module, from_cache) = if self.enable_cache {
            match self.module_cache.get(&wasm_bytes) {
                Some(module) => {
                    tracing::debug!("Using cached module");
                    (module, true)
                }
                None => {
                    // Compile and cache the module
                    tracing::debug!("Compiling new module");
                    let module = Module::from_binary(&self.engine, &wasm_bytes)
                        .map_err(|e| anyhow::anyhow!("Failed to compile Wasm module {}", e))?;

                    self.module_cache.insert(
                        wasm_bytes.to_vec(),
                        ModuleData {
                            module: module.clone(),
                            creation_time: Instant::now(),
                            size_bytes: wasm_bytes.len(),
                        },
                    );
                    (module, false)
                }
            }
        } else {
            // Always compile if cache is disabled
            tracing::debug!("Cache disabled. Compiling module");
            let module = Module::from_binary(&self.engine, &wasm_bytes)
                .map_err(|e| anyhow::anyhow!("Failed to compile Wasm module {}", e))?;
            (module, false)
        };
        let module_compile_time = module_compile_start.elapsed();

        // Take memory snapshot after compilation
        let memory_after_compile = self.take_memory_snapshot()?;
        let compile_memory_kb = memory_after_compile.process_memory_kb - memory_after_load.process_memory_kb;
        
        // Get a new sequential instance ID
        let instance_id = {
            let mut counter = INSTANCE_COUNTER.lock().unwrap();
            *counter += 1;
            counter.to_string()
        };

        // Store the module ID in the instances map with placeholder
        let metrics = PerformanceMetrics {
            module_load_time_us: module_load_time.as_micros() as u64,
            module_compile_time_us: module_compile_time.as_micros() as u64,
            instantiation_time_us: 0, // Will be set during run
            total_cold_start_time_us: 0, // Will be calculated during cold start
            warm_start_time_us: 0, // Will be set during run (same as instantiation_time_us)
            execution_time_us: 0, // Will be set during run
            from_cache,
            memory_usage_kb: load_memory_kb + compile_memory_kb,
            module_memory_kb: load_memory_kb,
            instantiation_memory_kb: 0, // Will be set during run
            execution_memory_kb: 0, // Will be set during run
        };

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
            memory_before_load: Some(memory_before_load),
            memory_after_load: Some(memory_after_load),
            memory_after_compile: Some(memory_after_compile),
            memory_before_instantiation: None,
            memory_after_instantiation: None,
            memory_after_execution: None,
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

        // Store metrics for research
        self.metrics.lock().await.push(metrics.clone());

        Ok((instance_id, metrics))
    }

    /// Run a function in the given instance with environment variables and args
    pub async fn run_instance(
        &self,
        instance_id: String,
        env_vars: HashMap<String, String>,
        args: Vec<String>,
    ) -> Result<(PerformanceMetrics, i32)> {
        // Take memory snapshot before instantiation - outside timing measurements
        let memory_before_instantiation = self.take_memory_snapshot()?;

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

        // Measure instantiation time (warm start time) - separate from memory measurements
        let instantiation_start = Instant::now();
        let instance = instance_pre
            .instantiate_async(&mut store)
            .await
            .context("Failed to instantiate Wasm module")?;
        let instantiation_time = instantiation_start.elapsed();

        // Try to pre-lookup entry function for faster invocation
        // Check both "_start" (WASI) and "_run" (custom function)
        let run_func = instance
            .get_typed_func::<(), i32>(&mut store, "_start")
            .or_else(|_| instance.get_typed_func::<(), i32>(&mut store, "run"))
            .context("Failed to find entry function")?;

        // Take memory snapshot after instantiation but before execution
        let memory_after_instantiation = self.take_memory_snapshot()?;
        let instantiation_memory_kb = memory_after_instantiation.process_memory_kb 
                                    - memory_before_instantiation.process_memory_kb;

        // Execute the function - measure time separately from memory
        let execution_start = Instant::now();
        let result = run_func
            .call_async(&mut store, ())
            .await
            .context("Failed to call function")?;
        let execution_time = execution_start.elapsed();

        // Take memory snapshot after execution
        let memory_after_execution = self.take_memory_snapshot()?;
        let execution_memory_kb = memory_after_execution.process_memory_kb 
                                - memory_after_instantiation.process_memory_kb;
        
        // Calculate total memory usage
        let total_memory_kb = memory_after_execution.process_memory_kb 
                            - memory_before_instantiation.process_memory_kb;

        // Update metrics - must lock mutex again
        let metrics = {
            let mut guard = inst.lock().await;
            
            // Update memory snapshots for future analysis
            guard.memory_before_instantiation = Some(memory_before_instantiation);
            guard.memory_after_instantiation = Some(memory_after_instantiation);
            guard.memory_after_execution = Some(memory_after_execution);
            
            // Update metrics
            let mut metrics = guard.metrics.clone();
            metrics.instantiation_time_us = instantiation_time.as_micros() as u64;
            metrics.warm_start_time_us = instantiation_time.as_micros() as u64; // Warm start = instantiation time
            metrics.execution_time_us = execution_time.as_micros() as u64;
            metrics.memory_usage_kb = total_memory_kb;
            metrics.instantiation_memory_kb = instantiation_memory_kb;
            metrics.execution_memory_kb = execution_memory_kb;
            
            // Update instance metrics
            guard.metrics = metrics.clone();
            metrics
        };
        
        // Add to research metrics
        self.metrics.lock().await.push(metrics.clone());

        tracing::info!("Function execution finished with result: {}", result);
        tracing::info!("Memory usage: Instantiation: {}KB, Execution: {}KB, Total: {}KB", 
                      instantiation_memory_kb, execution_memory_kb, total_memory_kb);

        // Return both metrics and result
        Ok((metrics, result))
    }

    /// Run a cold start (init + run) with file input only
    pub async fn init_and_run<P: AsRef<Path>>(
        &self,
        wasm_path: P,
        env_vars: HashMap<String, String>,
        args: Vec<String>,
    ) -> Result<(String, PerformanceMetrics, i32)> {

        // Initialize the instance
        let (instance_id, init_metrics) = self.init_instance_from_file(&wasm_path).await?;

        // Run the instance
        let (run_metrics, result) = self
            .run_instance(instance_id.clone(), env_vars, args)
            .await?;

        // Redefine cold start time = load time + compile time + instantiation time
        let combined_metrics = PerformanceMetrics {
            module_load_time_us: init_metrics.module_load_time_us,
            module_compile_time_us: init_metrics.module_compile_time_us,
            instantiation_time_us: run_metrics.instantiation_time_us,
            execution_time_us: run_metrics.execution_time_us,
            warm_start_time_us: run_metrics.instantiation_time_us, // Warm start = instantiation time
            total_cold_start_time_us: init_metrics.module_load_time_us + 
                                     init_metrics.module_compile_time_us + 
                                     run_metrics.instantiation_time_us,
            from_cache: init_metrics.from_cache,
            memory_usage_kb: run_metrics.memory_usage_kb,
            module_memory_kb: init_metrics.module_memory_kb,
            instantiation_memory_kb: run_metrics.instantiation_memory_kb,
            execution_memory_kb: run_metrics.execution_memory_kb,
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

    /// Get detailed memory metrics for an instance
    pub async fn get_detailed_memory_metrics(&self, instance_id: &str) -> Result<DetailedMemoryMetrics> {
        let instances = self.instances.lock().await;
        let instance = instances
            .get(instance_id)
            .context(format!("No instance: {}", instance_id))?;

        let guard = instance.lock().await;
        
        // Get memory snapshots
        let memory_before_load = guard.memory_before_load.clone()
            .context("Memory snapshot before load not available")?;
        let memory_after_compile = guard.memory_after_compile.clone()
            .context("Memory snapshot after compile not available")?;
            
        // These may not be available if run hasn't been called yet
        let memory_after_execution = guard.memory_after_execution.clone();
        let memory_before_instantiation = guard.memory_before_instantiation.clone();
        let memory_after_instantiation = guard.memory_after_instantiation.clone();
        
        // Calculate memory metrics
        let mut metrics = DetailedMemoryMetrics {
            process_memory_before_kb: memory_before_load.process_memory_kb,
            process_memory_after_compile_kb: memory_after_compile.process_memory_kb,
            process_memory_after_execution_kb: memory_after_execution.clone()
                .map(|m| m.process_memory_kb)
                .unwrap_or(0),
            module_memory_kb: guard.metrics.module_memory_kb,
            instantiation_memory_kb: guard.metrics.instantiation_memory_kb,
            execution_memory_kb: guard.metrics.execution_memory_kb,
            total_memory_kb: guard.metrics.memory_usage_kb,
            system_total_memory_kb: memory_before_load.total_memory_kb,
            system_used_memory_kb: memory_before_load.used_memory_kb,
        };
        
        // Add the instantiation and execution metrics if available
        if let (Some(before_inst), Some(after_inst), Some(after_exec)) = 
            (memory_before_instantiation, memory_after_instantiation, memory_after_execution.clone()) {
            metrics.instantiation_memory_kb = after_inst.process_memory_kb - before_inst.process_memory_kb;
            metrics.execution_memory_kb = after_exec.process_memory_kb - after_inst.process_memory_kb;
        }

        Ok(metrics)
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

        let mut csv = String::from("module_load_time_us,module_compile_time_us,instantiation_time_us,total_cold_start_time_us,warm_start_time_us,execution_time_us,from_cache,memory_usage_kb,module_memory_kb,instantiation_memory_kb,execution_memory_kb\n");

        for metric in metrics.iter() {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                metric.module_load_time_us,
                metric.module_compile_time_us,
                metric.instantiation_time_us,
                metric.total_cold_start_time_us,
                metric.warm_start_time_us,
                metric.execution_time_us,
                metric.from_cache as i32,
                metric.memory_usage_kb,
                metric.module_memory_kb,
                metric.instantiation_memory_kb,
                metric.execution_memory_kb
            ));
        }

        fs::write(path, csv)?;

        Ok(())
    }
}

/// Detailed memory metrics for analysis
#[derive(Debug, Clone)]
pub struct DetailedMemoryMetrics {
    pub process_memory_before_kb: u64,
    pub process_memory_after_compile_kb: u64,
    pub process_memory_after_execution_kb: u64,
    pub module_memory_kb: u64,
    pub instantiation_memory_kb: u64,
    pub execution_memory_kb: u64,
    pub total_memory_kb: u64,
    pub system_total_memory_kb: u64,
    pub system_used_memory_kb: u64,
}