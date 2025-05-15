sudo apt install pkg-config libssl-dev

# Setup S3 dataset
python3 setup_s3_dataset.py --bucket-name wasmcontainer --num-images 1000

