//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//

use std::sync::Arc;

use tokio::sync::Semaphore;

use tokio::task::JoinSet;
use tokio_stream::StreamExt;

use aws_sdk_s3::Client;

//mod crate::error;
//pub use crate::error::RustslingerError;

pub struct DownloadFile {
    pub key: String,
    pub data: bytes::Bytes,
    pub permit: tokio::sync::OwnedSemaphorePermit,
}

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
            key,
            data: object_response_body.into_bytes(),
            permit,
        };
        Some(download_file)
    } else {
        drop(permit);
        None
    }
}

pub async fn download_files_tasker(client: Client, bucket: &str) -> JoinSet<Option<DownloadFile>> {
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
