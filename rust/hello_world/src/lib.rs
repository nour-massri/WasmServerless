#[no_mangle]
pub extern "C" fn _start() -> i32 {
    println!("Hello from Wasm");
    0
}