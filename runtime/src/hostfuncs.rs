//hostfuncs.rs
use anyhow::Result;
use aws_credential_types::{provider::SharedCredentialsProvider, Credentials};
use aws_sdk_s3::{Client as S3Client, Config};
use aws_config::Region;use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::timeout;
use wasmtime::*;
use wasmtime_wasi::preview1::WasiP1Ctx;
use serde_json::json;
use dirs;
use tracing::{info, warn, error, debug};
use ini::Ini;
#[derive(Clone)]
struct AwsCredentials {
    credentials_provider: SharedCredentialsProvider,
    region: Region,
}  
  fn load_aws_credentials() -> anyhow::Result<AwsCredentials> {
        debug!("Loading AWS credentials");
        
        // Try to load from environment variables first
        if let (Ok(access_key), Ok(secret_key)) = (
            std::env::var("AWS_ACCESS_KEY_ID"),
            std::env::var("AWS_SECRET_ACCESS_KEY"),
        ) {
            info!("Loading AWS credentials from environment variables");
            let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());
            
            let credentials = aws_sdk_s3::config::Credentials::new(
                access_key,
                secret_key,
                std::env::var("AWS_SESSION_TOKEN").ok(),
                None,
                "rust-wasm-container"
            );
            
            return Ok(AwsCredentials {
                credentials_provider: aws_sdk_s3::config::SharedCredentialsProvider::new(credentials),
                region: aws_sdk_s3::config::Region::new(region),
            });
        }

        // Try to load from ~/.aws/credentials
        info!("Loading AWS credentials from ~/.aws files");
        let home_dir = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to find home directory"))?;
        let credentials_path = home_dir.join(".aws").join("credentials");
        let config_path = home_dir.join(".aws").join("config");

        debug!("Credentials path: {:?}", credentials_path);
        debug!("Config path: {:?}", config_path);

        // Load credentials file
        let creds_ini = Ini::load_from_file(&credentials_path)
            .map_err(|e| anyhow::anyhow!("Failed to load ~/.aws/credentials: {}. Please ensure the file exists and is readable.", e))?;

        // Load config file for region
        let config_ini = Ini::load_from_file(&config_path).ok();

        let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".to_string());
        info!("Using AWS profile: {}", profile);

        // Get credentials from profile
        let section = creds_ini.section(Some(&profile))
            .ok_or_else(|| anyhow::anyhow!("Profile '{}' not found in credentials file", profile))?;

        let access_key = section.get("aws_access_key_id")
            .ok_or_else(|| anyhow::anyhow!("aws_access_key_id not found in profile '{}'", profile))?;

        let secret_key = section.get("aws_secret_access_key")
            .ok_or_else(|| anyhow::anyhow!("aws_secret_access_key not found in profile '{}'", profile))?;

        let session_token = section.get("aws_session_token");

        debug!("Found credentials for profile: {}", profile);

        // Get region from config or use default
        let region = if let Some(config_ini) = config_ini {
            let config_section_name = if profile == "default" {
                "default".to_string()
            } else {
                format!("profile {}", profile)
            };
            
            config_ini.section(Some(&config_section_name))
                .and_then(|s| s.get("region"))
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    warn!("No region found in config, using default: us-east-1");
                    "us-east-1".to_string()
                })
        } else {
            warn!("No config file found, using default region: us-east-1");
            "us-east-1".to_string()
        };

        info!("Using AWS region: {}", region);

        let credentials = aws_sdk_s3::config::Credentials::new(
            access_key,
            secret_key,
            session_token.map(|s| s.to_string()),
            None,
            "rust-wasm-container"
        );

        info!("Successfully loaded AWS credentials");

        Ok(AwsCredentials {
            credentials_provider: aws_sdk_s3::config::SharedCredentialsProvider::new(credentials),
            region: aws_sdk_s3::config::Region::new(region),
        })
    }
/// Context for Wasm instance to handle I/O operations
pub struct WasmIOContext {
    ctx: WasiP1Ctx,
    instance_id: String,
    stdout: Mutex<Vec<u8>>,
    stderr: Mutex<Vec<u8>>,
    file_handles: Mutex<HashMap<u32, Box<dyn ReadWriteSync + Send>>>,
    next_fd: Mutex<u32>,
}

/// Trait to handle both Read and Write operations synchronously
pub trait ReadWriteSync: Read + Write {}

impl<T: Read + Write + Send + 'static> ReadWriteSync for T {}

impl WasmIOContext {
    pub fn new(instance_id: String, ctx: WasiP1Ctx) -> Self {
        Self {
            ctx,
            instance_id,
            stdout: Mutex::new(Vec::new()),
            stderr: Mutex::new(Vec::new()),
            file_handles: Mutex::new(HashMap::new()),
            next_fd: Mutex::new(3), // Start after stdin, stdout, stderr
        }
    }
    pub fn wasi_ctx(&mut self) -> &mut WasiP1Ctx {
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
        // HTTP GET Function
        linker.func_wrap_async(
            "env",
            "http_get",
            |mut caller: Caller<'_, WasmIOContext>, params: (u32, i32, u32, i32)| {
                // Destructure parameters
                let (url_ptr, url_len, response_ptr, response_len) = params;

                Box::new(async move {
                    let mem = match caller.get_export("memory") {
                        Some(Extern::Memory(mem)) => mem,
                        _ => return Ok(-1),
                    };

                    // Read URL from memory
                    let mut url_bytes = vec![0; url_len as usize];
                    if mem
                        .read(&mut caller, url_ptr as usize, &mut url_bytes)
                        .is_err()
                    {
                        return Ok(-1);
                    }

                    let url = match std::str::from_utf8(&url_bytes) {
                        Ok(u) => u,
                        Err(_) => return Ok(-1),
                    };

                    // Make HTTP request directly with async/await
                    let client = reqwest::Client::new();
                    let response =
                        match timeout(Duration::from_secs(10), client.get(url).send()).await {
                            Ok(Ok(resp)) => match resp.text().await {
                                Ok(text) => text,
                                Err(_) => return Ok(-2),
                            },
                            Ok(Err(_)) => return Ok(-3),
                            Err(_) => return Ok(-4), // Timeout
                        };

                    let response_bytes = response.into_bytes();
                    let copy_len = std::cmp::min(response_bytes.len(), response_len as usize);

                    if mem
                        .write(
                            &mut caller,
                            response_ptr as usize,
                            &response_bytes[..copy_len],
                        )
                        .is_err()
                    {
                        return Ok(-1);
                    }

                    Ok(copy_len as i32)
                })
            },
        )?;

        // HTTP POST Function
        linker.func_wrap_async(
            "env",
            "http_post",
            |mut caller: Caller<'_, WasmIOContext>, params: (u32, i32, u32, i32, u32, i32)| {
                // Destructure parameters
                let (url_ptr, url_len, body_ptr, body_len, response_ptr, response_len) = params;

                Box::new(async move {
                    let mem = match caller.get_export("memory") {
                        Some(Extern::Memory(mem)) => mem,
                        _ => return Ok(-1),
                    };

                    // Read URL from memory
                    let mut url_bytes = vec![0; url_len as usize];
                    if mem
                        .read(&mut caller, url_ptr as usize, &mut url_bytes)
                        .is_err()
                    {
                        return Ok(-1);
                    }

                    let url = match std::str::from_utf8(&url_bytes) {
                        Ok(u) => u,
                        Err(_) => return Ok(-1),
                    };

                    // Read body from memory
                    let mut body_bytes = vec![0; body_len as usize];
                    if mem
                        .read(&mut caller, body_ptr as usize, &mut body_bytes)
                        .is_err()
                    {
                        return Ok(-1);
                    }

                    let body = match std::str::from_utf8(&body_bytes) {
                        Ok(b) => b,
                        Err(_) => return Ok(-1),
                    };

                    // Make HTTP request directly with async/await
                    let client = reqwest::Client::new();
                    let response = match timeout(
                        Duration::from_secs(10),
                        client
                            .post(url)
                            .header("Content-Type", "application/json")
                            .body(body.to_string())
                            .send(),
                    )
                    .await
                    {
                        Ok(Ok(resp)) => match resp.text().await {
                            Ok(text) => text,
                            Err(_) => return Ok(-2),
                        },
                        Ok(Err(_)) => return Ok(-3),
                        Err(_) => return Ok(-4), // Timeout
                    };

                    let response_bytes = response.into_bytes();
                    let copy_len = std::cmp::min(response_bytes.len(), response_len as usize);

                    if mem
                        .write(
                            &mut caller,
                            response_ptr as usize,
                            &response_bytes[..copy_len],
                        )
                        .is_err()
                    {
                        return Ok(-1);
                    }

                    Ok(copy_len as i32)
                })
            },
        )?;

         let aws_credentials = load_aws_credentials()?;

       let aws_credentials = match load_aws_credentials() {
            Ok(creds) => {
                info!("Successfully loaded AWS credentials");
                creds
            }
            Err(e) => {
                error!("Failed to load AWS credentials: {}", e);
                return Err(e);
            }
        };

        // S3 List Objects function with better error handling
        let creds_for_list = aws_credentials.clone();
        linker.func_wrap_async(
            "env",
            "s3_list_objects",
            move |mut caller: Caller<'_, WasmIOContext>, 
                  params: (u32, i32, u32, i32, i32, u32, i32, u32, i32)| {
                let (bucket_ptr, bucket_len, prefix_ptr, prefix_len, max_keys,
                     start_after_ptr, start_after_len, response_ptr, response_len) = params;
                
                let credentials = creds_for_list.clone();
                
                Box::new(async move {
                    debug!("Starting S3 list objects operation");
                    
                    let mem = match caller.get_export("memory") {
                        Some(Extern::Memory(mem)) => mem,
                        _ => {
                            error!("Failed to get memory export");
                            return Ok(-1);
                        }
                    };

                    // Read parameters from memory with error handling
                    let bucket = match read_string_from_memory(&mem, &mut caller, bucket_ptr, bucket_len) {
                        Ok(b) => {
                            debug!("Bucket: {}", b);
                            b
                        }
                        Err(e) => {
                            error!("Failed to read bucket from memory: {}", e);
                            return Ok(-1);
                        }
                    };
                    
                    let prefix = match read_string_from_memory(&mem, &mut caller, prefix_ptr, prefix_len) {
                        Ok(p) => {
                            debug!("Prefix: {}", p);
                            p
                        }
                        Err(e) => {
                            error!("Failed to read prefix from memory: {}", e);
                            return Ok(-1);
                        }
                    };
                    
                    let start_after = if start_after_len > 0 {
                        match read_string_from_memory(&mem, &mut caller, start_after_ptr, start_after_len) {
                            Ok(s) => {
                                debug!("Start after: {}", s);
                                Some(s)
                            }
                            Err(e) => {
                                error!("Failed to read start_after from memory: {}", e);
                                return Ok(-1);
                            }
                        }
                    } else {
                        None
                    };

                    debug!("Creating S3 client with region: {:?}", credentials.region);

                    // Create S3 client with explicit behavior version
                    let config = aws_sdk_s3::Config::builder()
                        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                        .region(credentials.region.clone())
                        .credentials_provider(credentials.credentials_provider.clone())
                        .build();
                    
                    let client = aws_sdk_s3::Client::from_conf(config);

                    debug!("Building list objects request");

                    // Build list request
                    let mut request = client
                        .list_objects_v2()
                        .bucket(&bucket)
                        .prefix(&prefix)
                        .max_keys(max_keys);
                    
                    if let Some(start_after) = start_after {
                        request = request.start_after(start_after);
                    }

                    debug!("Sending list objects request");

                    // List objects
                    let resp = match request.send().await {
                        Ok(resp) => {
                            debug!("Successfully received S3 list response");
                            resp
                        }
                        Err(e) => {
                            error!("S3 list error: {:?}", e);
                            
                            // Return different error codes for different error types
                            match e {
                                aws_sdk_s3::error::SdkError::ServiceError(service_err) => {
                                    match service_err.err() {
                                        aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error::NoSuchBucket(_) => return Ok(-3),
                                        _ => return Ok(-2),
                                    }
                                }
                                aws_sdk_s3::error::SdkError::ConstructionFailure(_) => return Ok(-4),
                                aws_sdk_s3::error::SdkError::DispatchFailure(_) => return Ok(-5),
                                aws_sdk_s3::error::SdkError::TimeoutError(_) => return Ok(-6),
                                _ => return Ok(-2),
                            }
                        }
                    };

                    debug!("Building response JSON");

                    // Build response JSON
                    let mut contents = Vec::new();
                    let mut total_objects = 0;
                    
                    if let objects = resp.contents() {
                        for object in objects {
                            total_objects += 1;
                            if let Some(key) = object.key() {
                                contents.push(json!({
                                    "Key": key,
                                    "Size": object.size().unwrap_or(0),
                                    "LastModified": object.last_modified().map(|t| t.to_string()).unwrap_or_default()
                                }));
                            }
                        }
                    }

                    debug!("Found {} objects", total_objects);

                    let response = json!({
                        "Contents": contents,
                        "IsTruncated": resp.is_truncated().unwrap_or(false),
                        "NextContinuationToken": resp.next_continuation_token(),
                        "KeyCount": resp.key_count().unwrap_or(0),
                        "MaxKeys": resp.max_keys().unwrap_or(0)
                    });

                    let response_str = response.to_string();
                    let response_bytes = response_str.as_bytes();
                    let copy_len = std::cmp::min(response_bytes.len(), response_len as usize);

                    debug!("Writing {} bytes to response buffer", copy_len);

                    if mem.write(&mut caller, response_ptr as usize, &response_bytes[..copy_len]).is_err() {
                        error!("Failed to write response to memory");
                        return Ok(-1);
                    }

                    debug!("S3 list operation completed successfully");
                    Ok(copy_len as i32)
                })
            },
        )?;

        // S3 Get Object function with better error handling
        let creds_for_get = aws_credentials.clone();
        linker.func_wrap_async(
            "env",
            "s3_get_object",
            move |mut caller: Caller<'_, WasmIOContext>,
                  params: (u32, i32, u32, i32, u32, i32)| {
                let (bucket_ptr, bucket_len, key_ptr, key_len, data_ptr, data_len) = params;
                let credentials = creds_for_get.clone();

                Box::new(async move {
                    debug!("Starting S3 get object operation");
                    
                    let mem = match caller.get_export("memory") {
                        Some(Extern::Memory(mem)) => mem,
                        _ => {
                            error!("Failed to get memory export");
                            return Ok(-1);
                        }
                    };

                    // Read parameters
                    let bucket = match read_string_from_memory(&mem, &mut caller, bucket_ptr, bucket_len) {
                        Ok(b) => {
                            debug!("Get object from bucket: {}", b);
                            b
                        }
                        Err(e) => {
                            error!("Failed to read bucket: {}", e);
                            return Ok(-1);
                        }
                    };
                    
                    let key = match read_string_from_memory(&mem, &mut caller, key_ptr, key_len) {
                        Ok(k) => {
                            debug!("Get object key: {}", k);
                            k
                        }
                        Err(e) => {
                            error!("Failed to read key: {}", e);
                            return Ok(-1);
                        }
                    };

                    // Create S3 client with explicit behavior version
                    let config = aws_sdk_s3::Config::builder()
                        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                        .region(credentials.region.clone())
                        .credentials_provider(credentials.credentials_provider.clone())
                        .build();

                    let client = aws_sdk_s3::Client::from_conf(config);

                    debug!("Sending get object request");

                    // Get object
                    let resp = match client.get_object().bucket(&bucket).key(&key).send().await {
                        Ok(resp) => {
                            debug!("Successfully received object");
                            resp
                        }
                        Err(e) => {
                            error!("S3 get error: {:?}", e);
                            match e {
                                aws_sdk_s3::error::SdkError::ServiceError(service_err) => {
                                return Ok(-2);
                                }
                                _ => return Ok(-2),
                            }
                        }
                    };

                    debug!("Reading object body");

                    // Read body
                    let body = match resp.body.collect().await {
                        Ok(data) => {
                            let bytes = data.into_bytes();
                            debug!("Successfully read {} bytes", bytes.len());
                            bytes
                        }
                        Err(e) => {
                            error!("Failed to read object body: {:?}", e);
                            return Ok(-5);
                        }
                    };

                    let copy_len = std::cmp::min(body.len(), data_len as usize);
                    debug!("Writing {} bytes to memory", copy_len);
                    
                    if mem.write(&mut caller, data_ptr as usize, &body[..copy_len]).is_err() {
                        error!("Failed to write object data to memory");
                        return Ok(-1);
                    }

                    debug!("S3 get operation completed successfully");
                    Ok(copy_len as i32)
                })
            },
        )?;

        // S3 Put Object function with better error handling
        let creds_for_put = aws_credentials.clone();
        linker.func_wrap_async(
            "env",
            "s3_put_object",
            move |mut caller: Caller<'_, WasmIOContext>,
                  params: (u32, i32, u32, i32, u32, i32)| {
                let (bucket_ptr, bucket_len, key_ptr, key_len, data_ptr, data_len) = params;
                let credentials = creds_for_put.clone();

                Box::new(async move {
                    debug!("Starting S3 put object operation");
                    
                    let mem = match caller.get_export("memory") {
                        Some(Extern::Memory(mem)) => mem,
                        _ => {
                            error!("Failed to get memory export");
                            return Ok(-1);
                        }
                    };

                    // Read parameters
                    let bucket = match read_string_from_memory(&mem, &mut caller, bucket_ptr, bucket_len) {
                        Ok(b) => {
                            debug!("Put object to bucket: {}", b);
                            b
                        }
                        Err(e) => {
                            error!("Failed to read bucket: {}", e);
                            return Ok(-1);
                        }
                    };
                    
                    let key = match read_string_from_memory(&mem, &mut caller, key_ptr, key_len) {
                        Ok(k) => {
                            debug!("Put object key: {}", k);
                            k
                        }
                        Err(e) => {
                            error!("Failed to read key: {}", e);
                            return Ok(-1);
                        }
                    };

                    // Read data
                    let mut data = vec![0u8; data_len as usize];
                    if mem.read(&mut caller, data_ptr as usize, &mut data).is_err() {
                        error!("Failed to read data from memory");
                        return Ok(-1);
                    }

                    debug!("Read {} bytes of data", data.len());

                    // Create S3 client with explicit behavior version
                    let config = aws_sdk_s3::Config::builder()
                        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                        .region(credentials.region.clone())
                        .credentials_provider(credentials.credentials_provider.clone())
                        .build();

                    let client = aws_sdk_s3::Client::from_conf(config);

                    debug!("Sending put object request");

                    // Put object
                    let body = aws_sdk_s3::primitives::ByteStream::from(data);
                    match client
                        .put_object()
                        .bucket(&bucket)
                        .key(&key)
                        .body(body)
                        .content_type("image/jpeg")
                        .send()
                        .await
                    {
                        Ok(_) => {
                            debug!("Successfully uploaded object");
                            Ok(0)
                        }
                        Err(e) => {
                            error!("S3 put error: {:?}", e);
                            match e {
                                aws_sdk_s3::error::SdkError::ServiceError(service_err) => {
                                return Ok(-2);
                                }
                                _ => return Ok(-2),
                            }
                        }
                    }
                })
            },
        )?;

        Ok(())
    }
}

// Helper function to read strings from WASM memory with better error handling
fn read_string_from_memory(
    mem: &wasmtime::Memory,
    caller: &mut wasmtime::Caller<WasmIOContext>,
    ptr: u32,
    len: i32,
) -> anyhow::Result<String> {
    if len < 0 {
        anyhow::bail!("Invalid string length: {}", len);
    }
    
    let mut bytes = vec![0u8; len as usize];
    mem.read(caller, ptr as usize, &mut bytes)
        .map_err(|e| anyhow::anyhow!("Failed to read memory: {}", e))?;
    
    // Handle potential NUL termination
    if let Some(null_pos) = bytes.iter().position(|&b| b == 0) {
        bytes.truncate(null_pos);
    }
    
    String::from_utf8(bytes)
        .map_err(|e| anyhow::anyhow!("Invalid UTF-8 string: {}", e))
}