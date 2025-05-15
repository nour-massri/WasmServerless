#!/usr/bin/env python3
import subprocess
import socket
import json
import time
import pandas as pd
import matplotlib.pyplot as plt
import numpy as np
import concurrent.futures
import os
import sys
import threading
import signal
from datetime import datetime
from typing import List, Dict, Tuple
import tempfile
import shutil
import http.client
from urllib.parse import urlparse

class UnixSocketHTTPConnection:
    """HTTP client over Unix socket"""
    def __init__(self, socket_path: str):
        self.socket_path = socket_path
        self.sock = None
    def connect(self):
        """Connect to Unix socket"""
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10.0)  # Add 10 second timeout
        self.sock.connect(self.socket_path)

    def close(self):
        """Close the socket connection"""
        if self.sock:
            self.sock.close()
            self.sock = None
    
    def request(self, method: str, path: str, body: str = None, headers: Dict[str, str] = None) -> Tuple[int, Dict[str, str], str]:
        """Send HTTP request over Unix socket"""
        if not self.sock:
            self.connect()
        
        # Prepare headers
        req_headers = {
            'Host': 'localhost',
            'Content-Type': 'application/json',
            'Connection': 'close'
        }
        if headers:
            req_headers.update(headers)
        
        if body:
            req_headers['Content-Length'] = str(len(body.encode('utf-8')))
        
        # Build HTTP request
        request_lines = [f"{method} {path} HTTP/1.1"]
        for key, value in req_headers.items():
            request_lines.append(f"{key}: {value}")
        request_lines.append("")  # Empty line
        if body:
            request_lines.append(body)
        
        request_str = "\r\n".join(request_lines)
        
        # Send request
        self.sock.sendall(request_str.encode('utf-8'))
        
        # Read response
        response_data = b""
        while True:
            chunk = self.sock.recv(4096)
            if not chunk:
                break
            response_data += chunk
            # Check if we have the complete response
            if b"\r\n\r\n" in response_data:
                break
        
        # Parse response
        response_str = response_data.decode('utf-8')
        lines = response_str.split('\r\n')
        
        # Parse status line
        status_line = lines[0]
        status_code = int(status_line.split()[1])
        
        # Parse headers
        response_headers = {}
        i = 1
        while i < len(lines) and lines[i]:
            if ':' in lines[i]:
                key, value = lines[i].split(':', 1)
                response_headers[key.strip()] = value.strip()
            i += 1
        
        # Parse body
        body_start = i + 1
        if body_start < len(lines):
            response_body = '\r\n'.join(lines[body_start:])
        else:
            response_body = ""
        
        self.close()  # Close connection after each request
        
        return status_code, response_headers, response_body

class WasmBenchmark:
    def __init__(self, server_binary_path: str, hello_world_wasm_path: str):
        self.server_binary_path = server_binary_path
        self.hello_world_wasm_path = hello_world_wasm_path
        self.server_process = None
        self.socket_path = "/tmp/wasm-serverless.sock"
        self.results = []
        self.log_dir = f"benchmark_logs_{datetime.now().strftime('%Y%m%d_%H%M%S')}"
        os.makedirs(self.log_dir, exist_ok=True)
            
    def start_server(self, optimize: bool = True, cache: bool = False, timeout_seconds: int = 60):
        """Start the server with specified configuration"""
        if self.server_process:
            self.stop_server()
            
        # Remove existing socket if it exists
        if os.path.exists(self.socket_path):
            os.remove(self.socket_path)
                
        cmd = [
            self.server_binary_path,
            "--log-level", "info",
            "--socket-path", self.socket_path,
            "--timeout-seconds", str(timeout_seconds)
        ]
        
        # Only add flags if they should be enabled
        if optimize:
            cmd.append("--optimize")
            
        if cache:
            cmd.append("--cache")
            
        # Create log files
        config_name = f"{'opt' if optimize else 'no-opt'}_{'cache' if cache else 'no-cache'}"
        stdout_log = open(f"{self.log_dir}/server_{config_name}_stdout.log", "w")
        stderr_log = open(f"{self.log_dir}/server_{config_name}_stderr.log", "w")
        
        print(f"Starting server with command: {' '.join(cmd)}")
        time.sleep(5)

        # self.server_process = subprocess.Popen(
        #     cmd,
        #     stdout=stdout_log,
        #     stderr=stderr_log,
        #     preexec_fn=os.setsid  # Create new process group for clean shutdown
        # )
        # # MODIFIED: Wait for server to start with improved health check
        # start_time = time.time()
        # max_wait_time = 30  # seconds
        # check_interval = 1.5  # seconds
        
        # while time.time() - start_time < max_wait_time:
        #     # Check if process is still running
        #     if self.server_process.poll() is not None:
        #         stdout_log.close()
        #         stderr_log.close()
        #         with open(f"{self.log_dir}/server_{config_name}_stderr.log", "r") as f:
        #             stderr_content = f.read()
        #         raise Exception(f"Server process died with exit code {self.server_process.returncode}. Stderr: {stderr_content}")
            
        #     # MODIFIED: Check if socket file exists before attempting connection
        #     if not os.path.exists(self.socket_path):
        #         print(f"Waiting for socket file to be created... ({time.time() - start_time:.1f}s)")
        #         time.sleep(check_interval)
        #         continue
                
        #     # Try a health check with timeout
        #     try:
        #         print(f"Attempting health check... ({time.time() - start_time:.1f}s)")
        #         # MODIFIED: Use a standard socket connection first to verify the socket is accepting connections
        #         sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        #         sock.settimeout(3.0)
        #         try:
        #             sock.connect(self.socket_path)
        #             sock.close()
        #         except Exception as e:
        #             print(f"Socket connection failed: {e}")
        #             time.sleep(check_interval)
        #             continue
                    
        #         # Now try the HTTP health check
        #         conn = UnixSocketHTTPConnection(self.socket_path)
        #         status, headers, body = conn.request("GET", "/health")
        #         if status == 200:
        #             print(f"Server started successfully after {time.time() - start_time:.1f} seconds")
        #             return
        #         else:
        #             print(f"Health check failed with status {status}: {body}")
        #     except Exception as e:
        #         print(f"Health check failed: {str(e)}")
                
        #     time.sleep(check_interval)
            
        # # If we get here, capture final error state
        # stdout_log.close()
        # stderr_log.close()
        # if self.server_process.poll() is not None:
        #     with open(f"{self.log_dir}/server_{config_name}_stderr.log", "r") as f:
        #         stderr_content = f.read()
        #     raise Exception(f"Server failed to start within {max_wait_time} seconds. Process exited with code {self.server_process.returncode}. Stderr: {stderr_content}")
        # else:
        #     raise Exception(f"Server failed to start within {max_wait_time} seconds - health check timed out")

    def stop_server(self):
        """Stop the server process"""
        if self.server_process:
            try:
                # Send SIGTERM to the process group
                os.killpg(os.getpgid(self.server_process.pid), signal.SIGTERM)
                self.server_process.wait(timeout=5)
            except:
                # Force kill if needed
                os.killpg(os.getpgid(self.server_process.pid), signal.SIGKILL)
                self.server_process.wait()
            finally:
                self.server_process = None
                time.sleep(1)  # Give time for cleanup
                # Remove socket file
                if os.path.exists(self.socket_path):
                    os.remove(self.socket_path)
    
    def init_module(self) -> str:
        """Initialize the WebAssembly module and return module_id"""
        conn = UnixSocketHTTPConnection(self.socket_path)
        payload = {"wasm_path": self.hello_world_wasm_path}
        status, headers, body = conn.request("POST", "/init", json.dumps(payload))
        
        if status != 200:
            raise Exception(f"Init failed with status {status}: {body}")
            
        data = json.loads(body)
        return data["module_id"]
    
    def run_module(self, module_id: str) -> Dict:
        """Run a module and return the metrics"""
        conn = UnixSocketHTTPConnection(self.socket_path)
        payload = {
            "module_id": module_id,
            "env": {},
            "args": [],
            "timeout_seconds": None
        }
        status, headers, body = conn.request("POST", "/run", json.dumps(payload))
        
        if status != 200:
            raise Exception(f"Run failed with status {status}: {body}")
            
        return json.loads(body)
    
    def calculate_start_time(self, metrics: Dict) -> float:
        """Calculate start time from metrics (in milliseconds)"""
        return (
            metrics["module_load_time_us"] + 
            metrics["instantiation_time_us"] + 
            metrics["execution_time_us"]
        ) / 1000.0  # Convert to milliseconds
    
    def run_serial_benchmark(self, module_id: str, num_runs: int, test_type: str, skip_first: bool = False) -> List[float]:
        """Run serial benchmark and return start times"""
        start_times = []
        
        print(f"Running {num_runs} serial {test_type} starts...")
        for i in range(num_runs):
            if skip_first and i == 0:
                # Skip first run as it's a cold start in warm test
                result = self.run_module(module_id)
                print(f"Skipped run {i+1} (cold start)")
                continue
                
            start_time = time.time()
            result = self.run_module(module_id)
            end_time = time.time()
            
            metrics = result["metrics"]
            start_time_ms = self.calculate_start_time(metrics)
            start_times.append(start_time_ms)
            
            # Check if it was loaded from memory (warm start)
            loaded_from_memory = metrics.get("loaded_from_memory", False)
            
            # Log detailed metrics
            print(f"Run {i+1}: {start_time_ms:.2f}ms (load: {metrics['module_load_time_us']/1000:.2f}ms, "
                  f"init: {metrics['instantiation_time_us']/1000:.2f}ms, "
                  f"exec: {metrics['execution_time_us']/1000:.2f}ms, "
                  f"cached: {loaded_from_memory})")
            
            # Sleep between runs to avoid overwhelming the server
            time.sleep(1)
            
        return start_times
    
    def run_concurrent_benchmark(self, module_id: str, num_runs: int, test_type: str, skip_first: bool = False) -> List[float]:
        """Run concurrent benchmark and return start times"""
        if skip_first:
            # Do one run to warm up
            self.run_module(module_id)
            print("Warmed up with one run")
        
        start_times = []
        
        def run_single():
            result = self.run_module(module_id)
            metrics = result["metrics"]
            return self.calculate_start_time(metrics)
        
        print(f"Running {num_runs} concurrent {test_type} starts...")
        
        # Use ThreadPoolExecutor for concurrent execution
        with concurrent.futures.ThreadPoolExecutor(max_workers=num_runs) as executor:
            futures = [executor.submit(run_single) for _ in range(num_runs)]
            
            for i, future in enumerate(concurrent.futures.as_completed(futures)):
                try:
                    start_time_ms = future.result()
                    start_times.append(start_time_ms)
                    print(f"Completed run {i+1}: {start_time_ms:.2f}ms")
                except Exception as e:
                    print(f"Run failed: {e}")
        
        return sorted(start_times)
    # ADDED: New method for running concurrent benchmarks in batches
    def run_concurrent_benchmark_in_batches(self, module_id: str, batch_size: int, test_type: str, skip_first: bool = False) -> List[float]:
        """Run concurrent benchmark in batches of requests and return start times"""
        if skip_first:
            # Do one run to warm up
            self.run_module(module_id)
            print("Warmed up with one run")
        
        all_start_times = []
        total_runs = 100  # Total number of runs 
        num_batches = total_runs // batch_size
        
        print(f"Running {total_runs} concurrent {test_type} starts in {num_batches} batches of {batch_size}...")
        
        for batch in range(num_batches):
            print(f"\nStarting batch {batch+1}/{num_batches}...")
            batch_start_times = []
            
            def run_single():
                try:
                    result = self.run_module(module_id)
                    metrics = result["metrics"]
                    return self.calculate_start_time(metrics)
                except Exception as e:
                    print(f"Run failed: {e}")
                    return None
            
            # Use ThreadPoolExecutor for concurrent execution within each batch
            with concurrent.futures.ThreadPoolExecutor(max_workers=batch_size) as executor:
                futures = [executor.submit(run_single) for _ in range(batch_size)]
                
                for i, future in enumerate(concurrent.futures.as_completed(futures)):
                    try:
                        start_time_ms = future.result()
                        if start_time_ms is not None:
                            batch_start_times.append(start_time_ms)
                            print(f"Batch {batch+1}, Run {i+1}: {start_time_ms:.2f}ms")
                    except Exception as e:
                        print(f"Error processing result: {e}")
            
            all_start_times.extend(batch_start_times)
            print(f"Completed batch {batch+1}/{num_batches} with {len(batch_start_times)} successful runs")
            
            # Add a short pause between batches to avoid overwhelming the server
            if batch < num_batches - 1:
                time.sleep(2)
        
        return sorted(all_start_times)

    def run_benchmark_suite(self, num_serial: int = 100, num_concurrent: int = 50):
        """Run the complete benchmark suite"""
        all_results = {}
        
        # Separated the test configurations more clearly
        cold_start_config = {"name": "cold_start", "optimize": True, "cache": False, "skip_first": False}
        warm_start_config = {"name": "warm_start", "optimize": True, "cache": True, "skip_first": True}
        
        configs = [cold_start_config, warm_start_config]
        
        for config in configs:
            print(f"\n{'='*50}")
            print(f"Testing {config['name']} configuration")
            print(f"{'='*50}")
            
            #  Start server with specific configuration for each test type
            print(f"Starting server with {'cache enabled' if config['cache'] else 'cache disabled'}")
            self.start_server(optimize=config["optimize"], cache=config["cache"])
            
            try:
                # Initialize module
                module_id = self.init_module()
                print(f"Module initialized: {module_id}")
                
                # Run serial tests
                print(f"\nSerial {config['name']} tests:")
                serial_times = self.run_serial_benchmark(
                    module_id, num_serial, config["name"], config["skip_first"]
                )
                
                # Use the new batch concurrent method
                print(f"\nConcurrent {config['name']} tests:")
                concurrent_times = self.run_concurrent_benchmark_in_batches(
                    module_id, num_concurrent, config["name"], config["skip_first"]
                )
                
                all_results[config["name"]] = {
                    "serial": serial_times,
                    "concurrent": concurrent_times
                }
                
                # Print summary statistics
                print(f"\n{config['name']} Summary:")
                print(f"Serial - Mean: {np.mean(serial_times):.2f}ms, "
                    f"Median: {np.median(serial_times):.2f}ms, "
                    f"Min: {np.min(serial_times):.2f}ms, "
                    f"Max: {np.max(serial_times):.2f}ms")
                print(f"Concurrent - Mean: {np.mean(concurrent_times):.2f}ms, "
                    f"Median: {np.median(concurrent_times):.2f}ms, "
                    f"Min: {np.min(concurrent_times):.2f}ms, "
                    f"Max: {np.max(concurrent_times):.2f}ms")
                
            finally:
                self.stop_server()
                
        return all_results
    
    def save_results_to_csv(self, results: Dict):
        """Save results to CSV files"""
        # Save detailed results
        detailed_data = []
        for test_type, data in results.items():
            for exec_type, times in data.items():
                for i, time_ms in enumerate(times):
                    detailed_data.append({
                        "test_type": test_type,
                        "execution_type": exec_type,
                        "run_number": i + 1,
                        "start_time_ms": time_ms
                    })
        
        df = pd.DataFrame(detailed_data)
        csv_path = f"{self.log_dir}/benchmark_results.csv"
        df.to_csv(csv_path, index=False)
        print(f"\nDetailed results saved to {csv_path}")
        
        # Save summary statistics
        summary_data = []
        for test_type, data in results.items():
            for exec_type, times in data.items():
                summary_data.append({
                    "test_type": test_type,
                    "execution_type": exec_type,
                    "count": len(times),
                    "mean_ms": np.mean(times),
                    "median_ms": np.median(times),
                    "min_ms": np.min(times),
                    "max_ms": np.max(times),
                    "std_ms": np.std(times),
                    "p50_ms": np.percentile(times, 50),
                    "p90_ms": np.percentile(times, 90),
                    "p95_ms": np.percentile(times, 95),
                    "p99_ms": np.percentile(times, 99)
                })
        
        summary_df = pd.DataFrame(summary_data)
        summary_path = f"{self.log_dir}/benchmark_summary.csv"
        summary_df.to_csv(summary_path, index=False)
        print(f"Summary statistics saved to {summary_path}")
    
    def plot_cdf_comparison(self, results: Dict):
        """Create CDF plots similar to Firecracker benchmarks"""
        # Create figure with subplots
        fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(15, 6))
        
        # Colors for different test types
        colors = {
            "cold_start": "#FF6B6B",
            "warm_start": "#4ECDC4"
        }
        
        # Plot serial execution CDF
        ax1.set_title("Serial Execution Start Times", fontsize=14)
        for test_type, data in results.items():
            times = data["serial"]
            times_sorted = np.sort(times)
            y = np.arange(1, len(times_sorted) + 1) / len(times_sorted)
            ax1.plot(times_sorted, y, label=f"{test_type.replace('_', ' ').title()}", 
                    color=colors[test_type], linewidth=2)
        
        ax1.set_xlabel("Boot time (ms)", fontsize=12)
        ax1.set_ylabel("CDF", fontsize=12)
        ax1.legend(fontsize=11)
        ax1.grid(True, alpha=0.3)
        ax1.set_xlim(0, None)
        ax1.set_ylim(0, 1)
        
        # Plot concurrent execution CDF
        ax2.set_title("Concurrent Execution Start Times", fontsize=14)
        for test_type, data in results.items():
            times = data["concurrent"]
            times_sorted = np.sort(times)
            y = np.arange(1, len(times_sorted) + 1) / len(times_sorted)
            ax2.plot(times_sorted, y, label=f"{test_type.replace('_', ' ').title()}", 
                    color=colors[test_type], linewidth=2)
        
        ax2.set_xlabel("Boot time (ms)", fontsize=12)
        ax2.set_ylabel("CDF", fontsize=12)
        ax2.legend(fontsize=11)
        ax2.grid(True, alpha=0.3)
        ax2.set_xlim(0, None)
        ax2.set_ylim(0, 1)
        
        plt.tight_layout()
        plot_path = f"{self.log_dir}/benchmark_cdf_comparison.png"
        plt.savefig(plot_path, dpi=300, bbox_inches='tight')
        print(f"CDF comparison plot saved to {plot_path}")
        plt.close()
        
        # Also create individual plots like Firecracker
        self.plot_individual_cdfs(results)
    
    def plot_individual_cdfs(self, results: Dict):
        """Create individual CDF plots for each execution type"""
        for exec_type in ["serial", "concurrent"]:
            fig, ax = plt.subplots(figsize=(10, 6))
            
            for test_type, data in results.items():
                times = data[exec_type]
                times_sorted = np.sort(times)
                y = np.arange(1, len(times_sorted) + 1) / len(times_sorted)
                
                label = f"{test_type.replace('_', ' ').title()}"
                color = "#FF6B6B" if test_type == "cold_start" else "#4ECDC4"
                ax.plot(times_sorted, y, label=label, color=color, linewidth=3)
            
            num_runs = len(results["cold_start"][exec_type])
            exec_title = exec_type.title()
            title = f"Cumulative distribution of wall-clock times for starting {num_runs} "
            title += f"containers in {exec_type}, for WebAssembly containers"
            
            ax.set_title(title, fontsize=12)
            ax.set_xlabel("Boot time (ms)", fontsize=12)
            ax.set_ylabel("CDF", fontsize=12)
            ax.legend(fontsize=11)
            ax.grid(True, alpha=0.3)
            ax.set_xlim(0, None)
            ax.set_ylim(0, 1)
            
            plt.tight_layout()
            plot_path = f"{self.log_dir}/benchmark_cdf_{exec_type}.png"
            plt.savefig(plot_path, dpi=300, bbox_inches='tight')
            print(f"{exec_type.title()} CDF plot saved to {plot_path}")
            plt.close()

def create_hello_world_wasm():
    """Create a simple hello world WebAssembly module"""
    # Create a temporary directory for building
    with tempfile.TemporaryDirectory() as temp_dir:
        # Create hello.c
        c_code = '''
#include <stdio.h>

int main() {
    printf("Hello, WebAssembly World!\\n");
    return 0;
}
'''
        c_file = os.path.join(temp_dir, "hello.c")
        with open(c_file, "w") as f:
            f.write(c_code)
        
        # Compile to WebAssembly
        wasm_file = "hello_world.wasm"
        
        # Try different compilers
        compilers = [
            ["clang", "--target=wasm32-unknown-wasi", "-o", wasm_file, c_file],
            ["clang", "--target=wasm32-wasi", "-o", wasm_file, c_file],
            ["gcc", "--target=wasm32-wasi", "-o", wasm_file, c_file],
        ]
        
        for cmd in compilers:
            try:
                print(f"Trying to compile with: {' '.join(cmd)}")
                subprocess.run(cmd, check=True, capture_output=True)
                if os.path.exists(wasm_file):
                    print(f"Successfully created {wasm_file}")
                    return os.path.abspath(wasm_file)
            except (subprocess.CalledProcessError, FileNotFoundError) as e:
                print(f"Failed: {e}")
                continue
        
        raise Exception("Could not compile hello world to WebAssembly. Please ensure you have a WebAssembly-capable compiler installed.")

def main():
    if len(sys.argv) < 2:
        print("Usage: python benchmark.py <path_to_server_binary> [path_to_hello_world.wasm]")
        print("If hello_world.wasm is not provided, it will be created automatically.")
        sys.exit(1)
    
    server_binary = sys.argv[1]
    
    # Get or create hello world wasm
    if len(sys.argv) >= 3:
        hello_world_wasm = sys.argv[2]
        if not os.path.exists(hello_world_wasm):
            print(f"Error: WebAssembly file {hello_world_wasm} not found")
            sys.exit(1)
    else:
        print("Creating hello world WebAssembly module...")
        hello_world_wasm = create_hello_world_wasm()
    
    # Verify server binary exists
    if not os.path.exists(server_binary):
        print(f"Error: Server binary {server_binary} not found")
        sys.exit(1)
    
    # Run benchmark
    benchmark = WasmBenchmark(server_binary, hello_world_wasm)
    
    try:
        print("Starting WebAssembly Container Benchmark (Unix Socket)")
        print(f"Server binary: {server_binary}")
        print(f"Hello world WASM: {hello_world_wasm}")
        print(f"Socket path: {benchmark.socket_path}")
        print(f"Results will be saved to: {benchmark.log_dir}")
        
        # Run the benchmark suite
        results = benchmark.run_benchmark_suite(num_serial=100, num_concurrent=50)
        
        # Save results and create visualizations
        benchmark.save_results_to_csv(results)
        benchmark.plot_cdf_comparison(results)
        
        print(f"\nBenchmark completed successfully!")
        print(f"All results and logs saved to: {benchmark.log_dir}")
        
    except KeyboardInterrupt:
        print("\nBenchmark interrupted by user")
    except Exception as e:
        print(f"Benchmark failed: {e}")
        import traceback
        traceback.print_exc()
    finally:
        # Ensure server is stopped
        benchmark.stop_server()

if __name__ == "__main__":
    main()