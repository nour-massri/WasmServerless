#!/bin/bash
# benchmark_serial_cold_start.sh
# Measures serial cold start times

OUTPUT_FILE="serial_cold_start_results.csv"
WASM_MODULE="/home/mnm/work/wasmserverless/hello_world.wasm"

echo "timestamp,instance_id,total_cold_start_time_us,module_load_time_us,module_compile_time_us,instantiation_time_us,execution_time_us,memory_usage_kb" > $OUTPUT_FILE

for i in {1..50}; do
    echo "Running cold start test $i/50..."
    
    # Send request to start a new instance with cold start
    response=$(curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/coldstart \
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
    
    # Log results
    echo "$(date +%s),$instance_id,$total_time,$module_load,$module_compile,$instantiation,$execution,$memory" >> $OUTPUT_FILE
    
    # Delete the instance
    curl --unix-socket /tmp/wasm-serverless.sock -X DELETE http://localhost080/instance/$instance_id > /dev/null
    
    # Sleep briefly to ensure complete cleanup
    sleep 0.5
done

echo "Serial cold start benchmark complete. Results saved to $OUTPUT_FILE"