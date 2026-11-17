use super::*;

use tokio_stream::StreamExt;
use tonic::Code;
use ztreamer_chain::test_utils::mixed_records;
use ztreamer_index::index::BlockId;

use crate::test_utils::{EphemeralService, NodeSubtrees, hash, record};

/// The durable tip of [`subtree_service`]. The index seals 0 to 999 and holds 1,000 to 1,005 as
/// mutable rows.
const DURABLE_TIP: u32 = 1_005;
/// The visible tip of [`subtree_service`]: the volatile head holds 1,006 to 1,009.
const VISIBLE_TIP: u32 = 1_009;

const POOLS: [(Pool, proto::ShieldedProtocol); 3] = [
    (Pool::Sapling, proto::ShieldedProtocol::Sapling),
    (Pool::Orchard, proto::ShieldedProtocol::Orchard),
    (Pool::Ironwood, proto::ShieldedProtocol::Ironwood),
];

/// The roots of an answer, and the code and the message of an error.
type Answer = Vec<Result<proto::SubtreeRoot, (Code, String)>>;

/// A service over empty blocks to [`VISIBLE_TIP`], whose node completed the subtrees of
/// [`heights`].
async fn subtree_service() -> (EphemeralService, NodeSubtrees) {
    let node = NodeSubtrees::default();
    for (pool, _) in POOLS {
        heights(pool).iter().for_each(|&height| node.complete(pool, height));
    }
    let records: Vec<_> = (0..=DURABLE_TIP)
        .map(|height| ztreamer_chain::test_utils::record(height, Vec::new()))
        .collect();
    let ctx = EphemeralService::with_backend(&records, Some(999), node.clone()).await;
    let head = (DURABLE_TIP + 1..=VISIBLE_TIP).map(record).collect();
    ctx.follower.publish_head(ctx.state, head).unwrap();
    (ctx, node)
}

/// The blocks of a fork from `from` to `to`: each hash ends in 0xaa, and the first block links
/// to `parent`.
fn fork(from: u32, to: u32, parent: block::Hash) -> Vec<CompactBlockRecord> {
    (from..=to)
        .scan(parent, |parent, height| {
            let mut block = record(height);
            block.hash.0[31] = 0xaa;
            block.previous_hash = std::mem::replace(parent, block.hash);
            Some(block)
        })
        .collect()
}

fn request(
    protocol: proto::ShieldedProtocol,
    start_index: u32,
    max_entries: u32,
) -> proto::GetSubtreeRootsArg {
    proto::GetSubtreeRootsArg { start_index, shielded_protocol: protocol as i32, max_entries }
}

async fn collect(stream: RpcStream<proto::SubtreeRoot>) -> Answer {
    stream
        .map(|root| root.map_err(|status| (status.code(), status.message().to_owned())))
        .collect()
        .await
}

async fn cached(ctx: &EphemeralService, request: proto::GetSubtreeRootsArg) -> Answer {
    collect(ctx.service.subtree_roots(request).await.unwrap()).await
}

async fn uncached(ctx: &EphemeralService, request: proto::GetSubtreeRootsArg) -> Answer {
    let (pool, start, limit) = subtree_request(request).unwrap();
    let snapshot = ctx.service.snapshot();
    collect(ctx.service.uncached_subtree_roots(pool, start, limit, snapshot).await.unwrap()).await
}

/// The answer that a node with `heights` gives for `pool` from `start`, at most `max` roots,
/// with `hash` of each completing height.
fn expected(
    pool: Pool,
    heights: &[u32],
    start: u32,
    max: u32,
    hash: impl Fn(u32) -> block::Hash,
) -> Answer {
    let end = if max == 0 { heights.len() } else { (start + max) as usize };
    (start as usize..end.min(heights.len()))
        .map(|index| {
            Ok(proto::SubtreeRoot {
                root_hash: NodeSubtrees::root(pool, index as u16).to_vec(),
                completing_block_hash: hash(heights[index]).0.to_vec(),
                completing_block_height: u64::from(heights[index]),
            })
        })
        .collect()
}

/// The completing heights of the subtrees of `pool` in [`subtree_service`]: in sealed, mutable
/// and volatile blocks, two of them in one block.
fn heights(pool: Pool) -> &'static [u32] {
    match pool {
        Pool::Sapling => &[0, 3, 3, 500, 999, 1_000, 1_004, 1_007, 1_009],
        Pool::Orchard => &[10, 1_001, 1_008],
        Pool::Ironwood => &[1_006],
    }
}

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

/// Each pool, with slices that start at 0, inside, at the end and after the end, with no limit,
/// small limits and limits past the end. Each answer is the node's roots with the hashes of the
/// sealed, mutable and volatile completing blocks, in the stored byte order, and is the answer
/// with no cache.
#[tokio::test]
async fn cached_answers_are_the_uncached_answers_for_each_pool_and_slice() {
    let (ctx, _node) = subtree_service().await;
    let slices =
        [(0, 0), (0, 1), (0, 3), (1, 1), (2, 0), (2, 2), (3, 100), (8, 0), (9, 0), (50, 3)];
    for (pool, protocol) in POOLS {
        for (start, max) in slices.into_iter().chain([(0, u32::from(u16::MAX))]) {
            let request = request(protocol, start, max);
            let answer = cached(&ctx, request).await;
            assert_eq!(
                answer,
                expected(pool, heights(pool), start, max, hash),
                "{pool:?} {start} {max}"
            );
            assert_eq!(answer, uncached(&ctx, request).await, "{pool:?} {start} {max}");
        }
    }
}

#[tokio::test]
async fn one_view_asks_the_node_once_for_each_pool() {
    let (ctx, node) = subtree_service().await;

    for (start, max) in [(0, 0), (3, 2), (0, 0)] {
        cached(&ctx, request(proto::ShieldedProtocol::Sapling, start, max)).await;
    }
    assert_eq!(node.requests(), 1);
    cached(&ctx, request(proto::ShieldedProtocol::Orchard, 0, 0)).await;
    assert_eq!(node.requests(), 2);

    // A new snapshot of the same blocks, as after a failed poll of the head source, keeps the
    // view.
    ctx.follower.mark_source_failure("poll failed".into(), std::time::Duration::from_secs(30));
    cached(&ctx, request(proto::ShieldedProtocol::Sapling, 0, 0)).await;
    assert_eq!(node.requests(), 2);

    let head = (DURABLE_TIP + 1..=VISIBLE_TIP + 1).map(record).collect();
    ctx.follower.publish_head(ctx.state, head).unwrap();
    cached(&ctx, request(proto::ShieldedProtocol::Sapling, 0, 0)).await;
    assert_eq!(node.requests(), 3);
}

/// A subtree that the node completes above the visible tip is not served from the cache of the
/// view, and is served once the head has its block.
#[tokio::test]
async fn a_new_subtree_is_served_from_the_first_view_with_its_block() {
    let (ctx, node) = subtree_service().await;
    let sapling = |start, max| request(proto::ShieldedProtocol::Sapling, start, max);
    let before = cached(&ctx, sapling(0, 0)).await;

    node.complete(Pool::Sapling, VISIBLE_TIP + 1);
    assert_eq!(cached(&ctx, sapling(0, 0)).await, before);

    let head = (DURABLE_TIP + 1..=VISIBLE_TIP + 1).map(record).collect();
    ctx.follower.publish_head(ctx.state, head).unwrap();
    let after = cached(&ctx, sapling(0, 0)).await;
    let heights = [heights(Pool::Sapling), &[VISIBLE_TIP + 1]].concat();
    assert_eq!(after, expected(Pool::Sapling, &heights, 0, 0, hash));
    assert_eq!(after, uncached(&ctx, sapling(0, 0)).await);
}

/// A root whose completing block is above the visible tip when the answer is built ends each
/// slice that reaches it with `OUT_OF_RANGE`, as with no cache. The slices before it have no
/// error.
#[tokio::test]
async fn a_root_above_the_visible_tip_fails_as_with_no_cache() {
    let (ctx, node) = subtree_service().await;
    node.complete(Pool::Orchard, VISIBLE_TIP + 1);
    node.complete(Pool::Orchard, VISIBLE_TIP + 2);

    for (start, max) in [(0, 0), (0, 3), (0, 4), (2, 0), (3, 0), (4, 0), (4, 1), (5, 0)] {
        let request = request(proto::ShieldedProtocol::Orchard, start, max);
        let answer = cached(&ctx, request).await;
        assert_eq!(answer, uncached(&ctx, request).await, "{start} {max}");
        let ends_out_of_range = matches!(answer.last(), Some(Err((Code::OutOfRange, _))));
        let end = if max == 0 { u32::MAX } else { start + max };
        assert_eq!(ends_out_of_range, start < 5 && end > 3, "{start} {max}");
    }
}

/// A reorg of the volatile head changes the visible tip. The next answer has the hashes of the
/// new blocks.
#[tokio::test]
async fn a_volatile_reorg_replaces_the_completing_hashes() {
    let (ctx, _node) = subtree_service().await;
    let sapling = request(proto::ShieldedProtocol::Sapling, 0, 0);
    assert_eq!(
        cached(&ctx, sapling).await,
        expected(Pool::Sapling, heights(Pool::Sapling), 0, 0, hash)
    );

    let head = fork(1_007, VISIBLE_TIP, hash(1_006));
    let forked = |height| {
        head.iter().find(|block| block.height == height).map_or(hash(height), |block| block.hash)
    };
    let mut volatile = vec![record(1_006)];
    volatile.extend(head.iter().cloned());
    ctx.follower.publish_head(ctx.state, volatile).unwrap();

    for (pool, protocol) in POOLS {
        let request = request(protocol, 0, 0);
        let answer = cached(&ctx, request).await;
        assert_eq!(answer, expected(pool, heights(pool), 0, 0, forked), "{pool:?}");
        assert_eq!(answer, uncached(&ctx, request).await, "{pool:?}");
    }
}

/// A reorg of the mutable rows commits a new generation. The next answer has the hashes of the
/// new durable blocks.
#[tokio::test]
async fn a_durable_reorg_replaces_the_completing_hashes() {
    let (ctx, _node) = subtree_service().await;
    let sapling = request(proto::ShieldedProtocol::Sapling, 0, 0);
    assert_eq!(
        cached(&ctx, sapling).await,
        expected(Pool::Sapling, heights(Pool::Sapling), 0, 0, hash)
    );

    let blocks = fork(1_003, VISIBLE_TIP, hash(1_002));
    let (durable, volatile) = blocks.split_at((DURABLE_TIP - 1_002) as usize);
    let ancestor = BlockId::new(1_002, hash(1_002));
    let state = ctx
        .index
        .replace_mutable_suffix(ctx.state.generation(), ancestor, durable.to_vec(), None)
        .unwrap();
    assert_ne!(state.generation(), ctx.state.generation());
    ctx.follower.publish_head(state, volatile.to_vec()).unwrap();

    let forked = |height| {
        blocks.iter().find(|block| block.height == height).map_or(hash(height), |block| block.hash)
    };
    for (pool, protocol) in POOLS {
        let request = request(protocol, 0, 0);
        let answer = cached(&ctx, request).await;
        assert_eq!(answer, expected(pool, heights(pool), 0, 0, forked), "{pool:?}");
        assert_eq!(answer, uncached(&ctx, request).await, "{pool:?}");
    }
}

/// While deep reorg recovery runs, each request asks the node, as with no cache.
#[tokio::test]
async fn recovery_answers_with_no_cache() {
    let (ctx, node) = subtree_service().await;
    let sapling = request(proto::ShieldedProtocol::Sapling, 1, 4);
    let before = cached(&ctx, sapling).await;
    assert_eq!(node.requests(), 1);

    ctx.follower.begin_recovery();
    assert_eq!(cached(&ctx, sapling).await, before);
    assert_eq!(cached(&ctx, sapling).await, before);
    assert_eq!(node.requests(), 3);
}

#[tokio::test]
async fn invalid_arguments_fail_before_the_node_is_asked() {
    let (ctx, node) = subtree_service().await;
    let sapling = proto::ShieldedProtocol::Sapling;
    for (request, message) in [
        (request(sapling, 1 << 16, 0), "subtree start index exceeds u16"),
        (request(sapling, 0, 1 << 16), "subtree entry limit exceeds u16"),
        (
            proto::GetSubtreeRootsArg { shielded_protocol: 7, ..request(sapling, 0, 0) },
            "invalid shielded protocol",
        ),
        (
            proto::GetSubtreeRootsArg { shielded_protocol: 7, ..request(sapling, 1 << 16, 0) },
            "subtree start index exceeds u16",
        ),
    ] {
        let status = ctx.service.subtree_roots(request).await.err().unwrap();
        assert_eq!((status.code(), status.message()), (Code::InvalidArgument, message));
    }
    assert_eq!(node.requests(), 0);
}
