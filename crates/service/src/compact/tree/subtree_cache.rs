//! [`SubtreeRootCache`]: the built `GetSubtreeRoots` answer of each pool, for one serving view.
//!
//! # When an answer is stale
//!
//! A [`CachedRoots`] holds the answer of a pool for one [`ServingView`]: the generation of the
//! index and the visible tip of a [`ServingSnapshot`]. The view fixes every block that the service
//! serves:
//!
//! - Each commit to the index increases the generation. That includes the replacement of a reorg
//!   in the mutable rows and in the sealed ranges. Thus the generation fixes the durable blocks.
//! - The volatile head links the durable tip to the visible tip, hash by hash. Thus the visible
//!   tip fixes the volatile head: a reorg of the volatile head changes the visible tip.
//!
//! A request whose snapshot has another view rebuilds the answer: it asks the node for all the
//! subtrees of the pool, and reads each completing block in the new view. A root whose completing
//! block is in the volatile head is thus never served after a reorg replaced the block, and a
//! subtree that completes is served from the first view that has its completing block. A view
//! lasts one block, so the rebuild runs about once each block and pool.
//!
//! In one view, the service does not ask the node again. A subtree that the node completes in a
//! block above the visible tip shows when the head follower publishes the block, as the other
//! requests see the block. A root that the node had above the visible tip when the answer was
//! built fails with `OUT_OF_RANGE` in each request that reaches it, as it did with no cache.
//!
//! While deep reorg recovery runs, the snapshot is not ready and the requests do not use the
//! cache.

use std::sync::Arc;

use tokio::sync::Mutex;
use tonic::Status;
use zakura_chain::subtree::NoteCommitmentSubtreeIndex;
use ztreamer_index::index::{BlockId, IndexError};
use ztreamer_protocol::{RpcStream, proto};
use ztreamer_snapshot::pool::Pool;
use ztreamer_sync::head::ServingSnapshot;

/// The [`CachedRoots`] of each pool. A rebuild holds the lock of its pool, so the requests that
/// arrive meanwhile wait for its answer and do not ask the node again.
#[derive(Debug, Default)]
pub(crate) struct SubtreeRootCache {
    sapling: Mutex<Option<CachedRoots>>,
    orchard: Mutex<Option<CachedRoots>>,
    ironwood: Mutex<Option<CachedRoots>>,
}

impl SubtreeRootCache {
    /// The cached answer of `pool`, behind its lock.
    pub(crate) const fn pool(&self, pool: Pool) -> &Mutex<Option<CachedRoots>> {
        match pool {
            Pool::Sapling => &self.sapling,
            Pool::Orchard => &self.orchard,
            Pool::Ironwood => &self.ironwood,
        }
    }
}

/// The `GetSubtreeRoots` answer of one pool from subtree index 0, built in one [`ServingView`].
/// Clones share the roots.
#[derive(Clone, Debug)]
pub(crate) struct CachedRoots {
    view: ServingView,
    /// The roots whose completing block is in the view, in index order from index 0.
    roots: Arc<[proto::SubtreeRoot]>,
    /// The completing heights of the subtrees after `roots`, which are above the visible tip.
    above_tip: Arc<[u32]>,
}

impl CachedRoots {
    /// The answer in the view of `snapshot`: `roots` from index 0, then the completing heights of
    /// the subtrees above the visible tip.
    pub(crate) fn new(
        snapshot: &ServingSnapshot,
        roots: Vec<proto::SubtreeRoot>,
        above_tip: Vec<u32>,
    ) -> Self {
        Self { view: ServingView::of(snapshot), roots: roots.into(), above_tip: above_tip.into() }
    }

    /// Returns `true` if the answer holds for `snapshot`: the snapshot is ready, and it has the
    /// view of the answer.
    pub(crate) fn serves(&self, snapshot: &ServingSnapshot) -> bool {
        snapshot.ready && self.view == ServingView::of(snapshot)
    }

    /// The answer to a request from `start`, of at most `limit` roots, or of all roots after
    /// `start` when `limit` is `None`. A subtree above the visible tip ends the answer with
    /// `OUT_OF_RANGE`, as the index read of its completing block does.
    ///
    /// The stream is ready at once, so tonic writes it in a few large frames.
    pub(crate) fn slice(
        &self,
        start: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> RpcStream<proto::SubtreeRoot> {
        let start = usize::from(start.0);
        let end = limit.map_or(usize::MAX, |limit| start + usize::from(limit.0));
        let roots = Arc::clone(&self.roots);
        let served = roots.len().min(start)..roots.len().min(end);
        let above_tip = (start.max(roots.len())..end)
            .next()
            .and_then(|position| self.above_tip.get(position - roots.len()).copied())
            .map(|height| Err(Status::from(IndexError::Coverage { height })));
        let roots = served.map(move |position| Ok(roots[position].clone()));
        Box::pin(tokio_stream::iter(roots.chain(above_tip)))
    }
}

/// The blocks that a [`ServingSnapshot`] serves: the generation of the index, which fixes the
/// durable blocks, and the visible tip, which fixes the volatile head above them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ServingView {
    generation: u64,
    visible_tip: Option<BlockId>,
}

impl ServingView {
    const fn of(snapshot: &ServingSnapshot) -> Self {
        Self { generation: snapshot.generation, visible_tip: snapshot.visible_tip }
    }
}
