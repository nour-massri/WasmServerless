#!/bin/bash
# benchmark_memory_overhead.sh
# Measures memory overhead for different numbers of instances

OUTPUT_FILE="memory_overhead_results.csv"
WASM_MODULE="./hello_world.wasm"

echo "num_instances,total_memory_kb,per_instance_memory_kb" > $OUTPUT_FILE

# Get baseline memory usage
baseline=$(ps -o rss= -p $(pgrep -f "wasm-serverless"))

echo "Baseline memory usage: $baseline KB"

# Test with increasing number of instances
for num_instances in 1 5 10 25 50 100; do
    echo "Testing with $num_instances instances..."
    
    # Array to store instance IDs
    instance_ids=()
    
    # Create instances
    for i in $(seq 1 $num_instances); do
        response=$(curl -s -X POST http://localhost:8080/init \
            -H "Content-Type: application/json" \
            -d "{\"module_path\": \"$WASM_MODULE\"}")
        
        instance_id=$(echo $response | jq -r .instance_id)
        instance_ids+=($instance_id)
    done
    
    # Wait a moment for memory to stabilize
    sleep 2
    
    # Measure memory
    current_memory=$(ps -o rss= -p $(pgrep -f "wasm-serverless"))
    memory_overhead=$((current_memory - baseline))
    per_instance=$((memory_overhead / num_instances))
    
    echo "$num_instances,$memory_overhead,$per_instance" >> $OUTPUT_FILE
    
    # Cleanup instances
    for id in "${instance_ids[@]}"; do
        curl -s -X DELETE http://localhost:8080/instance/$id > /dev/null
    done
    
    # Wait for cleanup
    sleep 2
done

echo "Memory overhead benchmark complete. Results saved to $OUTPUT_FILE"