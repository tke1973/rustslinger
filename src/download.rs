//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_stream::StreamExt;

use aws_sdk_s3::Client;

use crate::RustslingerError;

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
) -> Result<DownloadFile, RustslingerError> {
    let permit = semaphore
        .acquire_owned()
        .await
        .map_err(|_| RustslingerError::SemaphoreClosed)?;

    let body = client
        .get_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .map_err(|e| RustslingerError::Download { key: key.clone(), reason: e.to_string() })?
        .body
        .collect()
        .await
        .map_err(|e| RustslingerError::Download { key: key.clone(), reason: e.to_string() })?;

    Ok(DownloadFile { key, data: body.into_bytes(), permit })
}

pub async fn download_files_tasker(client: Client, bucket: &str) -> JoinSet<Result<DownloadFile, RustslingerError>> {
    let mut downloadfile_joinset = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(num_cpus::get() * 10));

    let mut list_s3objects_page = client
        .list_objects_v2()
        .bucket(bucket)
        .into_paginator()
        .send();

    while let Some(list_s3objects) = list_s3objects_page.next().await {
        if let Ok(s3objects) = list_s3objects {
            if let Some(s3objects_contents) = s3objects.contents() {
                for object in s3objects_contents {
                    let client = client.clone();
                    let key = object.key().unwrap().to_string();
                    let bucket = bucket.to_string();
                    let semaphore = semaphore.clone();
                    downloadfile_joinset.spawn(download_s3file(client, key, bucket, semaphore));
                }
            }
        }
    }

    downloadfile_joinset
}
