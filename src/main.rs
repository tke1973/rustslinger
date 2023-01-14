//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//

use clap::Parser;

//use std::dbg;
use std::env;
use std::fmt::Write;

use sha2::{Digest, Sha256};

use image::io::Reader as ImageReader;
use std::io::Cursor;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Semaphore;
use tokio::task;
use tokio::task::JoinSet;
use tokio_stream::StreamExt;

use aws_config::meta::region::RegionProviderChain;
use aws_config::profile::credentials::ProfileFileCredentialsProvider;
use aws_sdk_s3::Client;

mod error;
pub use error::RustslingerError;

use anyhow::{bail, Result};

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

struct DownloadFile {
    key: String,
    data: bytes::Bytes,
    permit: tokio::sync::OwnedSemaphorePermit,
}

#[derive(Debug)]
struct AnalyticsResultSet {
    key: String,
    hash: String,
    qr_code: String,
    qr_quality: String,
    gr_source: String,
}

// test function for downloading all files using tokio runtime

async fn download_s3file(
    client: Client,
    key: String,
    bucket: String,
    semaphore: Arc<Semaphore>,
) -> Option<DownloadFile> {
    let permit = if let Ok(permit) = semaphore.acquire_owned().await {
        permit
    } else {
        return None;
    };
    let object_response =
        if let Ok(object_response) = client.get_object().bucket(bucket).key(&key).send().await {
            object_response
        } else {
            drop(permit);
            return None;
        };

    if let Ok(object_response_body) = object_response.body.collect().await {
        let download_file: DownloadFile = DownloadFile {
            key: key,
            data: object_response_body.into_bytes(),
            permit,
        };
        Some(download_file)
    } else {
        drop(permit);
        None
    }
}

async fn download_files_tasker(client: Client, bucket: &str) -> JoinSet<Option<DownloadFile>> {
    let mut downloadfile_joinset = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(num_cpus::get() * 10));

    // Get a list of all objects in the specified s3 bucket paginateing.
    let mut list_s3objects_page = client
        .list_objects_v2()
        .bucket(bucket)
        .into_paginator()
        .send();

    // Iterate over all keys in the s3 bucket.
    while let Some(list_s3objects) = list_s3objects_page.next().await {
        if let Ok(s3objects) = list_s3objects {
            if let Some(s3objects_contents) = s3objects.contents() {
                for object in s3objects_contents {
                    let client = client.clone();
                    let key = object.key().unwrap().to_string().clone();
                    let bucket = bucket.to_string().clone();
                    let semaphore = semaphore.clone();

                    downloadfile_joinset.spawn(download_s3file(client, key, bucket, semaphore));
                }
            }
        }
    }

    downloadfile_joinset
}

fn analytics(
    key: String,
    image_bytes: bytes::Bytes,
    tx: UnboundedSender<AnalyticsResultSet>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let mut hash = Sha256::new();
    hash.update(&image_bytes);
    let hash = hash.finalize();

    let img2 = Cursor::new(image_bytes.clone());

    // print res as serialized hex string
    let mut hash_string = String::new();
    for byte in hash {
        std::write!(&mut hash_string, "{:02x}", byte).unwrap();
    }

    let image_reader = if let Ok(image_reader) =
        ImageReader::new(Cursor::new(&image_bytes)).with_guessed_format()
    {
        image_reader
    } else {
        // This error is unrecoverable and the thread will therefore not produce a result.
        // We end the thread to free CPU and memory resoucres for the next thread.
        return;
    };

    let image_decoded = if let Ok(image_decoded) = image_reader.decode() {
        let a = image_decoded.to_luma8();
        image::imageops::resize(&a, 800, 600, image::imageops::FilterType::Nearest)
    } else {
        // This error is unrecoverable and the thread  will therefore not produce a result.
        // We end the thread to free CPU and memory resoucres for the next thread.
        return;
    };

    let mut image_prepared = rqrr::PreparedImage::prepare(image_decoded);
    let grids = image_prepared.detect_grids();

    for g in grids {
        let message = match g.decode() {
            Ok((_metadata, qrcode)) => AnalyticsResultSet {
                key: key.clone(),
                hash: hash_string.clone(),
                qr_code: qrcode,
                qr_quality: "OK".to_string(),
                gr_source: "rqrr".to_string(),
            },
            Err(error) => AnalyticsResultSet {
                key: key.clone(),
                hash: hash_string.clone(),
                qr_code: "DECODER_ERROR".to_string(),
                qr_quality: error.to_string(),
                gr_source: "rqrr".to_string(),
            },
        };

        if tx.send(message).is_err() {
            // do something!
        }
    }

    let mut a = std::io::BufReader::new(img2);
    let exifreader = exif::Reader::new();
    let exif = if let Ok(exif) = exifreader.read_from_container(&mut a) {
        exif
    } else {
        println!("Error: Could not read exif data from image.");
        return;
    };

    for f in exif.fields() {
        if f.tag.to_string().eq(&"UserComment".to_string()) {
            let message = AnalyticsResultSet {
                key: key.clone(),
                hash: hash_string.clone(),
                qr_code: f.value.display_as(f.tag).to_string(),
                qr_quality: "OK".to_string(),
                gr_source: "EXIFUserComment".to_string(),
            };

            if tx.send(message).is_err() {
                // do something!
            }
        }
    }

    drop(permit);
}

async fn image_analysis_tasker(
    mut downloadfile_joinset: JoinSet<Option<DownloadFile>>,
    tx: UnboundedSender<AnalyticsResultSet>,
) {
    // This semaphore allows us to control the numnber of concurrent threads runnining in tppol.
    // Here we limit it to the number of CPUs available.
    // At one point we may want to try if "Number of CPUs plus 1" gives us a better performance.
    // Don't forget the in parallel we are still downloading files from aws s3 inside the Tokio runtime.

    let semaphore = Arc::new(Semaphore::new(num_cpus::get()));

    let mut joinset_blocking = Vec::new();

    while let Some(image_join_handle) = downloadfile_joinset.join_next().await {
        if let Ok(Some(image_bytes)) = image_join_handle {
            //let image_analysis_handle = image_analysis_tasker(image_analysis_handle, ressss);
            let tx = tx.clone();
            let semaphore = semaphore.clone();

            let permit = if let Ok(permit) = semaphore.acquire_owned().await {
                permit
            } else {
                return;
            };

            drop(image_bytes.permit);

            joinset_blocking.push(task::spawn_blocking(move || {
                analytics(image_bytes.key, image_bytes.data, tx, permit);
            }));
        }

        // ...
    }
}

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
    let handle_data = download_files_tasker(client.clone(), &args.bucket).await;

    println!("Analyzing files.");
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(image_analysis_tasker(handle_data, tx));

    println!("Waiting for results.");
    while let Some(ss) = rx.recv().await {
        println!(
            "{}, {}, {}, {}, {}",
            ss.key, ss.hash, ss.qr_code, ss.qr_quality, ss.gr_source
        );
    }

    println!("Done with it!");
    Ok(())
}
