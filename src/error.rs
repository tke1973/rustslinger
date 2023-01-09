use thiserror::Error;

#[derive(Error, Debug)]
pub enum RustslingerError {
    #[error("Error: could not list buckets")]
    AWSListBucketsOutputError,

    #[error("Error: could not list s3 buckets")]
    AWSListS3BucketsError,

    #[error("Error: could not list s3 objects")]
    AWSListS3ObjectsError,

    #[error("Error: could not list s3 objects contents")]
    AWSListS3ObjectsContentsError,

    #[error("Error: could not aquire semaphore permit")]
    SemaphorePermitError,

    #[error("Error: could not create thread pool")]
    ThreadPoolCreationError,

    #[error("Error: unknown error - impelment me!")]
    Unknown,
}
