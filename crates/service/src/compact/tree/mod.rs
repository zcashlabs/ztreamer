//! Commitment tree requests: tree states and note commitment subtree roots.

mod subtree_cache;

use std::sync::Arc;

use tonic::Status;
use tracing::Span;
use zakura_chain::{block, subtree::NoteCommitmentSubtreeIndex};
use ztreamer_backend::Subtrees;
use ztreamer_chain::codec::{CompactBlockRecord, RecordHeader};
use ztreamer_index::index::{Index, IndexError, IndexSnapshot};
use ztreamer_protocol::proto;
use ztreamer_snapshot::{manifest_entry::Frontiers, pool::Pool};

use self::subtree_cache::CachedRoots;
use super::{CompactService, RpcStream, ServingSnapshot};

pub(crate) use self::subtree_cache::SubtreeRootCache;

impl CompactService {
    /// Answers `GetTreeState`: from the note commitment trees of the index when they reach the
    /// block, else from the backend.
    pub async fn tree_state(&self, request: proto::BlockId) -> Result<proto::TreeState, Status> {
        let record = self.record_as::<CompactBlockRecord>(request).await?;
        let frontiers = match self.indexed_frontiers(&record).await? {
            Some(frontiers) => frontiers,
            None => Frontiers::from(&self.backend.tree_state(record.hash).await?),
        };
        Ok(proto::TreeState {
            network: self.chain_name.to_string(),
            height: u64::from(record.height),
            hash: record.hash.to_string(),
            time: record.time,
            sapling_tree: hex::encode(frontiers.sapling),
            orchard_tree: hex::encode(frontiers.orchard),
            ironwood_tree: hex::encode(frontiers.ironwood),
        })
    }

    /// The frontiers at the end of `record` from the trees of the index, at the generation of
    /// the serving snapshot. `None` if the trees do not reach the block yet, or if the index moved
    /// to another generation: the backend answers by hash then.
    async fn indexed_frontiers(
        &self,
        record: &CompactBlockRecord,
    ) -> Result<Option<Frontiers>, Status> {
        let index = Arc::clone(&self.index);
        let generation = self.snapshot().generation;
        let (height, sizes) = (record.height, record.end_tree_sizes);
        let span = Span::current();
        tokio::task::spawn_blocking(move || {
            span.in_scope(|| match index.snapshot(generation) {
                Ok(snapshot) => snapshot.tree_frontiers(height, sizes),
                Err(IndexError::Generation { .. }) => Ok(None),
                Err(error) => Err(error),
            })
        })
        .await
        .map_err(|error| Status::unavailable(format!("LMDB reader failed: {error}")))?
        .map_err(Status::from)
    }

    /// Answers `GetLatestTreeState`.
    pub async fn latest_tree_state(&self) -> Result<proto::TreeState, Status> {
        self.tree_state(self.latest_block()?).await
    }

    /// Answers `GetSubtreeRoots`.
    ///
    /// The answer is a slice of the cached answer of the pool from index 0. A request rebuilds the
    /// cached answer when the serving view changed: the generation of the index or the visible
    /// tip. The docs of the `subtree_cache` module give the rule.
    pub async fn subtree_roots(
        &self,
        request: proto::GetSubtreeRootsArg,
    ) -> Result<RpcStream<proto::SubtreeRoot>, Status> {
        let (pool, start, limit) = subtree_request(request)?;
        let mut cached = self.subtree_roots.pool(pool).lock().await;
        let snapshot = self.snapshot();
        if !snapshot.ready {
            drop(cached);
            return self.uncached_subtree_roots(pool, start, limit, snapshot).await;
        }
        let roots = match cached.as_ref().filter(|roots| roots.serves(&snapshot)) {
            Some(roots) => roots.clone(),
            None => match self.all_subtree_roots(pool, &snapshot).await? {
                Some(roots) => cached.insert(roots).clone(),
                None => {
                    drop(cached);
                    return self.uncached_subtree_roots(pool, start, limit, snapshot).await;
                }
            },
        };
        drop(cached);
        Ok(roots.slice(start, limit))
    }

    /// Answers `GetSubtreeRoots` with no cache: asks the node for the subtrees of the request,
    /// and reads their completing blocks in `snapshot`. The answer ends after the first block
    /// that it cannot read, with the error of the read.
    pub(crate) async fn uncached_subtree_roots(
        &self,
        pool: Pool,
        start: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
        snapshot: ServingSnapshot,
    ) -> Result<RpcStream<proto::SubtreeRoot>, Status> {
        let subtrees = self.node_subtrees(pool, start, limit).await?;
        let roots = self.completing_hashes(subtrees, snapshot).await?;
        Ok(Box::pin(tokio_stream::iter(roots)))
    }

    /// All subtrees of `pool` from the node, with the completing blocks in the view of
    /// `snapshot`. Returns `None` if the node does not give the subtrees from index 0 with no
    /// gaps, or if the read of a block at or below the visible tip fails: the request then
    /// answers with no cache.
    async fn all_subtree_roots(
        &self,
        pool: Pool,
        snapshot: &ServingSnapshot,
    ) -> Result<Option<CachedRoots>, Status> {
        let mut subtrees = self.node_subtrees(pool, NoteCommitmentSubtreeIndex(0), None).await?;
        if subtrees.iter().enumerate().any(|(position, subtree)| subtree.index != position) {
            return Ok(None);
        }
        // The roots from the first one above the visible tip on fail with `OUT_OF_RANGE`.
        let tip = snapshot.visible_tip.map(|tip| tip.height);
        let in_view = subtrees
            .iter()
            .position(|subtree| tip.is_none_or(|tip| subtree.end > tip))
            .unwrap_or(subtrees.len());
        let above_tip =
            subtrees.split_off(in_view).into_iter().map(|subtree| subtree.end).collect();
        // A cached answer has no error before its end: a failed read answers with no cache.
        let roots = self.completing_hashes(subtrees, snapshot.clone()).await?;
        let roots: Result<Vec<_>, _> = roots.into_iter().collect();
        Ok(roots.ok().map(|roots| CachedRoots::new(snapshot, roots, above_tip)))
    }

    /// The completed subtrees of `pool` from `start`, at most `limit` of them, from the node.
    async fn node_subtrees(
        &self,
        pool: Pool,
        start: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> Result<Vec<NodeSubtree>, Status> {
        let backend = &self.backend;
        Ok(match pool {
            Pool::Sapling => {
                NodeSubtree::list(backend.sapling_subtrees(start, limit).await?, |root| {
                    root.to_bytes().to_vec()
                })
            }
            Pool::Orchard => {
                NodeSubtree::list(backend.orchard_subtrees(start, limit).await?, |root| {
                    root.to_repr().to_vec()
                })
            }
            Pool::Ironwood => {
                NodeSubtree::list(backend.ironwood_subtrees(start, limit).await?, |root| {
                    root.to_repr().to_vec()
                })
            }
        })
    }

    /// The roots of `subtrees`, each with the hash of its completing block from the volatile head
    /// of `snapshot`, or else from the index at its generation. One index snapshot reads the
    /// headers of all the blocks, and no block is decoded.
    async fn completing_hashes(
        &self,
        subtrees: Vec<NodeSubtree>,
        snapshot: ServingSnapshot,
    ) -> Result<Vec<Result<proto::SubtreeRoot, Status>>, Status> {
        let index = Arc::clone(&self.index);
        let span = Span::current();
        tokio::task::spawn_blocking(move || {
            span.in_scope(|| completing_hashes(&index, &snapshot, subtrees))
        })
        .await
        .map_err(|error| Status::unavailable(format!("LMDB reader failed: {error}")))
    }
}

/// One completed subtree from the node.
#[derive(Debug)]
struct NodeSubtree {
    /// The subtree index.
    index: usize,
    /// The root, in the byte order of its pool.
    root: Vec<u8>,
    /// The height of the completing block.
    end: u32,
}

impl NodeSubtree {
    /// The subtrees of a node answer, in index order. `bytes` gives the bytes of a root.
    fn list<Node>(subtrees: Subtrees<Node>, bytes: impl Fn(&Node) -> Vec<u8>) -> Vec<Self> {
        subtrees
            .into_iter()
            .map(|(index, subtree)| Self {
                index: usize::from(index.0),
                root: bytes(&subtree.root),
                end: subtree.end_height.0,
            })
            .collect()
    }

    /// The root of the subtree, with `hash`, the hash of its completing block.
    fn into_root(self, hash: block::Hash) -> proto::SubtreeRoot {
        proto::SubtreeRoot {
            root_hash: self.root,
            completing_block_hash: hash.0.to_vec(),
            completing_block_height: u64::from(self.end),
        }
    }
}

/// Checks the arguments of a `GetSubtreeRoots` request: the start index, the entry limit, where
/// 0 is no limit, then the pool.
fn subtree_request(
    request: proto::GetSubtreeRootsArg,
) -> Result<(Pool, NoteCommitmentSubtreeIndex, Option<NoteCommitmentSubtreeIndex>), Status> {
    let start = NoteCommitmentSubtreeIndex(
        request
            .start_index
            .try_into()
            .map_err(|_| Status::invalid_argument("subtree start index exceeds u16"))?,
    );
    let limit = match request.max_entries {
        0 => None,
        limit => Some(NoteCommitmentSubtreeIndex(
            limit
                .try_into()
                .map_err(|_| Status::invalid_argument("subtree entry limit exceeds u16"))?,
        )),
    };
    let pool = match proto::ShieldedProtocol::try_from(request.shielded_protocol) {
        Ok(proto::ShieldedProtocol::Sapling) => Pool::Sapling,
        Ok(proto::ShieldedProtocol::Orchard) => Pool::Orchard,
        Ok(proto::ShieldedProtocol::Ironwood) => Pool::Ironwood,
        Err(_) => return Err(Status::invalid_argument("invalid shielded protocol")),
    };
    Ok((pool, start, limit))
}

/// The roots of `subtrees` with the hashes of their completing blocks, read as
/// [`CompactService::completing_hashes`] says. It opens the index snapshot at the first block
/// that is not in the volatile head. The roots end after the first block that it cannot read,
/// with the error of the read.
fn completing_hashes(
    index: &Index,
    snapshot: &ServingSnapshot,
    subtrees: Vec<NodeSubtree>,
) -> Vec<Result<proto::SubtreeRoot, Status>> {
    let mut reader = None;
    let mut roots = Vec::with_capacity(subtrees.len());
    for subtree in subtrees {
        let hash = match snapshot.volatile(subtree.end) {
            Some(block) => Ok(block.hash),
            None => indexed_hash(index, snapshot.generation, &mut reader, subtree.end),
        };
        match hash {
            Ok(hash) => roots.push(Ok(subtree.into_root(hash))),
            Err(error) => {
                roots.push(Err(Status::from(error)));
                break;
            }
        }
    }
    roots
}

/// The hash of the block at `height` in the index at `generation`. `reader` is the index
/// snapshot of the earlier reads. The first read opens it.
fn indexed_hash<'index>(
    index: &'index Index,
    generation: u64,
    reader: &mut Option<IndexSnapshot<'index>>,
    height: u32,
) -> Result<block::Hash, IndexError> {
    let reader = match reader {
        Some(reader) => reader,
        None => reader.insert(index.snapshot(generation)?),
    };
    reader.block::<RecordHeader>(height).map(|header| header.hash)
}

#[cfg(test)]
mod tests;
