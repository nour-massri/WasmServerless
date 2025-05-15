use anyhow::{Context, Result};
use clap::Parser;
use image::{GenericImageView, ImageFormat}; // Added GenericImageView trait
use std::fs;
use std::path::Path; // Removed unused PathBuf

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Input directory or file
    #[arg(short, long)]
    input: String,

    /// Output directory
    #[arg(short, long)]
    output: String,

    /// Target width
    #[arg(short, long, default_value = "800")]
    width: u32,

    /// Target height
    #[arg(short, long, default_value = "600")]
    height: u32,

    /// Quality (for JPEG, 1-100)
    #[arg(short, long, default_value = "85")]
    quality: u8,
}

#[no_mangle]
pub extern "C" fn _start() -> i32 {
    println!("Hello from Local Image Resizer Wasm");

    match main_impl() {
        Ok(_) => {
            println!("Local image resizing completed successfully");
            0
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            1
        }
    }
}

fn main_impl() -> Result<()> {
    let args = Args::parse();

    println!("Starting local image resize");
    println!("Input: {}", args.input);
    println!("Output: {}", args.output);
    println!("Target size: {}x{}", args.width, args.height);

    // Create output directory if it doesn't exist
    fs::create_dir_all(&args.output)?;

    let input_path = Path::new(&args.input);
    let mut processed_count = 0;

    if input_path.is_file() {
        process_single_image(input_path, &args)?;
        processed_count = 1;
    } else if input_path.is_dir() {
        processed_count = process_directory(input_path, &args)?;
    } else {
        anyhow::bail!("Input path does not exist: {}", args.input);
    }

    println!("Successfully processed {} images", processed_count);
    Ok(())
}

fn process_directory(input_dir: &Path, args: &Args) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(input_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && is_image_file(&path) {
            match process_single_image(&path, args) {
                Ok(_) => count += 1,
                Err(e) => println!("Warning: Failed to process {:?}: {}", path, e),
            }
        }
    }
    Ok(count)
}

fn process_single_image(input_path: &Path, args: &Args) -> Result<()> {
    println!("Processing: {:?}", input_path);

    // Load image
    let img = image::open(input_path)
        .with_context(|| format!("Failed to open image: {:?}", input_path))?;

    // Get original dimensions
    let (orig_width, orig_height) = img.dimensions();
    println!("Original dimensions: {}x{}", orig_width, orig_height);

    // Resize image
    let resized = img.resize(
        args.width,
        args.height,
        image::imageops::FilterType::Lanczos3,
    );
    let (new_width, new_height) = resized.dimensions();
    println!("Resized dimensions: {}x{}", new_width, new_height);

    // Determine output path
    let filename = input_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Invalid filename"))?;
    let output_path = Path::new(&args.output).join(filename);

    // Determine format and save
    let format = ImageFormat::from_path(input_path)?;
    match format {
        ImageFormat::Jpeg => {
            resized.save_with_format(&output_path, ImageFormat::Jpeg)?;
        }
        ImageFormat::Png => {
            resized.save_with_format(&output_path, ImageFormat::Png)?;
        }
        _ => {
            // Default to JPEG for other formats
            let jpeg_path = output_path.with_extension("jpg");
            resized.save_with_format(&jpeg_path, ImageFormat::Jpeg)?;
        }
    }

    println!("Saved: {:?}", output_path);
    Ok(())
}

fn is_image_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        matches!(
            ext.as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tiff" | "webp"
        )
    } else {
        false
    }
}
