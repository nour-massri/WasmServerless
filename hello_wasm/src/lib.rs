// Add this to your lib.rs
#[no_mangle]
pub extern "C" fn _start() -> i32 {
    println!("from main");
    0
}