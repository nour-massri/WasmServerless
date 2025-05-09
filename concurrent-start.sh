#!/bin/bash
# benchmark_concurrent_cold_start.sh
# Measures concurrent cold start times

OUTPUT_FILE="concurrent_cold_start_results.csv"
WASM_MODULE="./hello_world.wasm"

echo "concurrency,timestamp,instance_id,total_cold_start_time_us,module_load_time_us,module_compile_time_us,instantiation_time_us,execution_time_us,memory_usage_kb" > $OUTPUT_FILE

# Test with increasing concurrency
for concurrency in 1 2 4 8 16 32; do
    echo "Testing with concurrency: $concurrency"
    
    # Create temporary file for concurrent outputs
    temp_file=$(mktemp)
    
    # Start concurrent requests
    for i in $(seq 1 $concurrency); do
        (
            response=$(curl -s -X POST http://localhost:8080/coldstart \
                -H "Content-Type: application/json" \
                -d "{\"module_path\": \"$WASM_MODULE\"}")
            
            # Extract metrics from response
            instance_id=$(echo $response | jq -r .instance_id)
            total_time=$(echo $response | jq -r .metrics.total_cold_start_time_us)
            module_load=$(echo $response | jq -r .metrics.module_load_time_us)
            module_compile=$(echo $response | jq -r .metrics.module_compile_time_us)
            instantiation=$(echo $response | jq -r .metrics.instantiation_time_us)
            execution=$(echo $response | jq -r .metrics.execution_time_us)
            memory=$(echo $response | jq -r .metrics.memory_usage_kb)
            
            # Log results to temp file 
            echo "$concurrency,$(date +%s),$instance_id,$total_time,$module_load,$module_compile,$instantiation,$execution,$memory" >> $temp_file
            
            # Delete the instance
            curl -s -X DELETE http://localhost:8080/instance/$instance_id > /dev/null
        ) &
    done
    
    # Wait for all background jobs to complete
    wait
    
    # Append temp results to output file
    cat $temp_file >> $OUTPUT_FILE
    rm $temp_file
    
    # Sleep briefly to ensure complete cleanup
    sleep 2
done

echo "Concurrent cold start benchmark complete. Results saved to $OUTPUT_FILE"