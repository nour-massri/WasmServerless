use anyhow::{Context, Result};
use image::{GenericImageView, ImageOutputFormat};
use serde_json;
use std::env;
use std::io::Cursor;

// External host functions for S3 operations (no credentials needed)
extern "C" {
    fn s3_list_objects(
        bucket_ptr: *const u8,
        bucket_len: i32,
        prefix_ptr: *const u8,
        prefix_len: i32,
        max_keys: i32,
        start_after_ptr: *const u8,
        start_after_len: i32,
        response_ptr: *mut u8,
        response_len: i32,
    ) -> i32;

    fn s3_get_object(
        bucket_ptr: *const u8,
        bucket_len: i32,
        key_ptr: *const u8,
        key_len: i32,
        data_ptr: *mut u8,
        data_len: i32,
    ) -> i32;

    fn s3_put_object(
        bucket_ptr: *const u8,
        bucket_len: i32,
        key_ptr: *const u8,
        key_len: i32,
        data_ptr: *const u8,
        data_len: i32,
    ) -> i32;
}

struct Args {
    bucket: String,
    input_prefix: String,
    output_prefix: String,
    width: u32,
    height: u32,
    max_keys: i32,
    start_after: Option<String>,
}

impl Args {
    fn parse() -> Result<Self> {
        let args: Vec<String> = env::args().collect();

        let mut bucket = String::new();
        let mut input_prefix = String::new();
        let mut output_prefix = String::new();
        let mut width = 800u32;
        let mut height = 600u32;
        let mut max_keys = 100i32;
        let mut start_after: Option<String> = None;

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--bucket" | "-b" => {
                    i += 1;
                    if i < args.len() {
                        bucket = args[i].clone();
                    }
                }
                "--input-prefix" | "-i" => {
                    i += 1;
                    if i < args.len() {
                        input_prefix = args[i].clone();
                    }
                }
                "--output-prefix" | "-o" => {
                    i += 1;
                    if i < args.len() {
                        output_prefix = args[i].clone();
                    }
                }
                "--width" | "-w" => {
                    i += 1;
                    if i < args.len() {
                        width = args[i].parse().unwrap_or(800);
                    }
                }
                "--height" | "-h" => {
                    i += 1;
                    if i < args.len() {
                        height = args[i].parse().unwrap_or(600);
                    }
                }
                "--max-keys" => {
                    i += 1;
                    if i < args.len() {
                        max_keys = args[i].parse().unwrap_or(100);
                    }
                }
                "--start-after" => {
                    i += 1;
                    if i < args.len() {
                        start_after = Some(args[i].clone());
                    }
                }
                _ => {}
            }
            i += 1;
        }

        Ok(Args {
            bucket,
            input_prefix,
            output_prefix,
            width,
            height,
            max_keys,
            start_after,
        })
    }
}

#[no_mangle]
pub extern "C" fn _start() -> i32 {
    println!("Hello from S3 Image Resizer Wasm");

    match main_impl() {
        Ok(_) => {
            println!("S3 image resizing completed successfully");
            0
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            1
        }
    }
}

fn main_impl() -> Result<()> {
    let args = Args::parse()?;

    println!("Starting S3 image resize");
    println!("Bucket: {}", args.bucket);
    println!("Input prefix: {}", args.input_prefix);
    println!("Output prefix: {}", args.output_prefix);
    println!("Target size: {}x{}", args.width, args.height);
    println!("Max keys: {}", args.max_keys);
    if let Some(ref start_after) = args.start_after {
        println!("Start after: {}", start_after);
    }

    // List objects in S3 bucket with the given prefix
    let object_keys = list_s3_objects(&args)?;
    println!("Found {} objects to process", object_keys.len());

    // Process each image
    let mut processed_count = 0;
    for key in object_keys {
        if is_image_key(&key) {
            match process_s3_image(&key, &args) {
                Ok(_) => processed_count += 1,
                Err(e) => println!("Warning: Failed to process {}: {}", key, e),
            }
        }
    }

    println!("Successfully processed {} images", processed_count);
    Ok(())
}

fn list_s3_objects(args: &Args) -> Result<Vec<String>> {
    println!("Listing S3 objects with prefix: {}", args.input_prefix);

    let bucket = args.bucket.as_bytes();
    let prefix = args.input_prefix.as_bytes();
    let start_after = args
        .start_after
        .as_ref()
        .map(|s| s.as_bytes())
        .unwrap_or(&[]);

    // Allocate buffer for response
    let mut response_buf = vec![0u8; 1024 * 1024]; // 1MB buffer

    let result = unsafe {
        s3_list_objects(
            bucket.as_ptr(),
            bucket.len() as i32,
            prefix.as_ptr(),
            prefix.len() as i32,
            args.max_keys,
            start_after.as_ptr(),
            start_after.len() as i32,
            response_buf.as_mut_ptr(),
            response_buf.len() as i32,
        )
    };

    if result < 0 {
        anyhow::bail!("Failed to list S3 objects: error code {}", result);
    }

    // Parse response (assuming JSON format)
    let response_str = std::str::from_utf8(&response_buf[..result as usize])?;
    println!("S3 list response: {}", response_str);

    let response_json: serde_json::Value = serde_json::from_str(response_str)?;

    let mut keys = Vec::new();
    if let Some(contents) = response_json["Contents"].as_array() {
        for item in contents {
            if let Some(key) = item["Key"].as_str() {
                keys.push(key.to_string());
            }
        }
    }

    Ok(keys)
}

fn process_s3_image(key: &str, args: &Args) -> Result<()> {
    println!("Processing S3 object: {}", key);

    // Download image from S3
    let image_data = download_from_s3(key, args)?;
    println!("Downloaded {} bytes from S3", image_data.len());

    // Load and resize image
    let img = image::load_from_memory(&image_data)
        .with_context(|| format!("Failed to load image from S3: {}", key))?;

    let (orig_width, orig_height) = img.dimensions();
    println!("Original dimensions: {}x{}", orig_width, orig_height);

    let resized = img.resize(
        args.width,
        args.height,
        image::imageops::FilterType::Lanczos3,
    );
    let (new_width, new_height) = resized.dimensions();
    println!("Resized dimensions: {}x{}", new_width, new_height);

    // Convert to bytes
    let mut output_buf = Vec::new();
    resized.write_to(
        &mut Cursor::new(&mut output_buf),
        ImageOutputFormat::Jpeg(85),
    )?;
    println!("Encoded {} bytes", output_buf.len());

    // Generate output key
    let filename = key.split('/').last().unwrap_or(key);
    let output_key = if args.output_prefix.ends_with('/') {
        format!("{}{}", args.output_prefix, filename)
    } else {
        format!("{}/{}", args.output_prefix, filename)
    };

    // Upload to S3
    upload_to_s3(&output_key, &output_buf, args)?;

    println!("Uploaded resized image: {}", output_key);
    Ok(())
}

fn download_from_s3(key: &str, args: &Args) -> Result<Vec<u8>> {
    println!("Downloading from S3: {}", key);

    let bucket = args.bucket.as_bytes();
    let key_bytes = key.as_bytes();

    // Allocate buffer for image data (assuming max 10MB per image)
    let mut data_buf = vec![0u8; 10 * 1024 * 1024];

    let result = unsafe {
        s3_get_object(
            bucket.as_ptr(),
            bucket.len() as i32,
            key_bytes.as_ptr(),
            key_bytes.len() as i32,
            data_buf.as_mut_ptr(),
            data_buf.len() as i32,
        )
    };

    if result < 0 {
        anyhow::bail!("Failed to download from S3: error code {}", result);
    }

    data_buf.truncate(result as usize);
    Ok(data_buf)
}

fn upload_to_s3(key: &str, data: &[u8], args: &Args) -> Result<()> {
    println!("Uploading to S3: {} ({} bytes)", key, data.len());

    let bucket = args.bucket.as_bytes();
    let key_bytes = key.as_bytes();

    let result = unsafe {
        s3_put_object(
            bucket.as_ptr(),
            bucket.len() as i32,
            key_bytes.as_ptr(),
            key_bytes.len() as i32,
            data.as_ptr(),
            data.len() as i32,
        )
    };

    if result < 0 {
        anyhow::bail!("Failed to upload to S3: error code {}", result);
    }

    println!("Successfully uploaded to S3");
    Ok(())
}

fn is_image_key(key: &str) -> bool {
    let key_lower = key.to_lowercase();
    key_lower.ends_with(".jpg")
        || key_lower.ends_with(".jpeg")
        || key_lower.ends_with(".png")
        || key_lower.ends_with(".gif")
        || key_lower.ends_with(".bmp")
        || key_lower.ends_with(".tiff")
        || key_lower.ends_with(".webp")
}
