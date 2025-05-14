wasm-objdump -x hello.wasm

tinygo build -o hello_world.wasm -target wasi main.go
curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/init \
  -H "Content-Type: application/json" \
  -d '{
      "wasm_path": "/home/mnm/work/wasmserverless/go/hello_world.wasm"
  }'

cargo build --target wasm32-wasip1 --release
wasm-opt -Oz /home/mnm/work/wasmserverless/rust/hello_world/hello_wasm.wasm  -o hello_optimized.wasm

curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/init \
  -H "Content-Type: application/json" \
  -d '{
      "wasm_path": "/home/mnm/work/wasmserverless/hello_world.wasm"
  }'
curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/init \
  -H "Content-Type: application/json" \
  -d '{
      "wasm_path": "/home/mnm/work/wasmserverless/api_aggregator.wasm"
  }'
curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/init \
  -H "Content-Type: application/json" \
  -d '{
      "wasm_path": "/home/mnm/work/wasmserverless/primes_sieve.wasm"
  }'

curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/run \
-H "Content-Type: application/json" \
-d '{
  "module_id": "module_1"
}'

curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/run \
-H "Content-Type: application/json" \
-d '{
  "module_id": "module_1",
  "args" : ["prog", "50"]
}'

curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/init \
  -H "Content-Type: application/json" \
  -d '{
      "wasm_path": "/home/mnm/work/wasmserverless/rust/memory_allocator/memory_allocator.wasm"
  }'

curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/run \
-H "Content-Type: application/json" \
-d '{
  "instance_id": "1",
  "args": ["2048"]
}'

curl --unix-socket /tmp/wasm-serverless.sock -X GET http://localhost/memory-detailed/1 \
-H "Content-Type: application/json" \
-d '{
}'
