// memory_allocator.rs
use std::env;
use std::mem;
use std::sync::Mutex;
use once_cell::sync::Lazy;

// Use a thread-safe container for our global memory holder
static MEMORY_HOLDER: Lazy<Mutex<Vec<Vec<u8>>>> = Lazy::new(|| Mutex::new(Vec::new()));

fn main() {
    // Parse command line arguments
    let args: Vec<String> = env::args().collect();
    
    // Default allocation size (1MB) if no argument provided
    let size_kb = if args.len() > 1 {
        args[1].parse::<usize>().unwrap_or(1024)
    } else {
        1024 // Default 1MB
    };
    
    // Additional allocation count (default 1) for creating multiple chunks
    let num_allocs = if args.len() > 2 {
        args[2].parse::<usize>().unwrap_or(1)
    } else {
        1
    };
    
    // Calculate bytes to allocate
    let size_bytes = size_kb * 1024;
    
    // Print allocation plan
    println!("Allocating {} KB in {} chunks", size_kb * num_allocs, num_allocs);
    
    // Perform allocations
    for i in 0..num_allocs {
        // Create a vector filled with a repeating pattern to ensure it's actually allocated
        let memory_chunk = (0..size_bytes).map(|j| ((i + j) & 0xFF) as u8).collect::<Vec<u8>>();
        
        // Calculate the actual memory size for verification
        let actual_size = memory_chunk.len() * mem::size_of::<u8>();
        
        // Store the memory in our global holder to prevent it from being freed
        MEMORY_HOLDER.lock().unwrap().push(memory_chunk);
        
        println!("Allocated chunk {}: {} KB", i + 1, actual_size / 1024);
    }
    
    // Calculate total allocation size
    let total_alloc = MEMORY_HOLDER.lock().unwrap().iter().map(|v| v.len()).sum::<usize>() / 1024;
    println!("Total allocated: {} KB", total_alloc);
    
    // Perform some simple operations on memory to ensure it's not optimized away
    let mut sum = 0;
    let memory_holder = MEMORY_HOLDER.lock().unwrap();
    for chunk in memory_holder.iter() {
        for i in 0..std::cmp::min(1000, chunk.len()) {
            sum += chunk[i] as u32;
        }
    }
    
    println!("Memory checksum: {}", sum);
    println!("Memory will be held until program exit");
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> i32 {
    main();
    0
}