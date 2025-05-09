// Updated module_cache.rs
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::Instant;
use wasmtime::Module;

/// Enhanced ModuleData to store additional information
#[derive(Clone)]
pub struct ModuleData {
    pub module: Module,
    pub creation_time: Instant,
    pub size_bytes: usize,
}

/// Cache for compiled Wasm modules to avoid recompilation
pub struct ModuleCache {
    modules: Mutex<HashMap<u64, ModuleData>>,
}

impl ModuleCache {
    pub fn new() -> Self {
        Self {
            modules: Mutex::new(HashMap::new()),
        }
    }

    /// Get a module from the cache
    pub fn get(&self, wasm_bytes: &[u8]) -> Option<Module> {
        let hash = Self::hash_wasm(wasm_bytes);
        self.modules
            .lock()
            .unwrap()
            .get(&hash)
            .map(|data| data.module.clone())
    }

    /// Insert a module data into the cache
    pub fn insert(&self, wasm_bytes: Vec<u8>, module_data: ModuleData) {
        let hash = Self::hash_wasm(&wasm_bytes);
        self.modules.lock().unwrap().insert(hash, module_data);
    }

    /// Get all cached modules with their data
    pub fn get_all(&self) -> HashMap<u64, ModuleData> {
        self.modules.lock().unwrap().clone()
    }

    /// Count the number of entries in the cache
    pub fn len(&self) -> usize {
        self.modules.lock().unwrap().len()
    }

    /// Clear the cache
    pub fn clear(&self) {
        self.modules.lock().unwrap().clear();
    }

    /// Hash the Wasm bytes for cache lookup
    fn hash_wasm(wasm_bytes: &[u8]) -> u64 {
        let mut hasher = DefaultHasher::new();
        wasm_bytes.hash(&mut hasher);
        hasher.finish()
    }
}
