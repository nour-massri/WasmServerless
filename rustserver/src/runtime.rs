
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use anyhow::{Result, Context};
use wasmtime::*;
use crate::hostfuncs::{WasmIOContext, HostFunctions};
use crate::module_cache::ModuleCache;
use once_cell::sync::Lazy;
use uuid::Uuid;
use std::fs;

// Import WASI modules without the sync namespace
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder};

// Global instance counter
static INSTANCE_COUNTER: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));

// Performance metrics struct
#[derive(Debug, Clone, Default)]
pub struct PerformanceMetrics {
    pub module_load_time: Duration,    // Time to load module from disk
    pub module_compile_time: Duration, // Time to compile module (if not cached)
    pub instantiation_time: Duration,  // Time to instantiate the module 
    pub total_cold_start_time: Duration, // Total time from request to ready
    pub warm_start_time: Duration,     // Time to call function on warm instance
    pub execution_time: Duration,      // Time for function execution
    pub from_cache: bool,              // Whether module was loaded from cache
}

/// Represents a running Wasm instance
pub struct Instance {
    pub id: String,
    pub store: Store<WasmIOContext>,
    pub instance: wasmtime::Instance,
    pub run_func: Option<TypedFunc<(), i32>>, // Changed to take no params
    pub metrics: PerformanceMetrics,
}

/// Main runtime for wasmtime
pub struct Runtime {
    engine: Engine,
    linker: Linker<WasmIOContext>,
    module_cache: ModuleCache,
    instances: Mutex<HashMap<String, Arc<Mutex<Instance>>>>,
    pub metrics: Mutex<Vec<PerformanceMetrics>>, // Store metrics for research
}

impl Runtime {
    /// Create a new runtime with the given cache directory
    pub fn new<P: AsRef<Path>>(cache_dir: P) -> Result<Self> {
        let mut config = Config::new();
        
        // Optimize for execution speed
        config.cranelift_opt_level(OptLevel::Speed);
        
        // Enable copy-on-write memory
        config.memory_init_cow(true);
        
        // Configure the cache properly - don't try to load from file
        // let cache_dir_str = cache_dir.as_ref().to_string_lossy().to_string();
        // config.cache_config(true, &cache_dir_str)?;
        
        // Multi-threading
        config.parallel_compilation(true);
        
        // Set strategy
        config.strategy(Strategy::Auto);
        
        let engine = Engine::new(&config)?;
        let mut linker: Linker<WasmIOContext> = Linker::new(&engine);
        
        // Register host functions
        HostFunctions::register(&mut linker)?;
        wasmtime_wasi::snapshots::preview_1::add_wasi_snapshot_preview1_to_linker(
            &mut linker,
            |state: &mut WasmIOContext| -> &mut WasiCtx { state.wasi_ctx() }
        )?;

        Ok(Self {
            engine,
            linker,
            module_cache: ModuleCache::new(),
            instances: Mutex::new(HashMap::new()),
            metrics: Mutex::new(Vec::new()),
        })
    }
    
    /// Create a new instance from a file path with the given environment variables and args
    pub async fn create_instance_from_file<P: AsRef<Path>>(
        &self, 
        wasm_path: P, 
        env_vars: HashMap<String, String>,
        args: Vec<String>
    ) -> Result<(String, PerformanceMetrics)> {
        let start_time = Instant::now();
        
        // Load the WebAssembly module from file
        let module_load_start = Instant::now();
        let wasm_bytes = fs::read(&wasm_path)
            .context(format!("Failed to read WebAssembly file: {:?}", wasm_path.as_ref()))?;
        let module_load_time = module_load_start.elapsed();
        
        // Create instance with the loaded bytes
        let (instance_id, mut metrics) = self.create_instance(&wasm_bytes, env_vars, args).await?;
        
        // Update the total cold start time
        metrics.module_load_time = module_load_time;
        metrics.total_cold_start_time = start_time.elapsed();
        
        // Store metrics for research
        self.metrics.lock().unwrap().push(metrics.clone());
        
        // Return the instance ID and metrics
        Ok((instance_id, metrics))
    }
    
    /// Create a new instance with the given module bytes
    pub async fn create_instance(
        &self, 
        module_bytes: &[u8], 
        env_vars: HashMap<String, String>,
        args: Vec<String>
    ) -> Result<(String, PerformanceMetrics)> {
        let start_time = Instant::now();
        let mut metrics = PerformanceMetrics::default();
        
        // Increment global instance counter
        let instance_count = {
            let mut counter = INSTANCE_COUNTER.lock().unwrap();
            *counter += 1;
            *counter
        };
        
        // Create a unique instance ID
        let instance_id = Uuid::new_v4().to_string();
        
        // First, try to get module from cache
        let module_compile_start = Instant::now();
        let (module, from_cache) = match self.module_cache.get(&module_bytes) {
            Some(module) => {
                tracing::debug!("Using cached module");
                metrics.from_cache = true;
                (module, true)
            },
            None => {
                // Compile and cache the module
                tracing::debug!("Compiling new module");
                let module = Module::from_binary(&self.engine, module_bytes)
                    .context("Failed to compile Wasm module")?;
                
                self.module_cache.insert(module_bytes.to_vec(), module.clone());
                metrics.from_cache = false;
                (module, false)
            }
        };
        metrics.module_compile_time = module_compile_start.elapsed();
        
        let wasi_builder = WasiCtxBuilder::new();
        let wasi_builder = wasi_builder.inherit_stdio();
                
        // Handle the Result from each call using ? operator
        let mut wasi_builder_result = Ok(wasi_builder);
        
        // Add args - using the ? operator requires that your function returns Result
        for arg in &args {
            wasi_builder_result = wasi_builder_result?.arg(arg);
        }
                
        // Add environment variables - also using ? operator
        for (key, value) in &env_vars {
            wasi_builder_result = wasi_builder_result?.env(key, value);
        }
                
        // Unwrap at the end or propagate the error
        let wasi_builder = wasi_builder_result?;
        let wasip1 = wasi_builder.build();
        // Create the I/O context with WASI
        let io_context = WasmIOContext::new(instance_id.clone(), env_vars, wasip1);        
        
        // Create a store with the I/O context
        let mut store = Store::new(&self.engine, io_context);
        
        // Instantiate the module
        let instantiation_start = Instant::now();
        let instance = self.linker.instantiate(&mut store, &module)
        .map_err(|e| anyhow::anyhow!("Failed to instantiate Wasm module: {}", e))?;
        metrics.instantiation_time = instantiation_start.elapsed();
        
        // Try to pre-lookup entry function for faster invocation
        // Check both "_start" (WASI) and "_run" (our custom entry point)
        let run_func = instance.get_typed_func::<(), i32>(&mut store, "_start")
            .or_else(|_| instance.get_typed_func::<(), i32>(&mut store, "_run"))
            .ok();
        
        let instance = Instance {
            id: instance_id.clone(),
            store,
            instance,
            run_func,
            metrics: metrics.clone(),
        };
        
        // Store the instance
        self.instances.lock().unwrap().insert(instance_id.clone(), Arc::new(Mutex::new(instance)));
        
        tracing::info!("Created instance {}, total instances: {}", instance_id, instance_count);
        
        Ok((instance_id, metrics))
    }
    
    /// Run a function in the given instance 
    pub async fn run_instance(&self, instance_id: &str) -> Result<(PerformanceMetrics, i32)> {
        // get the Arc<Mutex<Instance>>
        let inst = {
            let m = self.instances.lock().unwrap();
            m.get(instance_id)
             .cloned()
             .context(format!("No instance: {}", instance_id))?
        };
    
        // 1) Lock once
        let mut guard = inst.lock().unwrap();
    
        // 2) Clone out whatever was cached
        let maybe_cached = guard.run_func.clone();
    
        // 3) Decide which function to use - cached or new
        let run_func = if let Some(func) = maybe_cached {
            // we never held a borrow of `guard` into this branch
            func
        } else {
            // Clone out the Instance handle (imm borrows guard.instance, but ends immediately)
            let wasm_inst = guard.instance.clone();
    
            // Call get_typed_func on your clone, borrowing only guard.store mutably
            // Try both "_run" (our custom entry) and "_start" (WASI standard entry)
            let new_f = wasm_inst
            .get_typed_func::<(), i32>(&mut guard.store, "_start")
            .or_else(|_| wasm_inst.get_typed_func::<(), i32>(&mut guard.store, "run"))

            .map_err(|e| anyhow::anyhow!("Failed to get entry function run {}", e))?;                
            guard.run_func = Some(new_f.clone());
            new_f
        };
    
        // Measure warm start time including function execution
        let warm_start = Instant::now();
    
        // Execute the function and get the result
        let result = run_func.call(&mut guard.store, ())
            .context("Failed to call function")?;
        
        // Update metrics
        let mut metrics = guard.metrics.clone();
        metrics.warm_start_time = warm_start.elapsed();
        metrics.execution_time = warm_start.elapsed(); // In this case they're the same
        
        // Store updated metrics
        guard.metrics = metrics.clone();
        
        // Add to research metrics
        self.metrics.lock().unwrap().push(metrics.clone());
        
        // Return both metrics and result
        Ok((metrics, result))
    }
    
    /// Benchmark throughput by running the instance multiple times
    pub async fn benchmark_throughput(&self, instance_id: &str, iterations: usize) -> Result<(f64, Vec<Duration>)> {
        let mut durations = Vec::with_capacity(iterations);
        
        for _ in 0..iterations {
            let start = Instant::now();
            let _ = self.run_instance(instance_id).await?;
            durations.push(start.elapsed());
        }
        
        // Calculate average ops per second
        let total_duration: Duration = durations.iter().sum();
        let ops_per_second = iterations as f64 / total_duration.as_secs_f64();
        
        Ok((ops_per_second, durations))
    }
    
    /// Terminate and remove an instance
    pub async fn terminate_instance(&self, instance_id: &str) -> Result<()> {
        let mut instances = self.instances.lock().unwrap();
        if instances.remove(instance_id).is_some() {
            tracing::info!("Terminated instance {}, remaining instances: {}", instance_id, instances.len());
            Ok(())
        } else {
            anyhow::bail!("Instance not found: {}", instance_id);
        }
    }
    
    /// Get all collected metrics for research
    pub fn get_metrics(&self) -> Vec<PerformanceMetrics> {
        self.metrics.lock().unwrap().clone()
    }
    
    /// Export metrics to a CSV file for analysis
    pub fn export_metrics_to_csv<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let metrics = self.metrics.lock().unwrap();
        if metrics.is_empty() {
            return Ok(());
        }
        
        let mut csv = String::from("module_load_time_ms,module_compile_time_ms,instantiation_time_ms,total_cold_start_time_ms,warm_start_time_ms,execution_time_ms,from_cache\n");
        
        for metric in metrics.iter() {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{}\n",
                metric.module_load_time.as_millis(),
                metric.module_compile_time.as_millis(),
                metric.instantiation_time.as_millis(),
                metric.total_cold_start_time.as_millis(),
                metric.warm_start_time.as_millis(),
                metric.execution_time.as_millis(),
                metric.from_cache as i32
            ));
        }
        
        fs::write(path, csv)?;
        
        Ok(())
    }
}