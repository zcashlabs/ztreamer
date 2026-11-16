//! Commitment tree requests: tree states and note commitment subtree roots.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;
use tracing::Span;
use zakura_chain::subtree::NoteCommitmentSubtreeIndex;
use ztreamer_chain::codec::CompactBlockRecord;
use ztreamer_index::index::IndexError;
use ztreamer_protocol::proto;
use ztreamer_snapshot::manifest_entry::Frontiers;

use super::{CompactService, RpcStream, ServingSnapshot};

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

    fn subtree_stream(
        &self,
        roots: Vec<(Vec<u8>, u32)>,
        snapshot: ServingSnapshot,
    ) -> RpcStream<proto::SubtreeRoot> {
        let index = Arc::clone(&self.index);
        let (sender, receiver) = mpsc::channel(1);
        let span = Span::current();
        tokio::task::spawn_blocking(move || {
            let _span = span.entered();
            for (root_hash, height) in roots {
                let block = match snapshot.volatile(height) {
                    Some(block) => block.clone(),
                    None => match index
                        .snapshot(snapshot.generation)
                        .and_then(|index_snapshot| index_snapshot.block(height))
                    {
                        Ok(block) => block,
                        Err(error) => {
                            let _ = sender.blocking_send(Err(Status::from(error)));
                            return;
                        }
                    },
                };
                if sender
                    .blocking_send(Ok(proto::SubtreeRoot {
                        root_hash,
                        completing_block_hash: block.hash.0.to_vec(),
                        completing_block_height: u64::from(height),
                    }))
                    .is_err()
                {
                    return;
                }
            }
        });
        Box::pin(ReceiverStream::new(receiver))
    }

    /// Answers `GetLatestTreeState`.
    pub async fn latest_tree_state(&self) -> Result<proto::TreeState, Status> {
        self.tree_state(self.latest_block()?).await
    }

    /// Answers `GetSubtreeRoots`.
    pub async fn subtree_roots(
        &self,
        request: proto::GetSubtreeRootsArg,
    ) -> Result<RpcStream<proto::SubtreeRoot>, Status> {
        let start_index = NoteCommitmentSubtreeIndex(
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
        let backend = &self.backend;
        let roots = match proto::ShieldedProtocol::try_from(request.shielded_protocol) {
            Ok(proto::ShieldedProtocol::Sapling) => backend
                .sapling_subtrees(start_index, limit)
                .await?
                .into_values()
                .map(|subtree| (subtree.root.to_bytes().to_vec(), subtree.end_height.0))
                .collect(),
            Ok(proto::ShieldedProtocol::Orchard) => backend
                .orchard_subtrees(start_index, limit)
                .await?
                .into_values()
                .map(|subtree| (subtree.root.to_repr().to_vec(), subtree.end_height.0))
                .collect(),
            Ok(proto::ShieldedProtocol::Ironwood) => backend
                .ironwood_subtrees(start_index, limit)
                .await?
                .into_values()
                .map(|subtree| (subtree.root.to_repr().to_vec(), subtree.end_height.0))
                .collect(),
            Err(_) => return Err(Status::invalid_argument("invalid shielded protocol")),
        };
        Ok(self.subtree_stream(roots, self.snapshot()))
    }
}

#[cfg(test)]
mod tests {
    use tokio_stream::StreamExt;

    use ztreamer_chain::test_utils::mixed_records;
    use ztreamer_protocol::proto;

    use crate::test_utils::{EphemeralService, hash, record};

    #[tokio::test]
    async fn tree_state_comes_from_the_index_once_its_trees_reach_the_block() {
        let records = mixed_records(40);
        let ctx = EphemeralService::with_records(&records, None).await;
        let request = |height| proto::BlockId { height, hash: Vec::new() };

        // The trees of the index reach no block: the backend answers, with empty trees.
        let from_backend = ctx.service.tree_state(request(39)).await.unwrap();
        assert_eq!(from_backend.sapling_tree, "");

        while ctx.index.extend_trees(2).unwrap().is_some() {}
        let expected = ctx.index.latest().unwrap().tree_frontiers(39, records[39].end_tree_sizes);
        let expected = expected.unwrap().unwrap();
        let tree_state = ctx.service.tree_state(request(39)).await.unwrap();
        assert_eq!(tree_state.hash, records[39].hash.to_string());
        assert_eq!(tree_state.sapling_tree, hex::encode(&expected.sapling));
        assert_eq!(tree_state.orchard_tree, hex::encode(&expected.orchard));
        assert_eq!(tree_state.ironwood_tree, hex::encode(&expected.ironwood));
        assert!(!expected.sapling.is_empty() && !expected.orchard.is_empty());
    }

    #[tokio::test]
    async fn subtree_stream_resolves_volatile_completion_blocks() {
        let ctx = EphemeralService::sealed_through(1_005).await;
        ctx.follower.publish_head(ctx.state, vec![record(1_006), record(1_007)]).unwrap();

        let mut stream =
            ctx.service.subtree_stream(vec![(vec![7; 32], 1_007)], ctx.service.snapshot());
        let root = stream.next().await.unwrap().unwrap();

        assert_eq!(root.root_hash, vec![7; 32]);
        assert_eq!(root.completing_block_hash, hash(1_007).0);
        assert_eq!(root.completing_block_height, 1_007);
    }
}
