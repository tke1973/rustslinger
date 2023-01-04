/*
 * Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0.
 */

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

#[tokio::main]
async fn main() -> Result<(), aws_sdk_s3::Error> {
    let region_provider = RegionProviderChain::default_provider().or_else("ap-southeast-1");

    let credentials_provider = ProfileFileCredentialsProvider::builder()
        .profile_name("fta")
        .build();

    let config = aws_config::from_env()
        .region(region_provider)
        .credentials_provider(credentials_provider)
        .load()
        .await;

    let client = Client::new(&config);

    let resp = client.list_buckets().send().await?;
    let buckets = resp.buckets().unwrap_or_default();
    let num_buckets = buckets.len();

    for bucket in buckets {
        println!("{}", bucket.name().unwrap_or_default());
    }

    println!();
    println!("Found {} buckets.", num_buckets);

    let resp = client
        .list_objects_v2()
        .bucket("tower1-ftp-detecon-tketest")
        .send()
        .await?;

    let resp3 = resp.contents().unwrap_or_default();

    let mut fetches = futures::stream::iter(resp3.iter().map(|path| async {
        let resp2 = client
            .get_object()
            .bucket("tower1-ftp-detecon-tketest")
            .key(path.key().unwrap_or_default())
            .send()
            .await
            .unwrap();

        resp2.body.collect().await.unwrap().into_bytes()
    }))
    .buffer_unordered(30);

    println!("Waiting...");

    let mut i = 0;
    let tpool = ThreadPool::new().unwrap();
    let (tx, mut rx) = mpsc::unbounded::<String>();
    let semaphore = Arc::new(Semaphore::new(num_cpus::get()));

    while let Some(image) = fetches.next().await {
        println!("this is {}", i);
        i += 1;

        let tx = tx.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();

        tpool.spawn_ok(async move {
            let mut thehash = Sha256::new();
            thehash.update(&image);
            let res = thehash.finalize();

            let img = ImageReader::new(Cursor::new(&image))
                .with_guessed_format()
                .unwrap()
                .decode()
                .unwrap()
                .to_luma8();

            let mut img = rqrr::PreparedImage::prepare(img);
            let grids = img.detect_grids();

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

            //println!("{} hash is: {:?}", object.key().unwrap_or_default(), res);
            println!("hash is: {:?}", res);

            drop(permit);
        });
    }

    drop(tx);

    while let Some(ss) = rx.next().await {
        println!("{:?}", ss);
    }

    println!("Done with it!");
    Ok(())
}
