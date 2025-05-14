use std::env;

extern "C" {
    fn http_get(url_ptr: *const u8, url_len: i32, response_ptr: *mut u8, response_len: i32) -> i32;
    fn http_post(url_ptr: *const u8, url_len: i32, body_ptr: *const u8, body_len: i32, response_ptr: *mut u8, response_len: i32) -> i32;
}

fn log_info(msg: &str) {
    println!("{}", msg);

}

fn http_get_request(url: &str) -> Result<String, i32> {
    let mut response_buffer = vec![0u8; 4096];
    let result = unsafe {
        http_get(
            url.as_ptr(),
            url.len() as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    };
    
    if result < 0 {
        Err(result)
    } else {
        response_buffer.truncate(result as usize);
        Ok(String::from_utf8_lossy(&response_buffer).to_string())
    }
}

fn http_post_request(url: &str, body: &str) -> Result<String, i32> {
    let mut response_buffer = vec![0u8; 4096];
    let result = unsafe {
        http_post(
            url.as_ptr(),
            url.len() as i32,
            body.as_ptr(),
            body.len() as i32,
            response_buffer.as_mut_ptr(),
            response_buffer.len() as i32,
        )
    };
    
    if result < 0 {
        Err(result)
    } else {
        response_buffer.truncate(result as usize);
        Ok(String::from_utf8_lossy(&response_buffer).to_string())
    }
}

// Simulate API aggregation workload
fn aggregate_apis(num_requests: usize) -> (usize, usize) {
    let mut successful_gets = 0;
    let mut successful_posts = 0;
    
    let test_urls = vec![
        "http://httpbin.org/delay/1",
        "http://httpbin.org/json",
        "http://httpbin.org/headers",
        "http://httpbin.org/user-agent",
        "http://httpbin.org/ip",
    ];
    
    log_info(&format!("Starting API aggregation with {} requests", num_requests));
    
    for i in 0..num_requests {
        let url_idx = i % test_urls.len();
        
        // Alternate between GET and POST requests
        if i % 2 == 0 {
            match http_get_request(test_urls[url_idx]) {
                Ok(_) => {
                    successful_gets += 1;
                    log_info(&format!("GET {} successful", url_idx));
                }
                Err(e) => log_info(&format!("GET {} failed: {}", url_idx, e)),
            }
        } else {
            let body = format!(r#"{{"request_id": {}, "timestamp": "{:?}"}}"#, i, std::time::SystemTime::now());
            match http_post_request("http://httpbin.org/post", &body) {
                Ok(_) => {
                    successful_posts += 1;
                    log_info(&format!("POST {} successful", i));
                }
                Err(e) => log_info(&format!("POST {} failed: {}", i, e)),
            }
        }
    }
    
    (successful_gets, successful_posts)
}

#[no_mangle]
pub extern "C" fn _start() -> i32 {
    let args: Vec<String> = env::args().collect();
    let num_requests = if args.len() > 1 {
        args[1].parse().unwrap_or(5)
    } else {
        5
    };
    
    let (gets, posts) = aggregate_apis(num_requests);
    log_info(&format!("Completed: {} GET, {} POST requests", gets, posts));
    0
}

