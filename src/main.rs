/*
 * Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0.
 */

use clap::Parser;
use std::env;

use sha2::{Digest, Sha256};

use image::io::Reader as ImageReader;
use std::io::Cursor;

use std::sync::Arc;
use tokio::sync::Semaphore;

use futures::channel::mpsc;
use futures::executor::ThreadPool;
use futures::stream::StreamExt;

use aws_config::meta::region::RegionProviderChain;
use aws_config::profile::credentials::ProfileFileCredentialsProvider;
use aws_sdk_s3::Client;

/// rustslinger
/// Fully functional learning and experimenting application. Entirely written in Rust.
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

// List all available s3 buckets
async fn list_s3buckets(client: &Client) -> Result<(), ()> {
    let list_buckets = if let Ok(list_buckets) = client.list_buckets().send().await {
        list_buckets
    } else {
        // This is an error we can't recover from.
        // Therefore, we throw an  error message.
        println!("Error: Could not list buckets. Exiting.");
        return Err(());
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
async fn main() {
    // get command line options
    let args = Args::parse();

    let region_provider = RegionProviderChain::default_provider().or_else("ap-southeast-1");
    let config = aws_config::from_env().region(region_provider);

    let profile = if let Some(profile_string) = args.profile {
        Some(profile_string)
    } else if let Some(osprofile_string) = env::var_os("AWS_DEFAULT_PROFILE") {
        Some(osprofile_string.to_string_lossy().to_string())
    } else {
        None
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
    if args.bucketlist {
        if let Err(_) = list_s3buckets(&client).await {
            println!("Error: Cant list buckets. Exiting.");
            return;
        }
    };

    let resp = if let Ok(resp) = client
        .list_objects_v2()
        .bucket(args.bucket.clone())
        .send()
        .await
    {
        resp
    } else {
        return;
    };

    let resp3 = resp.contents().unwrap_or_default();

    let mut fetches = futures::stream::iter(resp3.iter().map(|path| async {
        let object_resp = if let Ok(object_resp) = client
            .get_object()
            .bucket(args.bucket.clone())
            .key(path.key().unwrap_or_default())
            .send()
            .await
        {
            object_resp
        } else {
            return None;
        };

        if let Ok(object_resp_body) = object_resp.body.collect().await {
            Some(object_resp_body.into_bytes())
        } else {
            None
        }
    }))
    .buffer_unordered(30);

    println!("Waiting...");

    let mut i = 0;

    let tpool = if let Ok(tpool) = ThreadPool::new() {
        tpool
    } else {
        return;
    };

    let (tx, mut rx) = mpsc::unbounded::<String>();

    // This semaphore allows us to control the numnber of concurrent threads runnining in tppol.
    // Here we limit it to the number of CPUs available.
    // At one point we may want to try if "Number of CPUs plus 1" gives us a better result.
    // Don't forget the in parallel we are still downloading files from aws s3 inside the Tokio runtime.
    let semaphore = Arc::new(Semaphore::new(num_cpus::get()));

    while let Some(object_resp_body_bytes) = fetches.next().await {
        if let Some(image) = object_resp_body_bytes {
            println!("this is {}", i);
            i += 1;

            let tx = tx.clone();
            let permit = semaphore.clone().acquire_owned().await.unwrap();

            let img2 = Cursor::new(image.clone());

            tpool.spawn_ok(async move {
                let mut thehash = Sha256::new();
                thehash.update(&image);
                let res = thehash.finalize();

                let image_reader = if let Ok(image_reader) =
                    ImageReader::new(Cursor::new(&image)).with_guessed_format()
                {
                    image_reader
                } else {
                    // This error is unrecoverable and the thread will therefore not produce a result.
                    // We end the thread to free CPU and memory resoucres for the next thread.
                    return;
                };

                let image_decoded = if let Ok(image_decoded) = image_reader.decode() {
                    image_decoded.to_luma8()
                } else {
                    // This error is unrecoverable and the thread  will therefore not produce a result.
                    // We end the thread to free CPU and memory resoucres for the next thread.
                    return;
                };

                let mut image_prepared = rqrr::PreparedImage::prepare(image_decoded);
                let grids = image_prepared.detect_grids();

                let mut gc = 0;
                for g in grids {
                    gc += 1;

                    let qrcode_result = g.decode();

                    let qrcode = match qrcode_result {
                        Ok((_meta, content)) => content,
                        Err(_error) => "did got an error while decoding QR code".to_string(),
                    };
                    println!("Found one! Its number {:?}. Tag is: {:?}", gc, qrcode);
                    tx.unbounded_send(qrcode).expect("Failed to send");
                }

                println!("hash is: {:?}", res);

                let mut a = std::io::BufReader::new(img2);
                let exifreader = exif::Reader::new();
                let exif = exifreader.read_from_container(&mut a).unwrap();
                for f in exif.fields() {
                    tx.unbounded_send(format!(
                        "{} {} {}",
                        f.tag,
                        f.ifd_num,
                        f.display_value().with_unit(&exif)
                    ))
                    .expect("msg");
                }

                drop(permit);
            });
        };
    }

    drop(tx);

    while let Some(ss) = rx.next().await {
        println!("{:?}", ss);
    }

    println!("Done with it!");
}
