//! The server-wide limit on the block-range streams that run at a time.

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tonic::Status;

/// The default number of block-range streams that run at a time. lightwalletd defaults
/// `--max-concurrent-blockrange` to 200.
pub const DEFAULT_MAX_CONCURRENT_RANGES: NonZeroUsize = NonZeroUsize::new(16).unwrap();

/// How long a block-range request waits for a slot before it fails with `UNAVAILABLE`, as in
/// lightwalletd. The deadline or the cancel of the client ends the wait sooner: tonic drops the
/// future of the request.
pub const RANGE_ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);

/// The slots of the block-range streams, shared by the clones of a service. With no limit, each
/// request runs at once.
#[derive(Clone, Debug)]
pub(crate) struct RangeLimit(Option<Arc<Semaphore>>);

impl RangeLimit {
    /// At most `limit` streams at a time, or no limit for `None`.
    pub(crate) fn new(limit: Option<NonZeroUsize>) -> Self {
        Self(limit.map(|limit| Arc::new(Semaphore::new(limit.get()))))
    }

    /// Waits up to [`RANGE_ADMISSION_TIMEOUT`] for a slot. The stream holds the permit until it
    /// is dropped. Returns no permit when there is no limit.
    pub(crate) async fn admit(&self) -> Result<Option<OwnedSemaphorePermit>, Status> {
        let Some(slots) = &self.0 else {
            return Ok(None);
        };
        tokio::time::timeout(RANGE_ADMISSION_TIMEOUT, Arc::clone(slots).acquire_owned())
            .await
            .map_err(|_| {
                Status::unavailable("too many concurrent block-range requests; retry shortly")
            })?
            .map(Some)
            .map_err(|_| Status::unavailable("range stream pool is closed"))
    }
}

impl Default for RangeLimit {
    fn default() -> Self {
        Self::new(Some(DEFAULT_MAX_CONCURRENT_RANGES))
    }
}
