use std::env;

fn log_info(msg: &str) {
    println!("{}", msg);
}

// Sieve of Eratosthenes for prime generation
fn sieve_of_eratosthenes(limit: usize) -> Vec<usize> {
    let mut is_prime = vec![true; limit + 1];
    is_prime[0] = false;
    if limit > 0 {
        is_prime[1] = false;
    }
    
    for i in 2..=((limit as f64).sqrt() as usize) {
        if is_prime[i] {
            for j in ((i * i)..=limit).step_by(i) {
                is_prime[j] = false;
            }
        }
    }
    
    (2..=limit).filter(|&i| is_prime[i]).collect()
}

// CPU-intensive prime computation with additional work
fn compute_primes_with_validation(limit: usize) -> (usize, u64) {
    let start_time = std::time::Instant::now();
    
    // Generate primes
    let primes = sieve_of_eratosthenes(limit);
    let prime_count = primes.len();
    
    // Additional CPU work: validate primality and compute sum
    let mut sum = 0u64;
    for prime in primes {
        // Verify each prime by trial division (added CPU work)
        let mut is_prime = true;
        for i in 2..=((prime as f64).sqrt() as usize) {
            if prime % i == 0 {
                is_prime = false;
                break;
            }
        }
        if is_prime {
            sum += prime as u64;
        }
    }
    
    let elapsed = start_time.elapsed();
    log_info(&format!("Computed {} primes up to {} in {:?}", prime_count, limit, elapsed));
    
    (prime_count, sum)
}

#[no_mangle]
pub extern "C" fn _start() -> i32 {
    let args: Vec<String> = env::args().collect();
    let limit = if args.len() > 1 {
        args[1].parse().unwrap_or(10000)
    } else {
        10000
    };
    
    let (count, sum) = compute_primes_with_validation(limit);
    log_info(&format!("Result: {} primes, sum: {}", count, sum));
    0
}
