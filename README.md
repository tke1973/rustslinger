[![Build](https://github.com/tke1973/rustslinger/actions/workflows/build.yml/badge.svg)](https://github.com/tke1973/rustslinger/actions/workflows/build.yml)
# rustslinger

**rustslinger** is a tool for scanning and analysing large image data sets stored on AWS S3 buckets. 

**_It is in pre-alpha state. Use with caution and entirely at your own risk!_**


```
rustslinger  --help

rustslinger is a tool for scanning and analysing large image data sets stored in AWS S3 buckets.

Usage: rustslinger [OPTIONS] --bucket <BUCKET>

Options:
  -b, --bucket <BUCKET>        aws s3 bucket
  -p, --prefix <PREFIX>        aws s3 prefix
  -f, --profile <PROFILE>      aws s3 profile
  -l, --bucketlist             aws s3 bucket list
  -m, --model-path <PATH>      path to wechat_qrcode model files directory
      --dynamsoft-license      Dynamsoft Barcode Reader license key (--features dynamsoft)
      --dynamsoft-endpoint     Dynamsoft REST API endpoint URL (--features dynamsoft)
      --dynamsoft-only         use Dynamsoft only — skip rxing and wechat (--features dynamsoft)
  -h, --help                   Print help information
  -V, --version                Print version information
```

## Why?

**rustslinger** is a fully functional, non-trivial, learning and experimentation application written to get familiar with the Rust programming language. 

Key concepts and technologies used to develop **rustslinger** include:

- Concurrency & Multithreading with async/await and threadpools using [futures](https://crates.io/crates/futures) and [tokio](https://crates.io/crates/tokio)
- AWS Rust SDK using [aws-sdk-s3](https://crates.io/crates/aws-sdk-s3) and [aws-config](https://crates.io/crates/aws-config)
- QR Code Scanning using [rxing](https://crates.io/crates/rxing) (pure-Rust ZXing port) and [image](https://crates.io/crates/image), with optional [OpenCV wechat_qrcode](https://docs.opencv.org/4.x/d5/d04/classcv_1_1wechat__qrcode_1_1WeChatQRCode.html) as a second-pass backend and optional [Dynamsoft Barcode Reader](https://www.dynamsoft.com/barcode-reader/overview/) REST API as a third-pass backend
- EXIF Metadata Extraction using [kamadak-exif](https://crates.io/crates/kamadak-exif)
- Structured Error Handling using [thiserror](https://crates.io/crates/thiserror) and [anyhow](https://crates.io/crates/anyhow)
- Structured Diagnostic Logging using [tracing](https://crates.io/crates/tracing)
- Cryptographic Hashing using [sha2](https://crates.io/crates/sha2) and [hex](https://crates.io/crates/hex)
- Unit Testing with Tokio async test support
- and more ...

## How it works

**rustslinger** processes images through a three-stage async pipeline:

1. **Download** — paginates through all objects in the target S3 bucket and downloads them concurrently, throttled by a semaphore (`num_cpus × 10` permits) to avoid overwhelming the network or the AWS API.

2. **Analyse** — as each download completes, the download permit is released and a CPU-bound analysis task is spawned on a blocking thread pool (throttled separately to `num_cpus` permits). Each image is analysed in this order:
   - **SHA-256 hash** of the raw bytes
   - **EXIF `UserComment`** field via `kamadak-exif` — extracted first, independently of QR results
   - **QR codes** — via `rxing` (always), plus `wechat_qrcode` as a second pass when built with `--features wechat`, and optionally Dynamsoft Barcode Reader REST API as a third pass when built with `--features dynamsoft`; all results are deduplicated by content across backends

3. **Output** — results are streamed back to the main thread via an unbounded channel and printed as CSV-style rows:

```
index, key, hash, qr_code, qr_quality, qr_source
```

Each image can produce multiple result rows — one per QR code found, plus one if an EXIF `UserComment` is present. The `qr_source` field identifies which backend found the code: `rxing`, `wechat_qrcode`, `dynamsoft`, or `EXIFUserComment`.

## QR Code Backends

### Default — rxing (pure Rust, no extra dependencies)

`rxing` is a pure-Rust port of ZXing with native multi-code support. It runs on every build with `TryHarder` enabled to maximise detection coverage.

```bash
cargo build --release
```

### Optional — OpenCV wechat_qrcode (second-pass backend)

When built with `--features wechat`, `wechat_qrcode` runs after `rxing` and contributes any QR codes not already found — particularly useful for rotated, blurry, low-contrast, or partially occluded codes. Results from both backends are merged and deduplicated by content. It requires OpenCV 4.x with contrib modules installed.

**macOS:**
```bash
brew install opencv
cargo build --release --features wechat
```

**With CNN model files (best quality):**

Download the four model files from the [OpenCV contrib test data repository](https://github.com/opencv/opencv_contrib/tree/master/modules/wechat_qrcode/src/zxing/qrcode):
- `detect.prototxt` + `detect.caffemodel`
- `sr.prototxt` + `sr.caffemodel`

Then pass the directory at runtime:

```bash
rustslinger --bucket my-bucket --model-path /path/to/models
```

Without `--model-path`, the wechat backend uses its built-in lightweight detector automatically.

### Optional — Dynamsoft Barcode Reader (REST API, third-pass backend)

When built with `--features dynamsoft`, **rustslinger** can call a Dynamsoft Barcode Reader service via its REST API. Dynamsoft is particularly effective on damaged, low-resolution, or unusual barcode formats. It runs as a third pass after rxing and wechat, contributing any codes not already found. All results are merged and deduplicated by content.

Requires a running [Dynamsoft Barcode Reader](https://www.dynamsoft.com/barcode-reader/overview/) service instance and a valid license key.

```bash
cargo build --release --features dynamsoft
```

**Third-pass (after rxing + wechat):**

```bash
rustslinger --bucket my-bucket \
  --dynamsoft-license YOUR_LICENSE \
  --dynamsoft-endpoint http://localhost:18622/api/dbr/read
```

**Dynamsoft only (skip rxing and wechat):**

```bash
rustslinger --bucket my-bucket \
  --dynamsoft-license YOUR_LICENSE \
  --dynamsoft-endpoint http://localhost:18622/api/dbr/read \
  --dynamsoft-only
```

## Authentication

**rustslinger** resolves AWS credentials in the following order:

1. `--profile <name>` command line option
2. `AWS_DEFAULT_PROFILE` environment variable
3. Default AWS SDK credential chain (environment variables, instance profile, etc.)

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
