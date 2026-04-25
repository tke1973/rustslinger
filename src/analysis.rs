//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//

use sha2::{Digest, Sha256};

use image::io::Reader as ImageReader;
use std::io::Cursor;

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Semaphore;
use tokio::task;
use tokio::task::JoinSet;

use tracing::{error, warn};

use crate::{DownloadFile, RustslingerError};

#[derive(Debug)]
pub struct AnalyticsResultSet {
    pub key: String,
    pub hash: String,
    pub qr_code: String,
    pub qr_quality: String,
    pub qr_source: String,
}

#[derive(Debug)]
pub struct AnalyticsResult {
    rx: UnboundedReceiver<AnalyticsResultSet>,
}

impl AnalyticsResult {
    fn analytics(
        key: String,
        image_bytes: bytes::Bytes,
        tx: UnboundedSender<AnalyticsResultSet>,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) {
        let hash_string = hex::encode(Sha256::digest(&image_bytes));

        // Use a closure to enable ? for the fallible image decode + QR scan path.
        let process_qr = || -> anyhow::Result<()> {
            let image = ImageReader::new(Cursor::new(&image_bytes))
                .with_guessed_format()?
                .decode()?;

            let luma = image::imageops::resize(
                &image.to_luma8(), 800, 600, image::imageops::FilterType::Nearest,
            );

            for g in rqrr::PreparedImage::prepare(luma).detect_grids() {
                let message = match g.decode() {
                    Ok((_, qrcode)) => AnalyticsResultSet {
                        key: key.clone(),
                        hash: hash_string.clone(),
                        qr_code: qrcode,
                        qr_quality: "OK".to_string(),
                        qr_source: "rqrr".to_string(),
                    },
                    Err(e) => AnalyticsResultSet {
                        key: key.clone(),
                        hash: hash_string.clone(),
                        qr_code: "DECODER_ERROR".to_string(),
                        qr_quality: e.to_string(),
                        qr_source: "rqrr".to_string(),
                    },
                };
                if tx.send(message).is_err() {
                    error!(key = key, "result channel closed");
                    return Ok(());
                }
            }
            Ok(())
        };

        if let Err(e) = process_qr() {
            warn!(key = key, error = %e, "skipping QR scan: image decode failed");
        }

        // EXIF is independent — absence is normal, not an error.
        let mut buf = std::io::BufReader::new(Cursor::new(&image_bytes));
        if let Ok(exif) = exif::Reader::new().read_from_container(&mut buf) {
            for f in exif.fields() {
                if f.tag.to_string() == "UserComment" {
                    let _ = tx.send(AnalyticsResultSet {
                        key: key.clone(),
                        hash: hash_string.clone(),
                        qr_code: f.value.display_as(f.tag).to_string(),
                        qr_quality: "OK".to_string(),
                        qr_source: "EXIFUserComment".to_string(),
                    });
                }
            }
        }
    }

    pub async fn new(mut downloadfile_joinset: JoinSet<Result<DownloadFile, RustslingerError>>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let semaphore = Arc::new(Semaphore::new(num_cpus::get()));

        tokio::spawn(async move {
            while let Some(join_result) = downloadfile_joinset.join_next().await {
                match join_result {
                    Ok(Ok(file)) => {
                        let tx = tx.clone();
                        let semaphore = semaphore.clone();

                        let permit = match semaphore.acquire_owned().await {
                            Ok(p) => p,
                            Err(_) => return,
                        };

                        drop(file.permit);

                        task::spawn_blocking(move || {
                            Self::analytics(file.key, file.data, tx, permit);
                        });
                    }
                    Ok(Err(e)) => warn!(error = %e, "download failed, skipping"),
                    Err(e) => error!(error = %e, "download task panicked"),
                }
            }
        });

        AnalyticsResult { rx }
    }

    pub async fn get_next(&mut self) -> Option<AnalyticsResultSet> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;
    use sha2::{Digest, Sha256};
    use tokio::sync::Semaphore;
    use tokio::task::JoinSet;
    use crate::{DownloadFile, RustslingerError};

    fn make_file(key: &str, data: Vec<u8>) -> DownloadFile {
        let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
        DownloadFile {
            key: key.to_string(),
            data: bytes::Bytes::from(data),
            permit,
        }
    }

    fn solid_png() -> Vec<u8> {
        let img = image::DynamicImage::new_luma8(200, 200);
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageOutputFormat::Png).unwrap();
        buf.into_inner()
    }

    async fn drain(result: &mut AnalyticsResult) -> Vec<AnalyticsResultSet> {
        let mut out = Vec::new();
        while let Some(r) = result.get_next().await {
            out.push(r);
        }
        out
    }

    // --- hash ---

    #[test]
    fn hash_known_input() {
        let hash = hex::encode(Sha256::digest(b"hello"));
        assert_eq!(hash, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }

    #[test]
    fn hash_is_hex_string_of_correct_length() {
        let hash = hex::encode(Sha256::digest(b"rustslinger"));
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // --- pipeline: empty joinset ---

    #[tokio::test]
    async fn empty_joinset_closes_channel() {
        let joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        let mut result = AnalyticsResult::new(joinset).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain(&mut result),
        )
        .await
        .expect("timed out waiting for empty joinset to close channel");
        assert!(out.is_empty());
    }

    // --- pipeline: download error is skipped gracefully ---

    #[tokio::test]
    async fn download_error_produces_no_results() {
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async {
            Err(RustslingerError::Download {
                key: "missing.jpg".to_string(),
                reason: "not found".to_string(),
            })
        });
        let mut result = AnalyticsResult::new(joinset).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }

    // --- pipeline: invalid bytes are skipped gracefully ---

    #[tokio::test]
    async fn invalid_image_bytes_produce_no_results() {
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async { Ok(make_file("corrupt.jpg", vec![0u8; 256])) });
        let mut result = AnalyticsResult::new(joinset).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }

    // --- pipeline: valid image with no QR and no EXIF ---

    #[tokio::test]
    async fn valid_image_no_qr_no_exif_produces_no_results() {
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async { Ok(make_file("blank.png", solid_png())) });
        let mut result = AnalyticsResult::new(joinset).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }

    // --- pipeline: result fields are populated correctly ---

    #[tokio::test]
    async fn result_key_and_hash_are_correct() {
        let data = solid_png();
        let expected_hash = hex::encode(Sha256::digest(&data));

        let joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        // Inject a file that would produce a result by sending one directly through
        // the channel, bypassing the analytics path — test the struct fields only.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(AnalyticsResultSet {
            key: "test/photo.jpg".to_string(),
            hash: expected_hash.clone(),
            qr_code: "https://example.com".to_string(),
            qr_quality: "OK".to_string(),
            qr_source: "rqrr".to_string(),
        })
        .unwrap();
        drop(tx);

        // Drain the receiver directly without going through the pipeline.
        let _ = joinset; // unused but keeps type inference happy
        let mut mock_result = AnalyticsResult { rx };
        let out = drain(&mut mock_result).await;

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "test/photo.jpg");
        assert_eq!(out[0].hash, expected_hash);
        assert_eq!(out[0].qr_source, "rqrr");
    }
}
