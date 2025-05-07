use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use wasmtime::Module;

/// Cache for compiled Wasm modules to avoid recompilation
pub struct ModuleCache {
    modules: Mutex<HashMap<u64, Module>>,
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
        self.modules.lock().unwrap().get(&hash).cloned()
    }

    /// Insert a module into the cache
    pub fn insert(&self, wasm_bytes: Vec<u8>, module: Module) {
        let hash = Self::hash_wasm(&wasm_bytes);
        self.modules.lock().unwrap().insert(hash, module);
    }

    /// Hash the Wasm bytes for cache lookup
    fn hash_wasm(wasm_bytes: &[u8]) -> u64 {
        let mut hasher = DefaultHasher::new();
        wasm_bytes.hash(&mut hasher);
        hasher.finish()
    }
}
