[![Build](https://github.com/tke1973/rustslinger/actions/workflows/build.yml/badge.svg)](https://github.com/tke1973/rustslinger/actions/workflows/build.yml)
# rustslinger

**rustslinger** is a tool for scanning and analysing large image data sets stored on AWS S3 buckets. 

**_It is in pre-alpha state. Use with caution and entirely at your own risk!_**


```
rustslinger  --help

rustslinger is a tool for scanning and analysing large image data sets stored in AWS S3 buckets.

Usage: rustslinger [OPTIONS] --bucket <BUCKET>

Options:
  -b, --bucket <BUCKET>    aws s3 bucket
  -p, --prefix <PREFIX>    aws s3 prefix
  -f, --profile <PROFILE>  aws s3 profile
  -l, --bucketlist         aws s3 bucket list
  -h, --help               Print help information
  -V, --version            Print version information
```

## Why?

**rustslinger** is a fully functional, non-trivial, learning and experimentation application written to get familiar with the Rust programming language. 

Key concepts and technologies used to develop **rustslinger** include:

- Concurrency & Multithreading with async/await and threadpools using [futures](https://crates.io/crates/futures) and [tokio](https://crates.io/crates/tokio)
- AWS Rust SDK using [aws-sdk-s3](https://crates.io/crates/aws-sdk-s3) and [aws-config](https://crates.io/crates/aws-config)
- QR Code Scanning using [rqrr](https://crates.io/crates/rqrr) and [image](https://crates.io/crates/image)
- EXIF Metadata Extraction using [kamadak-exif](https://crates.io/crates/kamadak-exif)
- Structured Error Handling using [thiserror](https://crates.io/crates/thiserror) and [anyhow](https://crates.io/crates/anyhow)
- Structured Diagnostic Logging using [tracing](https://crates.io/crates/tracing)
- Cryptographic Hashing using [sha2](https://crates.io/crates/sha2) and [hex](https://crates.io/crates/hex)
- Unit Testing with Tokio async test support
- and more ...

## How it works

**rustslinger** processes images through a three-stage async pipeline:

1. **Download** — paginates through all objects in the target S3 bucket and downloads them concurrently, throttled by a semaphore (`num_cpus × 10` permits) to avoid overwhelming the network or the AWS API.

2. **Analyse** — as each download completes, the download permit is released and a CPU-bound analysis task is spawned on a blocking thread pool (throttled separately to `num_cpus` permits). Each image is analysed for:
   - **SHA-256 hash** of the raw bytes
   - **QR codes** via `rqrr` — the image is decoded, resized to 800×600, and scanned for QR grids
   - **EXIF `UserComment`** field via `kamadak-exif` — extracted independently of QR results

3. **Output** — results are streamed back to the main thread via an unbounded channel and printed as CSV-style rows:

```
index, key, hash, qr_code, qr_quality, qr_source
```

Each image can produce multiple result rows — one per QR code found, plus one if an EXIF `UserComment` is present.

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
