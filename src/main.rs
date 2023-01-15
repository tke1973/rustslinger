//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//
use anyhow::{bail, Result};
use clap::Parser;

use std::env;

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
}

// test function for downloading all files using tokio runtime

// List all available s3 buckets
async fn list_s3buckets(client: &Client) -> Result<(), RustslingerError> {
    let list_buckets = if let Ok(list_buckets) = client.list_buckets().send().await {
        list_buckets
    } else {
        // This is an error we can't recover from.
        // Therefore, we throw an  error message.
        return Err(RustslingerError::AWSListBucketsOutputError);
    };

    let bucket_names = if let Some(bucket_names) = list_buckets.buckets() {
        bucket_names
    } else {
        println!("NO BUCKETS FOUND.");
        return Ok(());
    };

    for bucket_name in bucket_names {
        if let Some(bucket_name) = bucket_name.name() {
            println!("{}", bucket_name);
        }
    }

    println!();
    println!("Found {} buckets.", bucket_names.len());

    Ok(())
}

// spawn a new thread for each file
// use tokio to download the file
// use sha2 to calculate the hash
#[tokio::main]
async fn main() -> Result<()> {
    // get command line options
    let args = Args::parse();

    let region_provider = RegionProviderChain::default_provider().or_else("ap-southeast-1");
    let config = aws_config::from_env().region(region_provider);

    let profile = if let Some(profile_string) = args.profile {
        Some(profile_string)
    } else {
        env::var_os("AWS_DEFAULT_PROFILE")
            .map(|osprofile_string| osprofile_string.to_string_lossy().to_string())
    };

    // We only use a custom credential_provider if a profile name has been specified.
    // Either by OS environment variable AWS_DEFAULT_PROFILE or by command line option.
    // Otherwaise, the deault credentional provider is used by the sdk.
    let config = if let Some(profile_string) = profile {
        let credentials_provider = ProfileFileCredentialsProvider::builder()
            .profile_name(&profile_string)
            .build();
        println!("PROFILE: Using profile {}.", profile_string);
        config.credentials_provider(credentials_provider)
    } else {
        println!("PROFILE: No profile specified. Using default credentials provider.");
        config
    };

    // Load our aws config abd create a new s3 client.
    let config = config.load().await;
    let client = Client::new(&config);

    // If the user specified the --bucketlist option, we list all available s3 buckets and exit.
    if args.bucketlist && list_s3buckets(&client).await.is_err() {
        bail!("Can't list s3 buckets.");
    };

    println!("Setup streaming files from bucket {}.", args.bucket);
    let handle_data = crate::download::download_files_tasker(client.clone(), &args.bucket).await;

    println!("Analyzing files.");

    let mut analysis_results = AnalyticsResult::new(handle_data).await;

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
