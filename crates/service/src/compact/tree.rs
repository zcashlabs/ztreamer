//! Commitment tree requests: tree states and note commitment subtree roots.

use std::sync::Arc;

use tokio::sync::Mutex;
use tonic::Status;
use tracing::Span;
use zakura_chain::{block, subtree::NoteCommitmentSubtreeIndex};
use ztreamer_chain::codec::CompactBlockRecord;
use ztreamer_index::index::{BlockId, Index, IndexError, IndexSnapshot};
use ztreamer_protocol::proto;
use ztreamer_snapshot::manifest_entry::Frontiers;

use super::{CompactService, RpcStream, ServingSnapshot};

/// The `GetSubtreeRoots` answer of each pool from index 0, by `ShieldedProtocol` value, with the
/// view that it was built in: the index generation and the visible tip. Each index commit, also a
/// reorg replacement, increases the generation, so it fixes the durable blocks. The volatile head
/// links the durable tip to the visible tip by hash, so the tip fixes the volatile head, and a
/// volatile reorg changes it. A request in another view rebuilds, so no root is served with a
/// hash that a reorg replaced. The node is asked once per view: a subtree that completes shows
/// once the head has its block. An answer with an error, or built while the snapshot is not
/// ready, is not cached.
pub(crate) type SubtreeRootCache =
    [Mutex<Option<((u64, Option<BlockId>), Arc<[proto::SubtreeRoot]>)>>; 3];

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

    /// The roots with the hashes of their completing blocks, from the volatile head of `snapshot`
    /// or else from one index snapshot at its generation. The answer ends after the first block
    /// that cannot be read, with the error of the read.
    async fn completing_roots(
        &self,
        roots: Vec<(Vec<u8>, u32)>,
        snapshot: ServingSnapshot,
    ) -> Result<Vec<Result<proto::SubtreeRoot, Status>>, Status> {
        let index = Arc::clone(&self.index);
        let span = Span::current();
        tokio::task::spawn_blocking(move || {
            let _span = span.entered();
            let (mut reader, mut answer) = (None, Vec::with_capacity(roots.len()));
            for (root_hash, height) in roots {
                let hash = match snapshot.volatile(height) {
                    Some(block) => Ok(block.hash),
                    None => indexed_hash(&index, snapshot.generation, &mut reader, height),
                };
                let Ok(hash) = hash.map_err(|error| answer.push(Err(Status::from(error)))) else {
                    break;
                };
                answer.push(Ok(proto::SubtreeRoot {
                    root_hash,
                    completing_block_hash: hash.0.to_vec(),
                    completing_block_height: u64::from(height),
                }));
            }
            answer
        })
        .await
        .map_err(|error| Status::unavailable(format!("LMDB reader failed: {error}")))
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
        let protocol = proto::ShieldedProtocol::try_from(request.shielded_protocol)
            .map_err(|_| Status::invalid_argument("invalid shielded protocol"))?;
        let slice = |len: usize| {
            let start = usize::from(start_index.0).min(len);
            start..limit.map_or(len, |limit| len.min(start + usize::from(limit.0)))
        };
        let mut cached = self.subtree_roots[protocol as usize].lock().await;
        let snapshot = self.snapshot();
        let view = (snapshot.generation, snapshot.visible_tip);
        if let Some((_, built)) =
            cached.as_ref().filter(|(built, _)| snapshot.ready && *built == view)
        {
            let built = Arc::clone(built);
            return Ok(Box::pin(tokio_stream::iter(
                slice(built.len()).map(move |i| Ok(built[i].clone())),
            )));
        }
        let (all, backend) = (NoteCommitmentSubtreeIndex(0), &self.backend);
        let roots: Vec<_> = match protocol {
            proto::ShieldedProtocol::Sapling => backend
                .sapling_subtrees(all, None)
                .await?
                .into_values()
                .map(|subtree| (subtree.root.to_bytes().to_vec(), subtree.end_height.0))
                .collect(),
            proto::ShieldedProtocol::Orchard => backend
                .orchard_subtrees(all, None)
                .await?
                .into_values()
                .map(|subtree| (subtree.root.to_repr().to_vec(), subtree.end_height.0))
                .collect(),
            proto::ShieldedProtocol::Ironwood => backend
                .ironwood_subtrees(all, None)
                .await?
                .into_values()
                .map(|subtree| (subtree.root.to_repr().to_vec(), subtree.end_height.0))
                .collect(),
        };
        let answer = self.completing_roots(roots.clone(), snapshot.clone()).await?;
        let built: Arc<[_]> = match answer.into_iter().collect() {
            Ok(built) if snapshot.ready => Arc::clone(&cached.insert((view, built)).1),
            Ok(built) => built,
            // The roots of the request, read alone, end with the error as before.
            Err(_) => {
                let answer = self.completing_roots(roots[slice(roots.len())].to_vec(), snapshot);
                return Ok(Box::pin(tokio_stream::iter(answer.await?)));
            }
        };
        Ok(Box::pin(tokio_stream::iter(slice(built.len()).map(move |i| Ok(built[i].clone())))))
    }
}

/// The hash of the block at `height` in the index at `generation`. The first read opens
/// `reader`, the later reads use it.
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
    reader.block::<CompactBlockRecord>(height).map(|block| block.hash)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tokio_stream::StreamExt;
    use zakura_chain::parameters::Network;
    use zakura_rpc::client::GetBlockchainInfoResponse;
    use ztreamer_chain::{codec::CompactBlockRecord, test_utils::mixed_records};
    use ztreamer_protocol::proto;
    use ztreamer_remote::{
        RemoteBackend, RpcClient, ZakuraClient,
        test_utils::{MockNode, Reply},
    };

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

    /// A service over empty blocks 0 to 1,009, of which 1,006 to 1,009 are volatile, and a remote
    /// node whose subtrees of each pool complete at the heights in `ends`.
    async fn subtree_service(ends: Arc<Mutex<Vec<u32>>>) -> (EphemeralService, MockNode) {
        let mock = MockNode::start(move |_, seen| match seen.body["method"].as_str().unwrap() {
            "getblockchaininfo" => {
                Reply::Result(serde_json::to_value(GetBlockchainInfoResponse::default()).unwrap())
            }
            "getblockhash" => Reply::Result(json!(Network::Mainnet.genesis_hash().to_string())),
            _ => {
                let (start, ends) = (seen.body["params"][1].as_u64().unwrap() as usize, ends.lock().unwrap());
                let limit = seen.body["params"][2].as_u64().map_or(usize::MAX, |limit| limit as usize);
                let subtrees: Vec<_> = (start..ends.len()).take(limit).map(|i| {
                    json!({"root": hex::encode([i as u8; 1]) + &"00".repeat(31), "end_height": ends[i]})
                }).collect();
                Reply::Result(json!({"pool": seen.body["params"][0], "start_index": start, "subtrees": subtrees}))
            }
        })
        .await;
        let node = ZakuraClient::new(RpcClient::new(mock.config()).unwrap());
        let backend = RemoteBackend::connect(node, Network::Mainnet).await.unwrap();
        let records: Vec<_> = (0..=1_005)
            .map(|height| ztreamer_chain::test_utils::record(height, Vec::new()))
            .collect();
        let ctx = EphemeralService::with_backend(&records, Some(999), backend).await;
        ctx.follower.publish_head(ctx.state, (1_006..=1_009).map(record).collect()).unwrap();
        (ctx, mock)
    }

    type Roots = Result<Vec<(Vec<u8>, Vec<u8>, u64)>, tonic::Code>;

    async fn roots(
        ctx: &EphemeralService,
        protocol: proto::ShieldedProtocol,
        start: u32,
        max: u32,
    ) -> Roots {
        let request = proto::GetSubtreeRootsArg {
            start_index: start,
            shielded_protocol: protocol as i32,
            max_entries: max,
        };
        let stream = ctx.service.subtree_roots(request).await.unwrap();
        stream
            .map(|root| {
                root.map(|root| {
                    (root.root_hash, root.completing_block_hash, root.completing_block_height)
                })
                .map_err(|status| status.code())
            })
            .collect()
            .await
    }

    fn subtree_requests(mock: &MockNode) -> usize {
        mock.seen().iter().filter(|seen| seen.body["method"] == "z_getsubtreesbyindex").count()
    }

    #[tokio::test]
    async fn cached_subtree_roots_are_the_node_roots_with_their_block_hashes() {
        let ends = vec![0, 3, 3, 999, 1_000, 1_004, 1_007, 1_009];
        let (ctx, _mock) =
            subtree_service(Arc::new(Mutex::new([ends.clone(), vec![1_012]].concat()))).await;
        for protocol in [proto::ShieldedProtocol::Sapling, proto::ShieldedProtocol::Orchard] {
            for (start, max) in [(0, 2), (2, 3), (6, 2), (7, 1), (9, 0), (20, 0)] {
                let end =
                    if max == 0 { ends.len() } else { ends.len().min((start + max) as usize) };
                let expected = (start as usize..end).map(|i| {
                    (
                        [vec![i as u8], vec![0; 31]].concat(),
                        hash(ends[i]).0.to_vec(),
                        u64::from(ends[i]),
                    )
                });
                assert_eq!(
                    roots(&ctx, protocol, start, max).await,
                    Ok(expected.collect()),
                    "{start} {max}"
                );
            }
            // The root at 1,012 is above the visible tip: the read of its block fails, as before.
            for (start, max) in [(0, 0), (8, 0), (5, 9)] {
                assert_eq!(roots(&ctx, protocol, start, max).await, Err(tonic::Code::OutOfRange));
            }
        }
    }

    #[tokio::test]
    async fn a_new_view_rebuilds_and_a_reorged_hash_is_not_served() {
        let ends = Arc::new(Mutex::new(vec![10, 1_007]));
        let (ctx, mock) = subtree_service(Arc::clone(&ends)).await;
        let sapling = || roots(&ctx, proto::ShieldedProtocol::Sapling, 0, 0);
        assert_eq!(sapling().await.unwrap()[1].1, hash(1_007).0.to_vec());
        sapling().await.unwrap();
        assert_eq!(subtree_requests(&mock), 1);

        // A volatile reorg replaces 1,007 to 1,009.
        let mut fork: Vec<_> = (1_006..=1_009).map(record).collect();
        for i in 1..fork.len() {
            fork[i].hash.0[31] = 0xaa;
            fork[i].previous_hash = fork[i - 1].hash;
        }
        ctx.follower.publish_head(ctx.state, fork.clone()).unwrap();
        assert_eq!(sapling().await.unwrap()[1].1, fork[1].hash.0.to_vec());
        assert_eq!(subtree_requests(&mock), 2);

        // A subtree completes in a new block.
        ends.lock().unwrap().push(1_010);
        let mut head = fork;
        head.push(CompactBlockRecord { previous_hash: head[3].hash, ..record(1_010) });
        ctx.follower.publish_head(ctx.state, head).unwrap();
        assert_eq!(sapling().await.unwrap()[2].1, hash(1_010).0.to_vec());
    }
}
