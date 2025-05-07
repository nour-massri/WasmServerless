  tinygo build -o hello.wasm -target wasi main.go

curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/create \
  -H "Content-Type: application/json" \
  -d '{
      "module_path": "/home/mnm/work/wasmserverless/go/hello.wasm",
    "env": {
      "WORKER_ID": "1"
    },
    "args": ["map"]
  }'

cargo build --target wasm32-wasip1

wasmtime /home/mnm/work/wasmserverless/hello_wasm/target/wasm32-wasip1/debug/hello_wasm.wasm
  curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/create \
  -H "Content-Type: application/json" \
  -d '{
      "module_path": "/home/mnm/work/wasmserverless/hello_wasm/target/wasm32-wasip1/debug/hello_wasm.wasm",
    "env": {
      "WORKER_ID": "1"
    },
    "args": ["map"]
  }'

  wasm-opt -Oz /home/mnm/work/wasmserverless/hello_wasm/target/wasm32-wasip1/debug/hello_wasm.wasm  -o hello_optimized.wasm

  curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/create \
  -H "Content-Type: application/json" \
  -d '{
      "module_path": "/home/mnm/work/wasmserverless/hello_wasm/hello_optimized.wasm",
    "env": {
      "WORKER_ID": "1"
    },
    "args": ["map"]
  }'


  curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/run \
  -H "Content-Type: application/json" \
  -d '{
    "instance_id": "6b6ebc29-cb7b-4a6a-871b-0ac6b0a73f76"
  }'

wasm-objdump -x hello.wasm