//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//

use sha2::{Digest, Sha256};

use std::collections::HashSet;
use std::io::Cursor;
use std::path::PathBuf;
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

// ---------------------------------------------------------------------------
// Image preparation — proportional downscale only, Lanczos3
// ---------------------------------------------------------------------------

fn prepare_luma(image: &image::DynamicImage) -> image::GrayImage {
    let luma = image.to_luma8();
    let (w, h) = (luma.width(), luma.height());
    const MAX_DIM: u32 = 4000;
    if w > MAX_DIM || h > MAX_DIM {
        let scale = MAX_DIM as f32 / w.max(h) as f32;
        image::imageops::resize(
            &luma,
            (w as f32 * scale) as u32,
            (h as f32 * scale) as u32,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        luma
    }
}

// ---------------------------------------------------------------------------
// rxing backend — pure Rust, always active
// ---------------------------------------------------------------------------

fn scan_qr_rxing(
    image_bytes: &bytes::Bytes,
    key: &str,
    hash_string: &str,
    tx: &UnboundedSender<AnalyticsResultSet>,
    emitted: &mut HashSet<String>,
) -> anyhow::Result<()> {
    let image = image::load_from_memory(image_bytes)
        .map_err(|e| anyhow::anyhow!("image decode: {}", e))?;

    let luma = prepare_luma(&image);
    let (width, height) = (luma.width(), luma.height());

    let mut hints = rxing::DecodeHints::default();
    hints.TryHarder = Some(true);

    let results = match rxing::helpers::detect_multiple_in_luma_with_hints(
        luma.into_raw(),
        width,
        height,
        &mut hints,
    ) {
        Ok(r) => r,
        // NotFoundException means no barcodes found — not an error condition.
        Err(rxing::Exceptions::NotFoundException(_)) => return Ok(()),
        Err(e) => return Err(anyhow::anyhow!("rxing: {}", e)),
    };

    for result in results {
        let text = result.getText().to_string();
        if emitted.insert(text.clone()) {
            if tx
                .send(AnalyticsResultSet {
                    key: key.to_string(),
                    hash: hash_string.to_string(),
                    qr_code: text,
                    qr_quality: "OK".to_string(),
                    qr_source: "rxing".to_string(),
                })
                .is_err()
            {
                error!(key, "result channel closed");
                break;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// wechat_qrcode backend — OpenCV, compiled in with --features wechat
// Runs after rxing; only emits codes not already found by rxing.
// ---------------------------------------------------------------------------

#[cfg(feature = "wechat")]
fn build_wechat_detector(
    model_path: &Option<PathBuf>,
) -> anyhow::Result<opencv::wechat_qrcode::WeChatQRCode> {
    use opencv::wechat_qrcode::WeChatQRCode;

    if let Some(p) = model_path {
        let dp = p.join("detect.prototxt");
        let dm = p.join("detect.caffemodel");
        let sp = p.join("sr.prototxt");
        let sm = p.join("sr.caffemodel");

        if dp.exists() && dm.exists() && sp.exists() && sm.exists() {
            return Ok(WeChatQRCode::new(
                &dp.to_string_lossy(),
                &dm.to_string_lossy(),
                &sp.to_string_lossy(),
                &sm.to_string_lossy(),
            )?);
        }
        warn!(path = ?p, "model files incomplete, falling back to lightweight detector");
    }

    Ok(WeChatQRCode::new("", "", "", "")?)
}

#[cfg(feature = "wechat")]
fn scan_qr_wechat(
    image_bytes: &bytes::Bytes,
    key: &str,
    hash_string: &str,
    model_path: &Option<PathBuf>,
    tx: &UnboundedSender<AnalyticsResultSet>,
    emitted: &mut HashSet<String>,
) -> anyhow::Result<()> {
    use opencv::prelude::*;

    let buf = opencv::core::Vector::<u8>::from_iter(image_bytes.iter().copied());
    let mat = opencv::imgcodecs::imdecode(&buf, opencv::imgcodecs::IMREAD_COLOR)?;

    if mat.empty() {
        anyhow::bail!("OpenCV could not decode image");
    }

    let mut detector = build_wechat_detector(model_path)?;
    let mut points = opencv::core::Vector::<opencv::core::Mat>::new();
    let results = detector.detect_and_decode(&mat, &mut points)?;

    for qr_code in results {
        // Only emit codes not already found by rxing.
        if emitted.insert(qr_code.clone()) {
            if tx
                .send(AnalyticsResultSet {
                    key: key.to_string(),
                    hash: hash_string.to_string(),
                    qr_code,
                    qr_quality: "OK".to_string(),
                    qr_source: "wechat_qrcode".to_string(),
                })
                .is_err()
            {
                error!(key, "result channel closed");
                break;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Core analytics — runs rxing then (optionally) wechat, union with dedup
// ---------------------------------------------------------------------------

impl AnalyticsResult {
    fn analytics(
        key: String,
        image_bytes: bytes::Bytes,
        tx: UnboundedSender<AnalyticsResultSet>,
        _permit: tokio::sync::OwnedSemaphorePermit,
        _model_path: Arc<Option<PathBuf>>,
    ) {
        let hash_string = hex::encode(Sha256::digest(&image_bytes));

        // Tracks all QR codes emitted for this image — shared across backends
        // so each unique code is sent exactly once.
        let mut emitted: HashSet<String> = HashSet::new();

        if let Err(e) = scan_qr_rxing(&image_bytes, &key, &hash_string, &tx, &mut emitted) {
            warn!(key = key, error = %e, "rxing scan failed");
        }

        #[cfg(feature = "wechat")]
        if let Err(e) =
            scan_qr_wechat(&image_bytes, &key, &hash_string, &_model_path, &tx, &mut emitted)
        {
            warn!(key = key, error = %e, "wechat_qrcode scan failed");
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

    pub async fn new(
        mut downloadfile_joinset: JoinSet<Result<DownloadFile, RustslingerError>>,
        model_path: Option<PathBuf>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let semaphore = Arc::new(Semaphore::new(num_cpus::get()));
        let model_path = Arc::new(model_path);

        tokio::spawn(async move {
            while let Some(join_result) = downloadfile_joinset.join_next().await {
                match join_result {
                    Ok(Ok(file)) => {
                        let tx = tx.clone();
                        let semaphore = semaphore.clone();
                        let model_path = model_path.clone();

                        let permit = match semaphore.acquire_owned().await {
                            Ok(p) => p,
                            Err(_) => return,
                        };

                        drop(file.permit);

                        task::spawn_blocking(move || {
                            Self::analytics(file.key, file.data, tx, permit, model_path);
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;
    use std::sync::Arc;
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

    fn qr_png(content: &str) -> Vec<u8> {
        use qrcode::QrCode;
        let code = QrCode::new(content.as_bytes()).unwrap();
        let img = code.render::<image::Luma<u8>>().build();
        let dynamic = image::DynamicImage::ImageLuma8(img);
        let mut buf = Cursor::new(Vec::new());
        dynamic.write_to(&mut buf, image::ImageOutputFormat::Png).unwrap();
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
        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }

    // --- pipeline: error handling ---

    #[tokio::test]
    async fn download_error_produces_no_results() {
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async {
            Err(RustslingerError::Download {
                key: "missing.jpg".to_string(),
                reason: "not found".to_string(),
            })
        });
        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn invalid_image_bytes_produce_no_results() {
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async { Ok(make_file("corrupt.jpg", vec![0u8; 256])) });
        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }

    // --- pipeline: valid image, no QR ---

    #[tokio::test]
    async fn valid_image_no_qr_no_exif_produces_no_results() {
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async { Ok(make_file("blank.png", solid_png())) });
        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }

    // --- rxing: detects a single QR code ---

    #[tokio::test]
    async fn rxing_detects_single_qr_code() {
        let content = "https://github.com/tke1973/rustslinger";
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async move { Ok(make_file("qr.png", qr_png(content))) });

        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            drain(&mut result),
        )
        .await
        .expect("timed out");

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].qr_code, content);
        assert_eq!(out[0].qr_source, "rxing");
    }

    // --- rxing: each unique QR code emitted exactly once ---

    #[tokio::test]
    async fn rxing_each_qr_code_emitted_once() {
        // Feed the same image twice — two files, same QR content.
        // Each file is independent so each should emit one result.
        let content = "dedup-test";
        let data = qr_png(content);
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        let d1 = data.clone();
        let d2 = data.clone();
        joinset.spawn(async move { Ok(make_file("a.png", d1)) });
        joinset.spawn(async move { Ok(make_file("b.png", d2)) });

        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            drain(&mut result),
        )
        .await
        .expect("timed out");

        // Two files → two results (dedup is per-image, not global)
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.qr_code == content));
    }

    // --- result fields ---

    #[tokio::test]
    async fn result_key_and_hash_are_correct() {
        let data = solid_png();
        let expected_hash = hex::encode(Sha256::digest(&data));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(AnalyticsResultSet {
            key: "test/photo.jpg".to_string(),
            hash: expected_hash.clone(),
            qr_code: "https://example.com".to_string(),
            qr_quality: "OK".to_string(),
            qr_source: "rxing".to_string(),
        })
        .unwrap();
        drop(tx);

        let mut mock_result = AnalyticsResult { rx };
        let out = drain(&mut mock_result).await;

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "test/photo.jpg");
        assert_eq!(out[0].hash, expected_hash);
        assert_eq!(out[0].qr_source, "rxing");
    }

    // --- wechat feature: dual-backend union ---

    #[cfg(feature = "wechat")]
    #[tokio::test]
    async fn dual_backend_detects_qr_code() {
        // Both rxing and wechat run. The QR code should appear exactly once
        // regardless of which backend finds it first.
        let content = "https://github.com/tke1973/rustslinger";
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async move { Ok(make_file("qr.png", qr_png(content))) });

        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            drain(&mut result),
        )
        .await
        .expect("timed out");

        // Exactly one result — dedup prevents double-emission.
        assert_eq!(out.len(), 1, "same QR code must not be emitted twice");
        assert_eq!(out[0].qr_code, content);
        // Source is whichever backend found it first (rxing runs first).
        assert!(
            out[0].qr_source == "rxing" || out[0].qr_source == "wechat_qrcode",
            "unexpected source: {}",
            out[0].qr_source
        );
    }

    #[cfg(feature = "wechat")]
    #[tokio::test]
    async fn wechat_blank_image_produces_no_results() {
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async { Ok(make_file("blank.png", solid_png())) });

        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }

    #[cfg(feature = "wechat")]
    #[tokio::test]
    async fn wechat_handles_invalid_image_gracefully() {
        let mut joinset: JoinSet<Result<DownloadFile, RustslingerError>> = JoinSet::new();
        joinset.spawn(async { Ok(make_file("corrupt.jpg", vec![0u8; 256])) });

        let mut result = AnalyticsResult::new(joinset, None).await;
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            drain(&mut result),
        )
        .await
        .expect("timed out");
        assert!(out.is_empty());
    }
}
