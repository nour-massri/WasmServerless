use anyhow::Result;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use wasmtime::*;
use wasmtime_wasi::{WasiCtx};

/// Context for Wasm instance to handle I/O operations
pub struct WasmIOContext {
    ctx: WasiCtx,
    instance_id: String,
    env_vars: HashMap<String, String>,
    stdout: Mutex<Vec<u8>>,
    stderr: Mutex<Vec<u8>>,
    file_handles: Mutex<HashMap<u32, Box<dyn ReadWriteSync + Send>>>,
    next_fd: Mutex<u32>,
}

/// Trait to handle both Read and Write operations synchronously
pub trait ReadWriteSync: Read + Write {}

impl<T: Read + Write + Send + 'static> ReadWriteSync for T {}

impl WasmIOContext {

    pub fn new(instance_id: String, env_vars: HashMap<String, String>, ctx: WasiCtx) -> Self {
        Self {
            ctx,
            instance_id,
            env_vars,
            stdout: Mutex::new(Vec::new()),
            stderr: Mutex::new(Vec::new()),
            file_handles: Mutex::new(HashMap::new()),
            next_fd: Mutex::new(3), // Start after stdin, stdout, stderr
        }
    }    
    pub fn wasi_ctx(&mut self) -> &mut WasiCtx {
        &mut self.ctx
    }
    /// Get the next available file descriptor
    fn next_fd(&self) -> u32 {
        let mut fd = self.next_fd.lock().unwrap();
        let current = *fd;
        *fd += 1;
        current
    }

    /// Write to stdout
    pub fn write_stdout(&self, bytes: &[u8]) -> Result<usize> {
        let mut stdout = self.stdout.lock().unwrap();
        stdout.write_all(bytes)?;
        tracing::debug!(
            "[{}] stdout: {}",
            self.instance_id,
            String::from_utf8_lossy(bytes)
        );
        Ok(bytes.len())
    }

    /// Write to stderr
    pub fn write_stderr(&self, bytes: &[u8]) -> Result<usize> {
        let mut stderr = self.stderr.lock().unwrap();
        stderr.write_all(bytes)?;
        tracing::debug!(
            "[{}] stderr: {}",
            self.instance_id,
            String::from_utf8_lossy(bytes)
        );
        Ok(bytes.len())
    }

    /// Get environment variable
    pub fn get_env(&self, name: &str) -> Option<String> {
        self.env_vars.get(name).cloned()
    }

    /// Open a file
    pub fn open_file(&self, path: &str, write: bool, append: bool) -> Result<u32> {
        // For security, we'll only allow files in a specific directory
        // In a real implementation, you'd want more robust sandboxing
        let safe_path = format!("/tmp/wasm-server/{}/{}", self.instance_id, path);

        // Ensure directory exists
        if let Some(parent) = Path::new(&safe_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .append(append)
            .create(write || append)
            .open(&safe_path)?;

        let fd = self.next_fd();
        self.file_handles.lock().unwrap().insert(fd, Box::new(file));

        tracing::debug!(
            "[{}] Opened file: {} (fd: {})",
            self.instance_id,
            safe_path,
            fd
        );
        Ok(fd)
    }

    /// Read from a file descriptor
    pub fn read_file(&self, fd: u32, buf: &mut [u8]) -> Result<usize> {
        match fd {
            0 => Ok(0), // stdin - not implemented
            _ => {
                let mut handles = self.file_handles.lock().unwrap();
                if let Some(file) = handles.get_mut(&fd) {
                    let n = file.read(buf)?;
                    Ok(n)
                } else {
                    anyhow::bail!("Invalid file descriptor: {}", fd)
                }
            }
        }
    }

    /// Write to a file descriptor
    pub fn write_file(&self, fd: u32, buf: &[u8]) -> Result<usize> {
        match fd {
            1 => self.write_stdout(buf),
            2 => self.write_stderr(buf),
            _ => {
                let mut handles = self.file_handles.lock().unwrap();
                if let Some(file) = handles.get_mut(&fd) {
                    let n = file.write(buf)?;
                    file.flush()?;
                    Ok(n)
                } else {
                    anyhow::bail!("Invalid file descriptor: {}", fd)
                }
            }
        }
    }

    /// Close a file descriptor
    pub fn close_file(&self, fd: u32) -> Result<()> {
        if fd > 2 {
            let mut handles = self.file_handles.lock().unwrap();
            if handles.remove(&fd).is_some() {
                tracing::debug!("[{}] Closed fd: {}", self.instance_id, fd);
                Ok(())
            } else {
                anyhow::bail!("Invalid file descriptor: {}", fd)
            }
        } else {
            // Can't close stdin/stdout/stderr
            anyhow::bail!("Cannot close standard file descriptor: {}", fd)
        }
    }
}
pub struct HostFunctions;

impl HostFunctions {
    pub fn register(linker: &mut Linker<WasmIOContext>) -> Result<()> {
        // Environment variables
        linker.func_wrap(
            "env",
            "get_env",
            |mut caller: Caller<'_, WasmIOContext>,
             name_ptr: i32,
             name_len: i32,
             value_ptr: i32,
             value_len: i32|
             -> i32 {
                let mem = match caller.get_export("memory") {
                    Some(Extern::Memory(mem)) => mem,
                    _ => return -1,
                };

                // Read the environment variable name
                let mut name_bytes = vec![0; name_len as usize];
                if mem
                    .read(&mut caller, name_ptr as usize, &mut name_bytes)
                    .is_err()
                {
                    return -1;
                }

                let name = match std::str::from_utf8(&name_bytes) {
                    Ok(n) => n,
                    Err(_) => return -1,
                };

                // Get the environment variable
                let value = match caller.data().get_env(name) {
                    Some(v) => v,
                    None => return 0, // Not found
                };

                let value_bytes = value.as_bytes();
                if value_bytes.len() > value_len as usize {
                    return -(value_bytes.len() as i32); // Buffer too small, return required size as negative
                }

                // Write the value to memory
                if mem
                    .write(&mut caller, value_ptr as usize, value_bytes)
                    .is_err()
                {
                    return -1;
                }

                value_bytes.len() as i32
            },
        )?;

        // File operations
        linker.func_wrap(
            "env",
            "open_file",
            |mut caller: Caller<'_, WasmIOContext>,
             path_ptr: i32,
             path_len: i32,
             write: i32,
             append: i32|
             -> i32 {
                let mem = match caller.get_export("memory") {
                    Some(Extern::Memory(mem)) => mem,
                    _ => return -1,
                };

                // Read the path
                let mut path_bytes = vec![0; path_len as usize];
                if mem
                    .read(&mut caller, path_ptr as usize, &mut path_bytes)
                    .is_err()
                {
                    return -1;
                }

                let path = match std::str::from_utf8(&path_bytes) {
                    Ok(p) => p,
                    Err(_) => return -1,
                };

                // Open the file
                match caller.data().open_file(path, write != 0, append != 0) {
                    Ok(fd) => fd as i32,
                    Err(_) => -1,
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "read_file",
            |mut caller: Caller<'_, WasmIOContext>, fd: i32, buf_ptr: i32, buf_len: i32| -> i32 {
                let mem = match caller.get_export("memory") {
                    Some(Extern::Memory(mem)) => mem,
                    _ => return -1,
                };

                // Prepare buffer for reading
                let mut buf = vec![0; buf_len as usize];

                // Read from the file
                let n = match caller.data().read_file(fd as u32, &mut buf) {
                    Ok(n) => n,
                    Err(_) => return -1,
                };

                // Write to memory
                if mem.write(&mut caller, buf_ptr as usize, &buf[..n]).is_err() {
                    return -1;
                }

                n as i32
            },
        )?;

        linker.func_wrap(
            "env",
            "write_file",
            |mut caller: Caller<'_, WasmIOContext>, fd: i32, buf_ptr: i32, buf_len: i32| -> i32 {
                let mem = match caller.get_export("memory") {
                    Some(Extern::Memory(mem)) => mem,
                    _ => return -1,
                };

                // Read data from memory
                let mut buf = vec![0; buf_len as usize];
                if mem.read(&mut caller, buf_ptr as usize, &mut buf).is_err() {
                    return -1;
                }

                // Write to the file
                match caller.data().write_file(fd as u32, &buf) {
                    Ok(n) => n as i32,
                    Err(_) => -1,
                }
            },
        )?;

        linker.func_wrap(
            "env",
            "close_file",
            |caller: Caller<'_, WasmIOContext>, fd: i32| -> i32 {
                match caller.data().close_file(fd as u32) {
                    Ok(_) => 0,
                    Err(_) => -1,
                }
            },
        )?;

        // Logging functions
        linker.func_wrap(
            "env",
            "log",
            |mut caller: Caller<'_, WasmIOContext>,
             level: i32,
             msg_ptr: i32,
             msg_len: i32|
             -> i32 {
                let mem = match caller.get_export("memory") {
                    Some(Extern::Memory(mem)) => mem,
                    _ => return -1,
                };

                // Read message from memory
                let mut msg_bytes = vec![0; msg_len as usize];
                if mem
                    .read(&mut caller, msg_ptr as usize, &mut msg_bytes)
                    .is_err()
                {
                    return -1;
                }

                let msg = match std::str::from_utf8(&msg_bytes) {
                    Ok(m) => m,
                    Err(_) => return -1,
                };

                // Log based on level
                match level {
                    1 => tracing::error!("[{}] {}", caller.data().instance_id, msg),
                    2 => tracing::warn!("[{}] {}", caller.data().instance_id, msg),
                    3 => tracing::info!("[{}] {}", caller.data().instance_id, msg),
                    4 => tracing::debug!("[{}] {}", caller.data().instance_id, msg),
                    _ => tracing::trace!("[{}] {}", caller.data().instance_id, msg),
                }

                0
            },
        )?;

        Ok(())
    }
}
