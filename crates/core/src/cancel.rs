//! Cooperative cancellation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cheap, cloneable flag checked by long-running operations.
///
/// Cancelling stops processing at the next checkpoint (between frames for
/// videos) and makes the operation return [`Error::Cancelled`](crate::Error::Cancelled).
/// Partially written outputs are removed.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Returns `Err(Cancelled)` if cancellation was requested.
    pub fn check(&self) -> crate::Result<()> {
        if self.is_cancelled() {
            Err(crate::Error::Cancelled)
        } else {
            Ok(())
        }
    }
}
