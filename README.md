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
- and more ...

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


