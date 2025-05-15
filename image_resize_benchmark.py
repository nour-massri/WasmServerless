#!/usr/bin/env python3
import subprocess
import os
import json
import time
import pandas as pd
import matplotlib.pyplot as plt
import numpy as np
import concurrent.futures
import boto3
from datetime import datetime
import argparse
from typing import List, Dict, Tuple
import socket

class ImageResizeBenchmark:
    def __init__(self, server_binary_path: str, s3_wasm_path: str, s3_bucket: str):
        self.server_binary_path = server_binary_path
        self.s3_wasm_path = s3_wasm_path
        self.s3_bucket = s3_bucket
        self.server_process = None
        self.socket_path = "/tmp/wasm-serverless.sock"
        self.log_dir = f"image_resize_benchmark_{datetime.now().strftime('%Y%m%d_%H%M%S')}"
        os.makedirs(self.log_dir, exist_ok=True)
        
        # Benchmark configurations - only 1 to 32 workers
        self.worker_counts = [1, 2, 4, 8, 16, 32]
        self.results = {}
        
    def start_server(self, optimize: bool = True, cache: bool = True):
        """Start the WASM runtime server"""
        if self.server_process:
            self.stop_server()
            
        # Remove existing socket
        if os.path.exists(self.socket_path):
            os.remove(self.socket_path)
        
        cmd = [
            self.server_binary_path,
            "--log-level", "info",
            "--socket-path", self.socket_path,
            "--timeout-seconds", "600"  # Increased timeout to 10 minutes
        ]
        
        if optimize:
            cmd.append("--optimize")
        if cache:
            cmd.append("--cache")
            
        print(f"Starting server: {' '.join(cmd)}")
        
        stdout_log = open(f"{self.log_dir}/server_stdout.log", "w")
        stderr_log = open(f"{self.log_dir}/server_stderr.log", "w")
        
        self.server_process = subprocess.Popen(
            cmd,
            stdout=stdout_log,
            stderr=stderr_log,
            preexec_fn=os.setsid
        )
        
        # Wait for server to start
        max_wait = 30
        start_time = time.time()
        
        while time.time() - start_time < max_wait:
            if os.path.exists(self.socket_path):
                time.sleep(2)
                print("Server started successfully")
                return
            time.sleep(1)
            
        raise Exception("Server failed to start within timeout")
    
    def stop_server(self):
        """Stop the server process"""
        if self.server_process:
            try:
                os.killpg(os.getpgid(self.server_process.pid), 15)
                self.server_process.wait(timeout=5)
            except:
                os.killpg(os.getpgid(self.server_process.pid), 9)
                self.server_process.wait()
            finally:
                self.server_process = None
                if os.path.exists(self.socket_path):
                    os.remove(self.socket_path)
    
    def init_module(self) -> str:
        """Initialize the S3 WASM module"""
        conn = UnixSocketHTTPConnection(self.socket_path)
        
        payload = {"wasm_path": self.s3_wasm_path}
        status, headers, body = conn.request("POST", "/init", json.dumps(payload))
        
        if status != 200:
            raise Exception(f"Module init failed: {status} - {body}")
        
        data = json.loads(body)
        module_id = data["module_id"]
        print(f"Initialized S3 module: {module_id}")
        return module_id
    
    def check_worker_directories(self, num_workers: int) -> List[Tuple[str, str]]:
        """Check if pre-created worker directories exist"""
        s3 = boto3.client('s3')
        worker_prefixes = []
        
        for worker_id in range(num_workers):
            input_prefix = f"{num_workers}worker_{worker_id + 1}/input/"
            output_prefix = f"{num_workers}worker_{worker_id + 1}/output/"
            
            # Check if input directory has images
            resp = s3.list_objects_v2(
                Bucket=self.s3_bucket,
                Prefix=input_prefix,
                MaxKeys=1
            )
            
            if 'Contents' in resp:
                worker_prefixes.append((input_prefix, output_prefix))
                print(f"Found worker {worker_id + 1} directory: {input_prefix}")
            else:
                raise Exception(f"Worker directory not found: {input_prefix}. Please run the setup script first.")
        
        return worker_prefixes
    
    def count_images_in_worker_dirs(self, num_workers: int) -> int:
        """Count total images in all worker directories"""
        s3 = boto3.client('s3')
        total_images = 0
        
        for worker_id in range(num_workers):
            input_prefix = f"{num_workers}worker_{worker_id + 1}/input/"
            
            paginator = s3.get_paginator('list_objects_v2')
            page_iterator = paginator.paginate(
                Bucket=self.s3_bucket,
                Prefix=input_prefix
            )
            
            worker_count = 0
            for page in page_iterator:
                if 'Contents' in page:
                    worker_count += len([obj for obj in page['Contents'] 
                                       if self.is_image_key(obj['Key'])])
            
            total_images += worker_count
            print(f"Worker {worker_id + 1}: {worker_count} images")
        
        return total_images
    
    def run_resize_task(self, module_id: str, worker_id: int, input_prefix: str, output_prefix: str,
                       width: int = 800, height: int = 600) -> Dict:
        """Run image resize task for a specific worker using pre-created directories"""
        conn = UnixSocketHTTPConnection(self.socket_path)
        
        args = [
            'program',
            '--bucket', self.s3_bucket,
            '--input-prefix', input_prefix,
            '--output-prefix', output_prefix,
            '--width', str(width),
            '--height', str(height),
            '--max-keys', '10000'  # Process all images in the worker's directory
        ]
        
        payload = {
            "module_id": module_id,
            "env": {},  # No need to pass AWS credentials
            "args": args,
            "timeout_seconds": 600  # Increased timeout to 10 minutes
        }
        
        start_time = time.time()
        status, headers, body = conn.request("POST", "/run", json.dumps(payload))
        end_time = time.time()
        
        if status != 200:
            raise Exception(f"Task failed: {status} - {body}")
        
        result = json.loads(body)
        result['wall_clock_time'] = end_time - start_time
        result['worker_id'] = worker_id
        result['input_prefix'] = input_prefix
        result['output_prefix'] = output_prefix
        return result
    
    def benchmark_parallel_workers(self, module_id: str, num_workers: int) -> Dict:
        """Benchmark with specified number of parallel workers using pre-created directories"""
        print(f"\nBenchmarking {num_workers} parallel workers (using pre-created directories)")
        
        # Check if worker directories exist
        worker_prefixes = self.check_worker_directories(num_workers)
        
        # Count total images
        total_images = self.count_images_in_worker_dirs(num_workers)
        print(f"Total images to process: {total_images}")
        
        # Run workers in parallel
        start_time = time.time()
        results = []
        
        with concurrent.futures.ThreadPoolExecutor(max_workers=num_workers) as executor:
            futures = []
            for worker_id, (input_prefix, output_prefix) in enumerate(worker_prefixes):
                future = executor.submit(
                    self.run_resize_task, module_id, worker_id, input_prefix, output_prefix
                )
                futures.append(future)
            
            for i, future in enumerate(concurrent.futures.as_completed(futures)):
                try:
                    result = future.result()
                    results.append(result)
                    print(f"Worker {result['worker_id']} completed in {result['wall_clock_time']:.2f}s")
                except Exception as e:
                    print(f"Worker {i} failed: {e}")
                    results.append({'error': str(e), 'wall_clock_time': -1, 'worker_id': i})
        
        end_time = time.time()
        total_time = end_time - start_time
        
        # Calculate statistics
        successful_results = [r for r in results if 'error' not in r]
        failed_count = len(results) - len(successful_results)
        
        if successful_results:
            metrics = {
                'num_workers': num_workers,
                'total_wall_clock_time': total_time,
                'successful_workers': len(successful_results),
                'failed_workers': failed_count,
                'avg_worker_time': np.mean([r['wall_clock_time'] for r in successful_results]),
                'max_worker_time': np.max([r['wall_clock_time'] for r in successful_results]),
                'min_worker_time': np.min([r['wall_clock_time'] for r in successful_results]),
                'std_worker_time': np.std([r['wall_clock_time'] for r in successful_results]),
                'avg_module_load_time': np.mean([r['metrics']['module_load_time_us'] / 1000000 
                                               for r in successful_results]),
                'avg_instantiation_time': np.mean([r['metrics']['instantiation_time_us'] / 1000000 
                                                 for r in successful_results]),
                'avg_execution_time': np.mean([r['metrics']['execution_time_us'] / 1000000 
                                             for r in successful_results]),
                'total_images_processed': total_images,
                'throughput_images_per_second': total_images / total_time,
                'efficiency': total_images / (num_workers * total_time),
                'speedup': (self.results[1]['total_wall_clock_time'] / total_time) if 1 in self.results else 1
            }
        else:
            metrics = {
                'num_workers': num_workers,
                'total_wall_clock_time': total_time,
                'successful_workers': 0,
                'failed_workers': failed_count,
                'error': 'All workers failed'
            }
        
        print(f"Completed {num_workers} workers in {total_time:.2f}s")
        print(f"Success rate: {len(successful_results)}/{num_workers}")
        if successful_results:
            print(f"Total images processed: {metrics['total_images_processed']}")
            print(f"Throughput: {metrics['throughput_images_per_second']:.2f} images/sec")
            print(f"Average worker time: {metrics['avg_worker_time']:.2f}s")
            print(f"Worker time std dev: {metrics['std_worker_time']:.2f}s")
        
        return metrics
    
    def run_full_benchmark(self):
        """Run the complete benchmark suite using pre-created worker directories"""
        print("Starting S3 Image Resize Benchmark (using pre-created worker directories)")
        print(f"S3 Bucket: {self.s3_bucket}")
        print("Mode: Using pre-divided worker directories")
        
        # Start server
        self.start_server(optimize=True, cache=True)
        
        try:
            # Initialize module
            module_id = self.init_module()
            
            # Run benchmarks with different worker counts
            all_results = []
            
            for i, num_workers in enumerate(self.worker_counts):
                print(f"\n{'='*70}")
                print(f"Testing with {num_workers} parallel workers ({i+1}/{len(self.worker_counts)})")
                print(f"{'='*70}")
                
                # Check if worker directories exist before running
                try:
                    self.check_worker_directories(num_workers)
                except Exception as e:
                    print(f"Skipping {num_workers} workers: {e}")
                    continue
                
                # Run benchmark
                result = self.benchmark_parallel_workers(module_id, num_workers)
                all_results.append(result)
                self.results[num_workers] = result
                
                # Add time buffer between experiments (except after the last test)
                if i < len(self.worker_counts) - 1:
                    print("Waiting 5 seconds before next test...")
                    time.sleep(5)
            
            # Save and visualize results
            self.save_results(all_results)
            self.create_visualizations(all_results)
            
            print(f"\nBenchmark completed successfully!")
            print(f"Results saved to: {self.log_dir}")
            
        finally:
            self.stop_server()
    
    @staticmethod
    def is_image_key(key: str) -> bool:
        """Check if S3 key is an image file"""
        key_lower = key.lower()
        return any(key_lower.endswith(ext) for ext in 
                  ['.jpg', '.jpeg', '.png', '.gif', '.bmp', '.tiff', '.webp'])
    
    def save_results(self, results: List[Dict]):
        """Save benchmark results to CSV and JSON"""
        # Save to CSV
        df = pd.DataFrame(results)
        csv_path = os.path.join(self.log_dir, "benchmark_results.csv")
        df.to_csv(csv_path, index=False)
        print(f"Results saved to: {csv_path}")
        
        # Save to JSON for detailed analysis
        json_path = os.path.join(self.log_dir, "benchmark_results.json")
        with open(json_path, 'w') as f:
            json.dump(results, f, indent=2)
        print(f"Detailed results saved to: {json_path}")
    
    def create_visualizations(self, results: List[Dict]):
        """Create comprehensive visualizations of benchmark results"""
        successful_results = [r for r in results if r.get('successful_workers', 0) > 0]
        
        if not successful_results:
            print("No successful results to visualize")
            return
        
        # Extract data
        worker_counts = [r['num_workers'] for r in successful_results]
        total_times = [r['total_wall_clock_time'] for r in successful_results]
        throughputs = [r['throughput_images_per_second'] for r in successful_results]
        avg_worker_times = [r['avg_worker_time'] for r in successful_results]
        total_images = [r['total_images_processed'] for r in successful_results]
        
        # Create comprehensive subplot layout
        fig = plt.figure(figsize=(20, 15))
        
        # 1. Total Time vs Number of Workers
        ax1 = plt.subplot(2, 3, 1)
        ax1.plot(worker_counts, total_times, 'bo-', linewidth=2, markersize=8)
        ax1.set_xlabel('Number of Parallel Workers')
        ax1.set_ylabel('Total Time (seconds)')
        ax1.set_title('Total Processing Time vs Number of Workers')
        ax1.grid(True, alpha=0.3)
        ax1.set_xscale('log', base=2)
        ax1.set_xticks(worker_counts)
        ax1.set_xticklabels(worker_counts)
        
        # 2. Throughput vs Number of Workers
        ax2 = plt.subplot(2, 3, 2)
        ax2.plot(worker_counts, throughputs, 'ro-', linewidth=2, markersize=8)
        ax2.set_xlabel('Number of Parallel Workers')
        ax2.set_ylabel('Throughput (images/second)')
        ax2.set_title('System Throughput vs Number of Workers')
        ax2.grid(True, alpha=0.3)
        ax2.set_xscale('log', base=2)
        ax2.set_xticks(worker_counts)
        ax2.set_xticklabels(worker_counts)
        
        # 3. Speedup vs Number of Workers
        ax3 = plt.subplot(2, 3, 3)
        baseline_time = total_times[0] if total_times else 1
        speedups = [baseline_time / t for t in total_times]
        ideal_speedup = worker_counts
        
        ax3.plot(worker_counts, speedups, 'go-', linewidth=2, markersize=8, label='Actual Speedup')
        ax3.plot(worker_counts, ideal_speedup, 'g--', linewidth=1, alpha=0.7, label='Ideal Speedup')
        ax3.set_xlabel('Number of Workers')
        ax3.set_ylabel('Speedup')
        ax3.set_title('Speedup vs Number of Workers')
        ax3.grid(True, alpha=0.3)
        ax3.set_xscale('log', base=2)
        ax3.set_yscale('log', base=2)
        ax3.set_xticks(worker_counts)
        ax3.set_xticklabels(worker_counts)
        ax3.legend()
        
        # 4. Parallel Efficiency
        ax4 = plt.subplot(2, 3, 4)
        efficiency = [s / w for s, w in zip(speedups, worker_counts)]
        ax4.plot(worker_counts, efficiency, 'mo-', linewidth=2, markersize=8)
        ax4.set_xlabel('Number of Workers')
        ax4.set_ylabel('Parallel Efficiency')
        ax4.set_title('Parallel Efficiency (Speedup / Workers)')
        ax4.grid(True, alpha=0.3)
        ax4.set_xscale('log', base=2)
        ax4.set_ylim(0, 1.1)
        ax4.set_xticks(worker_counts)
        ax4.set_xticklabels(worker_counts)
        
        # 5. Total Images Processed vs Workers
        ax5 = plt.subplot(2, 3, 5)
        ax5.plot(worker_counts, total_images, 'co-', linewidth=2, markersize=8)
        ax5.set_xlabel('Number of Workers')
        ax5.set_ylabel('Total Images Processed')
        ax5.set_title('Total Images Processed vs Workers')
        ax5.grid(True, alpha=0.3)
        ax5.set_xscale('log', base=2)
        ax5.set_xticks(worker_counts)
        ax5.set_xticklabels(worker_counts)
        
        # 6. Per-Worker Throughput
        ax6 = plt.subplot(2, 3, 6)
        images_per_second_per_worker = [t / w for t, w in zip(throughputs, worker_counts)]
        ax6.plot(worker_counts, images_per_second_per_worker, 'yo-', linewidth=2, markersize=8)
        ax6.set_xlabel('Number of Workers')
        ax6.set_ylabel('Images/Second per Worker')
        ax6.set_title('Per-Worker Throughput')
        ax6.grid(True, alpha=0.3)
        ax6.set_xscale('log', base=2)
        ax6.set_xticks(worker_counts)
        ax6.set_xticklabels(worker_counts)
        
        plt.tight_layout()
        
        # Save the comprehensive plot
        plot_path = os.path.join(self.log_dir, "benchmark_comprehensive_visualization.png")
        plt.savefig(plot_path, dpi=300, bbox_inches='tight')
        print(f"Comprehensive visualization saved to: {plot_path}")
        
        # Save individual plots
        self.save_individual_plots(successful_results)
        
        plt.close()
    
    def save_individual_plots(self, results: List[Dict]):
        """Save individual plots for detailed analysis"""
        worker_counts = [r['num_workers'] for r in results]
        throughputs = [r['throughput_images_per_second'] for r in results]
        total_times = [r['total_wall_clock_time'] for r in results]
        
        # Throughput plot
        plt.figure(figsize=(10, 6))
        plt.plot(worker_counts, throughputs, 'bo-', linewidth=3, markersize=10)
        plt.xlabel('Number of Parallel Workers', fontsize=14)
        plt.ylabel('Throughput (images/second)', fontsize=14)
        plt.title('S3 Image Resizing Throughput vs Parallel Workers (Pre-divided)', fontsize=16)
        plt.grid(True, alpha=0.3)
        plt.xscale('log', base=2)
        plt.xticks(worker_counts, worker_counts)
        
        # Add annotations
        for w, t in zip(worker_counts, throughputs):
            plt.annotate(f'{t:.1f}', (w, t), textcoords="offset points", 
                        xytext=(0,10), ha='center', fontsize=10)
        
        plt.tight_layout()
        plt.savefig(os.path.join(self.log_dir, "throughput_vs_workers_predivided.png"), 
                   dpi=300, bbox_inches='tight')
        plt.close()
        
        # Processing time plot
        plt.figure(figsize=(10, 6))
        plt.plot(worker_counts, total_times, 'ro-', linewidth=3, markersize=10)
        plt.xlabel('Number of Parallel Workers', fontsize=14)
        plt.ylabel('Total Processing Time (seconds)', fontsize=14)
        plt.title('S3 Image Resizing Time vs Parallel Workers (Pre-divided)', fontsize=16)
        plt.grid(True, alpha=0.3)
        plt.xscale('log', base=2)
        plt.xticks(worker_counts, worker_counts)
        
        # Add annotations
        for w, t in zip(worker_counts, total_times):
            plt.annotate(f'{t:.1f}s', (w, t), textcoords="offset points", 
                        xytext=(0,10), ha='center', fontsize=10)
        
        plt.tight_layout()
        plt.savefig(os.path.join(self.log_dir, "processing_time_vs_workers_predivided.png"), 
                   dpi=300, bbox_inches='tight')
        plt.close()


class UnixSocketHTTPConnection:
    """HTTP client over Unix socket"""
    def __init__(self, socket_path: str):
        self.socket_path = socket_path
        self.sock = None
    
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(200.0)  # Increased timeout
        self.sock.connect(self.socket_path)
    
    def close(self):
        if self.sock:
            self.sock.close()
            self.sock = None
    
    def request(self, method: str, path: str, body: str = None, 
               headers: Dict[str, str] = None) -> Tuple[int, Dict[str, str], str]:
        if not self.sock:
            self.connect()
        
        req_headers = {
            'Host': 'localhost',
            'Content-Type': 'application/json',
            'Connection': 'close'
        }
        if headers:
            req_headers.update(headers)
        
        if body:
            req_headers['Content-Length'] = str(len(body.encode('utf-8')))
        
        request_lines = [f"{method} {path} HTTP/1.1"]
        for key, value in req_headers.items():
            request_lines.append(f"{key}: {value}")
        request_lines.append("")
        if body:
            request_lines.append(body)
        
        request_str = "\r\n".join(request_lines)
        self.sock.sendall(request_str.encode('utf-8'))
        
        response_data = b""
        while True:
            chunk = self.sock.recv(4096)
            if not chunk:
                break
            response_data += chunk
            if b"\r\n\r\n" in response_data:
                break
        
        response_str = response_data.decode('utf-8')
        lines = response_str.split('\r\n')
        
        status_line = lines[0]
        status_code = int(status_line.split()[1])
        
        response_headers = {}
        i = 1
        while i < len(lines) and lines[i]:
            if ':' in lines[i]:
                key, value = lines[i].split(':', 1)
                response_headers[key.strip()] = value.strip()
            i += 1
        
        body_start = i + 1
        if body_start < len(lines):
            response_body = '\r\n'.join(lines[body_start:])
        else:
            response_body = ""
        
        self.close()
        return status_code, response_headers, response_body


def main():
    parser = argparse.ArgumentParser(description='S3 Image Resize Benchmark for WASM Containers (using pre-divided directories)')
    parser.add_argument('--server-binary', 
                       default="/home/mnm/work/wasmserverless/runtime/target/release/wasm-serverless",
                       help='Path to the WASM server binary')
    parser.add_argument('--s3-wasm', 
                       default="/home/mnm/work/wasmserverless/image_resizer_s3.wasm",
                       help='Path to the S3 image resizer WASM module')
    parser.add_argument('--s3-bucket', 
                       default="wasmcontainer",
                       help='S3 bucket for testing')
    
    args = parser.parse_args()
    
    # Validate inputs
    if not os.path.exists(args.server_binary):
        print(f"Error: Server binary not found: {args.server_binary}")
        return 1
    
    if not os.path.exists(args.s3_wasm):
        print(f"Error: S3 WASM module not found: {args.s3_wasm}")
        return 1
    
    # Create benchmark instance
    benchmark = ImageResizeBenchmark(
        args.server_binary,
        args.s3_wasm,
        args.s3_bucket
    )
    
    try:
        # Run benchmark using pre-created directories
        benchmark.run_full_benchmark()
        print("Benchmark completed successfully!")
        return 0
    except Exception as e:
        print(f"Benchmark failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

if __name__ == "__main__":
    exit(main())