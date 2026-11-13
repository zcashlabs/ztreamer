//! The progress of [`Importer::run`](super::Importer::run): the totals of the epochs to import,
//! and what the import installed of them.

use std::time::Instant;

use tracing::info;
use ztreamer_snapshot::epoch::Epoch;

use crate::metrics::ImportMetrics;

/// The epochs and the blob bytes that an import installed, of the totals at its start.
pub(crate) struct ImportProgress {
    started: Instant,
    epochs: usize,
    bytes: u64,
    installed_epochs: usize,
    installed_bytes: u64,
    metrics: ImportMetrics,
}

impl ImportProgress {
    /// Logs the totals of `epochs`, the epochs to import in height order. Never empty.
    pub(crate) fn start(epochs: &[Epoch<'_>]) -> Self {
        let entries = || epochs.iter().flat_map(|epoch| epoch.entries());
        let bytes = entries().map(|entry| entry.blob_len).sum();
        let start_height = entries().next().expect("the import has an epoch").start_height;
        let end_height = entries().last().expect("the import has an epoch").end_height;
        info!(
            epochs = epochs.len(),
            ranges = entries().count(),
            bytes,
            start_height,
            end_height,
            "importing epochs"
        );
        Self {
            started: Instant::now(),
            epochs: epochs.len(),
            bytes,
            installed_epochs: 0,
            installed_bytes: 0,
            metrics: ImportMetrics::new(epochs.len()),
        }
    }

    /// Adds the install of `epoch`, and logs the progress. `ranges` is the ranges installed.
    pub(crate) fn installed(&mut self, epoch: &Epoch<'_>, ranges: usize) {
        let bytes = epoch.entries().iter().map(|entry| entry.blob_len).sum();
        self.installed_epochs += 1;
        self.installed_bytes += bytes;
        self.metrics.installed_epoch(bytes);
        let bytes_per_second = self.installed_bytes as f64 / self.started.elapsed().as_secs_f64();
        let eta_seconds = (self.bytes - self.installed_bytes) as f64 / bytes_per_second;
        info!(
            end_height = epoch.end_height(),
            ranges,
            epochs = %format_args!("{}/{}", self.installed_epochs, self.epochs),
            bytes = %format_args!("{}/{}", self.installed_bytes, self.bytes),
            bytes_per_second = bytes_per_second as u64,
            eta_seconds = eta_seconds as u64,
            "imported epoch"
        );
    }
}
