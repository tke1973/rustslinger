//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//

use std::fmt::Write;

use sha2::{Digest, Sha256};

use image::io::Reader as ImageReader;
use std::io::Cursor;

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Semaphore;
use tokio::task;
use tokio::task::JoinSet;

use crate::DownloadFile;

#[derive(Debug)]
pub struct AnalyticsResultSet {
    pub key: String,
    pub hash: String,
    pub qr_code: String,
    pub qr_quality: String,
    pub qr_source: String,
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
                qr_source: "rqrr".to_string(),
            },
            Err(error) => AnalyticsResultSet {
                key: key.clone(),
                hash: hash_string.clone(),
                qr_code: "DECODER_ERROR".to_string(),
                qr_quality: error.to_string(),
                qr_source: "rqrr".to_string(),
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
                qr_source: "EXIFUserComment".to_string(),
            };

            if tx.send(message).is_err() {
                // do something!
            }
        }
    }

    drop(permit);
}

pub async fn image_analysis_tasker(
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
