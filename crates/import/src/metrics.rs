//! The metrics of the import.
//!
//! Each metric name and its help text is a constant in this file, and [`DESCRIBE`] gives the help
//! text to the recorder once. Other modules record only through the types of this file.

use std::sync::LazyLock;

use metrics::{Counter, counter, describe_counter, describe_gauge, gauge};

const EPOCHS: &str = "ztreamer.import.epochs";
const EPOCHS_HELP: &str = "Epochs that the import installed.";

const PLANNED_EPOCHS: &str = "ztreamer.import.planned.epochs";
const PLANNED_EPOCHS_HELP: &str = "Epochs that the running import installs in all.";

const BYTES: &str = "ztreamer.import.bytes";
const BYTES_HELP: &str = "Blob bytes of the epochs that the import installed.";

/// Gives the help text of each metric to the recorder. Each constructor of handles forces it.
static DESCRIBE: LazyLock<()> = LazyLock::new(|| {
    describe_counter!(EPOCHS, EPOCHS_HELP);
    describe_gauge!(PLANNED_EPOCHS, PLANNED_EPOCHS_HELP);
    describe_counter!(BYTES, BYTES_HELP);
});

/// The handles of one import.
pub(crate) struct ImportMetrics {
    epochs: Counter,
    bytes: Counter,
}

impl ImportMetrics {
    /// Takes the handles, and sets the planned epochs to `planned_epochs`.
    pub(crate) fn new(planned_epochs: usize) -> Self {
        LazyLock::force(&DESCRIBE);
        gauge!(PLANNED_EPOCHS).set(planned_epochs as f64);
        Self { epochs: counter!(EPOCHS), bytes: counter!(BYTES) }
    }

    /// Counts one installed epoch with `bytes` of blobs.
    pub(crate) fn installed_epoch(&self, bytes: u64) {
        self.epochs.increment(1);
        self.bytes.increment(bytes);
    }
}
