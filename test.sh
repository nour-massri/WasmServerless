  tinygo build -o hello.wasm -target wasi main.go

curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/init \
  -H "Content-Type: application/json" \
  -d '{
      "module_path": "/home/mnm/work/wasmserverless/go/hello.wasm"
  }'

cargo build --target wasm32-wasip1

wasmtime /home/mnm/work/wasmserverless/hello_wasm/target/wasm32-wasip1/debug/hello_wasm.wasm
  curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/init \
  -H "Content-Type: application/json" \
  -d '{
      "module_path": "/home/mnm/work/wasmserverless/hello_wasm/target/wasm32-wasip1/release/hello_wasm.wasm"
  }'

  wasm-opt -Oz /home/mnm/work/wasmserverless/hello_wasm/target/wasm32-wasip1/release/hello_wasm.wasm  -o hello_optimized.wasm

  curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/init \
  -H "Content-Type: application/json" \
  -d '{
      "module_path": "/home/mnm/work/wasmserverless/hello_wasm/hello_optimized.wasm"
  }'


  curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/run \
  -H "Content-Type: application/json" \
  -d '{
    "instance_id": "1"
  }'

wasm-objdump -x hello.wasm