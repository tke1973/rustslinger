//
// Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
// SPDX-License-Identifier: MIT OR Apache-2.0
//

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RustslingerError {
    #[error("could not list S3 buckets")]
    ListBuckets,

    #[error("download failed for '{key}': {reason}")]
    Download { key: String, reason: String },

    #[error("semaphore closed unexpectedly")]
    SemaphoreClosed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_buckets_message() {
        assert_eq!(RustslingerError::ListBuckets.to_string(), "could not list S3 buckets");
    }

    #[test]
    fn download_message_includes_key_and_reason() {
        let e = RustslingerError::Download {
            key: "images/test.jpg".to_string(),
            reason: "timeout".to_string(),
        };
        assert_eq!(e.to_string(), "download failed for 'images/test.jpg': timeout");
    }

    #[test]
    fn semaphore_closed_message() {
        assert_eq!(RustslingerError::SemaphoreClosed.to_string(), "semaphore closed unexpectedly");
    }
}
