curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/create \
  -H "Content-Type: application/json" \
  -d '{
      "module_path": "/home/mnm/work/wasmserverless/go/hello.wasm",
    "env": {
      "WORKER_ID": "1"
    },
    "args": ["map"]
  }'

curl --unix-socket /tmp/wasm-serverless.sock -X POST http://localhost/run \
  -H "Content-Type: application/json" \
  -d '{
    "instance_id": "6830a827-3118-499b-8a90-88ef3bf4b3cd"
  }'