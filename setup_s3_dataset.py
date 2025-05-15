#!/usr/bin/env python3
import boto3
import os
import sys
import argparse
import json
from concurrent.futures import ThreadPoolExecutor, as_completed
import requests
from urllib.parse import urlparse
import hashlib
import time
import random

def get_aws_region():
    """Get AWS region from credentials or default"""
    # Try to get region from boto3 session (reads from ~/.aws/config)
    session = boto3.Session()
    region = session.region_name
    if region:
        return region
    
    # Fallback to us-east-1 if no region configured
    print("Warning: No default region found in ~/.aws/config, using us-east-1")
    return 'us-east-1'

def setup_s3_bucket(bucket_name, region=None):
    """Create S3 bucket and set up IAM policies"""
    # Use default region if none provided
    if region is None:
        region = get_aws_region()
    
    # Create S3 client (will automatically use credentials from ~/.aws/credentials)
    s3 = boto3.client('s3', region_name=region)
    
    try:
        if region == 'us-east-1':
            s3.create_bucket(Bucket=bucket_name)
        else:
            s3.create_bucket(
                Bucket=bucket_name,
                CreateBucketConfiguration={'LocationConstraint': region}
            )
        print(f"Created bucket: {bucket_name} in region: {region}")
    except s3.exceptions.BucketAlreadyExists:
        print(f"Bucket {bucket_name} already exists")
    except Exception as e:
        print(f"Error creating bucket: {e}")
        return False
    
    # Set bucket policy for programmatic access
    policy = {
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "AllowImageResizingAccess",
                "Effect": "Allow",
                "Principal": "*",
                "Action": [
                    "s3:GetObject",
                    "s3:PutObject",
                    "s3:DeleteObject",
                    "s3:ListBucket"
                ],
                "Resource": [
                    f"arn:aws:s3:::{bucket_name}",
                    f"arn:aws:s3:::{bucket_name}/*"
                ]
            }
        ]
    }
    
    try:
        s3.put_bucket_policy(
            Bucket=bucket_name,
            Policy=json.dumps(policy)
        )
        print(f"Set bucket policy for {bucket_name}")
    except Exception as e:
        print(f"Warning: Could not set bucket policy: {e}")
    
    return True

def download_open_images_subset(output_dir, num_images=1000):
    """Download a subset of Open Images dataset"""
    # This is a simplified version - in practice you'd use the official Open Images tools
    
    # Create directory
    os.makedirs(output_dir, exist_ok=True)
    
    # For demo purposes, we'll download from Unsplash (requires API key in production)
    # This is just an example - you should use proper dataset sources
    
    urls = []
    # You would replace this with actual Open Images URLs
    for i in range(num_images):
        # Example using Lorem Picsum for testing
        urls.append(f"https://picsum.photos/1920/1080?random={i}")
    
    def download_image(url, idx):
        try:
            response = requests.get(url, timeout=30)
            if response.status_code == 200:
                filename = f"image_{idx:06d}.jpg"
                filepath = os.path.join(output_dir, filename)
                with open(filepath, 'wb') as f:
                    f.write(response.content)
                return filepath
        except Exception as e:
            print(f"Error downloading {url}: {e}")
            return None
    
    print(f"Downloading {num_images} images...")
    with ThreadPoolExecutor(max_workers=10) as executor:
        futures = [executor.submit(download_image, url, i) for i, url in enumerate(urls)]
        
        for i, future in enumerate(as_completed(futures)):
            result = future.result()
            if result:
                print(f"Downloaded: {result} ({i+1}/{num_images})")
            else:
                print(f"Failed to download image {i+1}")

def distribute_images_to_workers(image_files, num_workers):
    """Distribute images evenly among workers"""
    # Shuffle images for better distribution
    random.shuffle(image_files)
    
    # Calculate images per worker
    images_per_worker = len(image_files) // num_workers
    extra_images = len(image_files) % num_workers
    
    batches = []
    start_idx = 0
    
    for i in range(num_workers):
        # Some workers get an extra image if total doesn't divide evenly
        worker_images = images_per_worker + (1 if i < extra_images else 0)
        end_idx = start_idx + worker_images
        batch = image_files[start_idx:end_idx]
        batches.append(batch)
        start_idx = end_idx
    
    return batches

def create_worker_directories(bucket_name, image_files):
    """Create pre-divided worker directories in S3"""
    s3 = boto3.client('s3')
    worker_counts = [1, 2, 4, 8, 16, 32, 64]
    
    print("Creating worker directories and distributing images...")
    
    for num_workers in worker_counts:
        print(f"\nSetting up directories for {num_workers} workers...")
        
        # Distribute images among workers
        image_batches = distribute_images_to_workers(image_files.copy(), num_workers)
        
        # Create worker-specific directories and upload images
        for worker_id in range(num_workers):
            worker_prefix = f"{num_workers}worker_{worker_id + 1}"
            input_prefix = f"{worker_prefix}/input/"
            output_prefix = f"{worker_prefix}/output/"
            
            # Create empty marker file for output directory
            s3.put_object(
                Bucket=bucket_name,
                Key=f"{output_prefix}.keep",
                Body=b""
            )
            
            # Upload images for this worker
            batch = image_batches[worker_id]
            print(f"  Worker {worker_id + 1}: uploading {len(batch)} images...")
            
            for local_path in batch:
                filename = os.path.basename(local_path)
                s3_key = f"{input_prefix}{filename}"
                
                try:
                    s3.upload_file(local_path, bucket_name, s3_key)
                except Exception as e:
                    print(f"    Error uploading {local_path}: {e}")
            
            print(f"    Completed worker {worker_id + 1}/{num_workers}")
    
    print(f"\nWorker directories created successfully!")

def upload_to_s3(local_dir, bucket_name, s3_prefix='input/'):
    """Upload local images to S3"""
    # Use default credentials from ~/.aws/credentials
    s3 = boto3.client('s3')
    
    uploaded_count = 0
    total_size = 0
    image_files = []
    
    # First, collect all image files
    for root, dirs, files in os.walk(local_dir):
        for file in files:
            if file.lower().endswith(('.jpg', '.jpeg', '.png', '.gif', '.bmp')):
                local_path = os.path.join(root, file)
                image_files.append(local_path)
    
    # Upload all images to the main input directory
    print(f"Uploading {len(image_files)} images to main input directory...")
    for local_path in image_files:
        file = os.path.basename(local_path)
        s3_key = s3_prefix + file
        
        try:
            # Get file size
            file_size = os.path.getsize(local_path)
            total_size += file_size
            
            s3.upload_file(local_path, bucket_name, s3_key)
            uploaded_count += 1
            print(f"Uploaded: {s3_key} ({file_size / 1024 / 1024:.2f} MB)")
        except Exception as e:
            print(f"Error uploading {local_path}: {e}")
    
    print(f"\nMain upload complete:")
    print(f"Files uploaded: {uploaded_count}")
    print(f"Total size: {total_size / 1024 / 1024 / 1024:.2f} GB")
    
    # Now create worker-specific directories
    create_worker_directories(bucket_name, image_files)
    
    return uploaded_count, total_size

def create_iam_user(username='wasm-image-resizer'):
    """Create IAM user for the application"""
    # Use default credentials from ~/.aws/credentials
    iam = boto3.client('iam')
    
    # Create user
    try:
        iam.create_user(UserName=username)
        print(f"Created IAM user: {username}")
    except iam.exceptions.EntityAlreadyExistsException:
        print(f"IAM user {username} already exists")
    
    # Create and attach policy
    policy_document = {
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [
                    "s3:GetObject",
                    "s3:PutObject",
                    "s3:DeleteObject",
                    "s3:ListBucket"
                ],
                "Resource": "*"
            }
        ]
    }
    
    policy_name = f"{username}-s3-policy"
    try:
        iam.create_policy(
            PolicyName=policy_name,
            PolicyDocument=json.dumps(policy_document)
        )
        print(f"Created policy: {policy_name}")
    except iam.exceptions.EntityAlreadyExistsException:
        print(f"Policy {policy_name} already exists")
    
    # Attach policy to user
    try:
        policy_arn = f"arn:aws:iam::{boto3.client('sts').get_caller_identity()['Account']}:policy/{policy_name}"
        iam.attach_user_policy(
            UserName=username,
            PolicyArn=policy_arn
        )
        print(f"Attached policy to user")
    except Exception as e:
        print(f"Error attaching policy: {e}")
    
    # Create access keys
    try:
        response = iam.create_access_key(UserName=username)
        access_key = response['AccessKey']
        
        print(f"\nAccess keys created:")
        print(f"Access Key ID: {access_key['AccessKeyId']}")
        print(f"Secret Access Key: {access_key['SecretAccessKey']}")
        print(f"\nSave these credentials securely!")
        
        return access_key['AccessKeyId'], access_key['SecretAccessKey']
    except Exception as e:
        print(f"Error creating access keys: {e}")
        return None, None

def main():
    parser = argparse.ArgumentParser(description='Setup S3 dataset for image resizing benchmark')
    parser.add_argument('--bucket-name', default="wasmcontainer", help='S3 bucket name')
    parser.add_argument('--region', help='AWS region (optional, uses default from ~/.aws/config)')
    parser.add_argument('--num-images', type=int, default=500, help='Number of images to download')
    parser.add_argument('--local-dir', default='./dataset', help='Local directory for images')
    parser.add_argument('--skip-download', action='store_true', help='Skip downloading images')
    parser.add_argument('--skip-upload', action='store_true', help='Skip uploading to S3')
    parser.add_argument('--create-iam', action='store_true', help='Create IAM user and access keys')
    parser.add_argument('--worker-counts', nargs='+', type=int, default=[1, 2, 4, 8, 16, 32, 64], 
                       help='Worker counts to create directories for')
    
    args = parser.parse_args()
    
    # Get the region to use
    region = args.region or get_aws_region()
    print(f"Using AWS region: {region}")
    
    # Create IAM user if requested
    if args.create_iam:
        print("Creating IAM user...")
        access_key_id, secret_access_key = create_iam_user()
        if access_key_id:
            print(f"\nExport these environment variables:")
            print(f"export AWS_ACCESS_KEY_ID={access_key_id}")
            print(f"export AWS_SECRET_ACCESS_KEY={secret_access_key}")
            print(f"export AWS_REGION={region}")
    
    # # Setup S3 bucket
    print(f"Setting up S3 bucket: {args.bucket_name}")
    if not setup_s3_bucket(args.bucket_name, region):
        print("Failed to setup S3 bucket")
        return
    
    # Download images
    if not args.skip_download:
        print(f"Downloading {args.num_images} images to {args.local_dir}")
        download_open_images_subset(args.local_dir, args.num_images)

    # Upload to S3 and create worker directories
    if not args.skip_upload:
        print(f"Uploading images from {args.local_dir} to s3://{args.bucket_name}/input/")
        upload_to_s3(args.local_dir, args.bucket_name)
    
    print("\nSetup complete!")
    print("\nWorker directory structure created:")
    for num_workers in [1, 2, 4, 8, 16, 32]:
        print(f"  {num_workers} workers: {num_workers}worker_1, {num_workers}worker_2, ..., {num_workers}worker_{num_workers}")

if __name__ == "__main__":
    main()