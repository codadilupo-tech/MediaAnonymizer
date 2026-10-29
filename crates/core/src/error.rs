//! Error type shared by the whole library.

use std::path::PathBuf;

/// Convenient result alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every failure the library can report.
///
/// Variants carry enough context to produce an actionable message for the end user
/// (which file, which tool, what to do next) without needing a backtrace.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Underlying I/O failure.
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The input format is not recognised or the requested operation is not
    /// available for it.
    #[error("unsupported media: {0}")]
    Unsupported(String),

    /// The file looks like a supported format but its structure is broken.
    #[error("malformed {format} data: {reason}")]
    Malformed { format: &'static str, reason: String },

    /// Failure while decoding or encoding pixels.
    #[error("image codec error: {0}")]
    Codec(String),

    /// ONNX Runtime / face detector failure.
    #[error("face detection error: {0}")]
    Detection(String),

    /// The requested compute device or hardware accelerator is not available.
    #[error("hardware acceleration error: {0}")]
    Hardware(String),

    /// `ffmpeg`/`ffprobe` could not be found or failed.
    #[error("ffmpeg error: {0}")]
    Ffmpeg(String),

    /// The options combination is invalid.
    #[error("invalid options: {0}")]
    InvalidOptions(String),

    /// The operation was cancelled through a [`CancelToken`](crate::CancelToken).
    #[error("operation cancelled")]
    Cancelled,
}

impl Error {
    /// Builds an [`Error::Io`] tagged with the path being accessed.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io { path: path.into(), source }
    }

    pub(crate) fn malformed(format: &'static str, reason: impl Into<String>) -> Self {
        Error::Malformed { format, reason: reason.into() }
    }
}

impl From<image::ImageError> for Error {
    fn from(e: image::ImageError) -> Self {
        Error::Codec(e.to_string())
    }
}

impl From<ort::Error> for Error {
    fn from(e: ort::Error) -> Self {
        Error::Detection(e.to_string())
    }
}
