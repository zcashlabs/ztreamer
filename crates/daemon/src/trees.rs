//! The tree builder: from boot to shutdown it extends the note commitment trees of the index to
//! its durable tip, so that `GetTreeState` reads the index and not the node. The hashes run at the
//! lowest priority on the cores that the block pipeline leaves idle.

use std::{sync::Arc, thread, time::Duration};

use eyre::{Result, WrapErr};
use tokio_util::sync::CancellationToken;
use tracing::{debug, field::Empty, info, warn};
use ztreamer_index::{
    index::{Index, IndexError, TreeError},
    trees::TreeExtension,
};

/// The wait before the next check when the trees reach the tip, or after a failed extension.
const IDLE: Duration = Duration::from_secs(1);

/// Extends the note commitment trees of the index after its blocks until shutdown.
///
/// ```ignore
/// tasks.spawn("tree builder", TASK_STOPPED, TreeTask { index }.run(shutdown.clone()));
/// ```
pub(crate) struct TreeTask {
    pub(crate) index: Arc<Index>,
}

impl TreeTask {
    /// Extends the trees until `shutdown`. Each extension runs on a blocking thread and is a
    /// `trees.extend` span. `run` waits for an extension that runs at shutdown, so that no thread
    /// holds the index when it returns.
    pub(crate) async fn run(self, shutdown: CancellationToken) -> Result<()> {
        let threads = thread::available_parallelism().map_or(1, usize::from);
        let mut at_tip = false;
        while !shutdown.is_cancelled() {
            let index = Arc::clone(&self.index);
            let extended = tokio::task::spawn_blocking(move || extend(&index, threads))
                .await
                .wrap_err("note commitment tree task panicked")?;
            match extended {
                Ok(Some(_)) => continue,
                Ok(None) if !at_tip => {
                    info!(
                        "note commitment trees reach the index tip; GetTreeState reads the index"
                    );
                    at_tip = true;
                }
                Ok(None) => {}
                Err(IndexError::Tree(TreeError::Moved)) => {
                    debug!("a reorg moved the blocks under a tree extension; starting it again");
                }
                Err(error) => {
                    warn!(%error, "note commitment tree extension failed; the next check retries")
                }
            }
            tokio::select! {
                () = shutdown.cancelled() => {}
                () = tokio::time::sleep(IDLE) => {}
            }
        }
        Ok(())
    }
}

/// Extends the trees once if they are behind the durable tip, in a `trees.extend` root span.
/// Returns `None` if they reach the tip.
fn extend(index: &Index, threads: usize) -> Result<Option<TreeExtension>, IndexError> {
    let behind = {
        let snapshot = index.latest()?;
        snapshot.trees_through()? < snapshot.state().durable_tip().map(|tip| tip.height)
    };
    if !behind {
        return Ok(None);
    }
    let span = tracing::info_span!(
        parent: None,
        "trees.extend",
        start_height = Empty,
        end_height = Empty,
        commitments = Empty,
        otel.status_code = Empty,
        otel.status_description = Empty,
    );
    span.in_scope(|| {
        let result = index.extend_trees(threads);
        match &result {
            Ok(Some(extension)) => {
                span.record("start_height", extension.start_height);
                span.record("end_height", extension.end_height);
                span.record("commitments", extension.commitments);
            }
            Ok(None) => {}
            Err(error) => {
                span.record("otel.status_code", "ERROR");
                span.record("otel.status_description", tracing::field::display(error));
            }
        }
        result
    })
}
