[![Build](https://github.com/tke1973/rustslinger/actions/workflows/build.yml/badge.svg)](https://github.com/tke1973/rrustslinger/actions/workflows/build.yml)
# rustslinger

**rustslinger** is a tool for scanning and analysing large image data sets stored in AWS S3 buckets. 

## Why?

**rustslinger** is a fully functional, non-trivial learning and experimenting application written to get familiar with the Rust programming language. 

Key concepts and technologies used in **rustslinger** include:

- Conncurrency & Mutlitheading with async/await and threadpools using [futures](https://crates.io/crates/futures) and [Tokio](https://crates.io/crates/tokio)
- AWS Rust SDK using [aws-sdk-s3](https://crates.io/crates/aws-sdk-s3) and [aws_config](https://crates.io/crates/aws-config)
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


