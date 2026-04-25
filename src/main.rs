//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//
use anyhow::{bail, Result};
use clap::Parser;

use std::env;
use std::path::PathBuf;

use aws_config::meta::region::RegionProviderChain;
use aws_config::profile::credentials::ProfileFileCredentialsProvider;
use aws_sdk_s3::Client;

mod error;
pub use crate::error::RustslingerError;

mod download;
pub use crate::download::DownloadFile;

mod analysis;
pub use crate::analysis::{AnalyticsResult, AnalyticsResultSet};

/// rustslinger
///
/// rustslinger is a tool for scanning and analysing large image data sets stored on AWS S3 buckets.
/// A fully functional, non-trivial, learning and experimentation application written to get familiar with the Rust programming language.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// aws s3 bucket
    #[arg(short, long, required = true)]
    bucket: String,

    /// aws s3 prefix
    #[arg(short, long, required = false)]
    prefix: Option<String>,

    /// aws s3 profile
    #[arg(short = 'f', long, required = false)]
    profile: Option<String>,

    /// aws s3 bucket list
    #[arg(short = 'l', long, required = false)]
    bucketlist: bool,

    /// Path to directory containing wechat_qrcode model files:
    /// detect.prototxt, detect.caffemodel, sr.prototxt, sr.caffemodel.
    /// Only used when built with --features wechat. Falls back to the
    /// lightweight built-in detector if files are absent.
    #[arg(short = 'm', long, required = false)]
    model_path: Option<PathBuf>,
}

async fn list_s3buckets(client: &Client) -> Result<(), RustslingerError> {
    let list_buckets = client
        .list_buckets()
        .send()
        .await
        .map_err(|_| RustslingerError::ListBuckets)?;

    let bucket_names = match list_buckets.buckets() {
        Some(b) => b,
        None => {
            println!("No buckets found.");
            return Ok(());
        }
    };

    for bucket_name in bucket_names {
        if let Some(name) = bucket_name.name() {
            println!("{}", name);
        }
    }

    println!("\nFound {} buckets.", bucket_names.len());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    console_subscriber::init();
    let args = Args::parse();

    let region_provider = RegionProviderChain::default_provider().or_else("ap-southeast-1");
    let config = aws_config::from_env().region(region_provider);

    let profile = args.profile.or_else(|| {
        env::var_os("AWS_DEFAULT_PROFILE").map(|s| s.to_string_lossy().into_owned())
    });

    let config = if let Some(profile_string) = profile {
        println!("PROFILE: Using profile {}.", profile_string);
        config.credentials_provider(
            ProfileFileCredentialsProvider::builder()
                .profile_name(&profile_string)
                .build(),
        )
    } else {
        println!("PROFILE: No profile specified. Using default credentials provider.");
        config
    };

    let config = config.load().await;
    let client = Client::new(&config);

    if args.bucketlist && list_s3buckets(&client).await.is_err() {
        bail!("Can't list s3 buckets.");
    };

    println!("Setup streaming files from bucket {}.", args.bucket);
    let handle_data = crate::download::download_files_tasker(client.clone(), &args.bucket).await;

    println!("Analyzing files.");
    let mut analysis_results = AnalyticsResult::new(handle_data, args.model_path).await;

    println!("Waiting for results.");
    let mut rc: u128 = 0;
    while let Some(result) = analysis_results.get_next().await {
        rc += 1;
        println!(
            "{}, {}, {}, {}, {}, {}",
            rc, result.key, result.hash, result.qr_code, result.qr_quality, result.qr_source
        );
    }

    println!("Done with it!");
    Ok(())
}
