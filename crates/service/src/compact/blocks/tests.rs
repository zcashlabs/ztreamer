use super::*;
use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use tokio::sync::watch;
use tonic::Request;
use zakura_chain::{block::Block, orchard, sapling, serialization::ZcashDeserializeInto as _};
use zakura_test::vectors::BLOCK_MAINNET_1687121_BYTES;
use ztreamer_backend::TreeState;
use ztreamer_chain::{
    codec::{CompactBlockRecord, TreeSizes},
    parser::parse_block,
    test_utils::grow_chain,
    types::RawIndexBlock,
};
use ztreamer_index::{index::BlockId, ingest::OrderedBuilder};
use ztreamer_protocol::wire::compact_tx_streamer_server::CompactTxStreamer;
use ztreamer_sync::head::Readiness;

use crate::{
    compact::{DEFAULT_MAX_CONCURRENT_RANGES, RANGE_ADMISSION_TIMEOUT},
    test_utils::{EmptyChain, EphemeralService, NodeBlocks, hash, parsed, range, record},
};

#[tokio::test]
async fn streams_cross_range_descending_and_fetches_transparent_data() {
    let ctx = EphemeralService::sealed_through(1_005).await;
    assert_eq!(
        ctx.follower.readiness(),
        Readiness { historical: true, tip: false, recovering: false, source_error: None }
    );
    let descending = range(1_002, 998);
    let mut stream =
        ctx.service.get_block_range(Request::new(descending.clone())).await.unwrap().into_inner();
    let mut heights = Vec::new();
    while let Some(block) = stream.next().await {
        heights.push(block.unwrap().into_decoded().unwrap().height);
    }
    assert_eq!(heights, [1_002, 1_001, 1_000, 999, 998]);

    ctx.follower.publish_head(ctx.state, vec![record(1_006), record(1_007)]).unwrap();
    assert!(ctx.follower.readiness().tip);
    let mut stream =
        ctx.service.get_block_range(Request::new(range(1_004, 1_007))).await.unwrap().into_inner();
    let mut heights = Vec::new();
    while let Some(block) = stream.next().await {
        heights.push(block.unwrap().into_decoded().unwrap().height);
    }
    assert_eq!(heights, [1_004, 1_005, 1_006, 1_007]);

    let recovery_range = range(1_004, 1_007);
    let mut pinned = ctx
        .service
        .get_block_range(Request::new(recovery_range.clone()))
        .await
        .unwrap()
        .into_inner();
    ctx.follower.begin_recovery();
    assert_eq!(
        ctx.service.get_block_range(Request::new(recovery_range)).await.err().unwrap().code(),
        tonic::Code::Unavailable
    );
    let mut heights = Vec::new();
    while let Some(block) = pinned.next().await {
        heights.push(block.unwrap().into_decoded().unwrap().height);
    }
    assert_eq!(heights, [1_004, 1_005, 1_006, 1_007]);
    ctx.follower.publish_head(ctx.state, vec![record(1_006), record(1_007)]).unwrap();
    ctx.follower.mark_source_failure("offline".to_owned(), Duration::ZERO);
    assert!(!ctx.follower.readiness().tip);
    assert_eq!(
        ctx.service
            .get_latest_block(Request::new(proto::ChainSpec::default()))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );
    assert!(
        ctx.service
            .get_block(Request::new(proto::BlockId { height: 1_000, hash: Vec::new() }))
            .await
            .is_ok()
    );
    assert_eq!(
        ctx.service
            .get_transaction(Request::new(proto::TxFilter {
                hash: vec![0; 32],
                ..Default::default()
            }))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );

    // The backend has no block, so each transparent fetch fails. The stream ends at the first
    // error.
    let mut transparent = descending;
    transparent.pool_types = vec![proto::PoolType::Transparent as i32];
    let mut stream =
        ctx.service.get_block_range(Request::new(transparent)).await.unwrap().into_inner();
    assert_eq!(stream.next().await.unwrap().unwrap_err().code(), tonic::Code::Unavailable);
    assert!(stream.next().await.is_none());
    assert_eq!(
        CompactTxStreamer::send_transaction(
            &ctx.service,
            Request::new(proto::RawTransaction::default()),
        )
        .await
        .unwrap_err()
        .code(),
        tonic::Code::InvalidArgument
    );
    assert_eq!(
        CompactTxStreamer::get_mempool_tx(
            &ctx.service,
            Request::new(proto::GetMempoolTxRequest {
                pool_types: vec![proto::PoolType::Transparent as i32],
                ..Default::default()
            }),
        )
        .await
        .err()
        .unwrap()
        .code(),
        tonic::Code::Unavailable
    );
    assert_eq!(
        CompactTxStreamer::get_mempool_tx(
            &ctx.service,
            Request::new(proto::GetMempoolTxRequest::default()),
        )
        .await
        .err()
        .unwrap()
        .code(),
        tonic::Code::Unavailable
    );
    assert_eq!(
        CompactTxStreamer::get_mempool_stream(&ctx.service, Request::new(proto::Empty {}),)
            .await
            .err()
            .unwrap()
            .code(),
        tonic::Code::Unavailable
    );

    // A service on its own snapshot channel, so that the test can set any visible tip.
    let (snapshots, receiver) = watch::channel(ctx.follower.snapshot());
    let service =
        CompactService::with_backend(Arc::clone(&ctx.index), receiver, "main", EmptyChain)
            .with_node_version(EmptyChain::node_version());
    snapshots.send_modify(|snapshot| {
        snapshot.visible_tip = Some(BlockId::new(3_464_754, block::Hash([0; 32])));
    });
    let info = service.lightd_info();
    assert_eq!(info.sapling_activation_height, 419_200);
    assert_eq!(info.consensus_branch_id, "37a5165b");
    assert_eq!(info.block_height, 3_464_754);
    assert!(!info.zcashd_build.is_empty());
    assert!(!info.zcashd_subversion.is_empty());
}

#[tokio::test]
async fn range_admission_is_shared_and_releases_on_drop() {
    let ctx = EphemeralService::sealed_through(200).await;
    let clone = ctx.service.clone();
    let mut streams = Vec::new();
    for _ in 0..DEFAULT_MAX_CONCURRENT_RANGES.get() {
        streams.push(ctx.service.encoded_range(range(0, 200), false).await.unwrap());
    }
    // Poll once to prove the limit applies across service clones.
    let waiting = clone.encoded_range(range(0, 200), false);
    tokio::pin!(waiting);
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::future::Future::poll(waiting.as_mut(), cx).is_pending())
        })
        .await
    );
    drop(streams.pop());
    let mut admitted = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("dropping a response frees a slot")
        .unwrap();
    let mut count = 0;
    while let Some(block) = admitted.next().await {
        block.unwrap();
        count += 1;
    }
    assert_eq!(count, 201);
    drop(admitted);
    let _next_stream =
        tokio::time::timeout(Duration::from_secs(1), ctx.service.encoded_range(range(0, 0), false))
            .await
            .expect("completed response frees a slot")
            .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_full_range_limit_fails_with_unavailable_after_the_admission_timeout() {
    let ctx = EphemeralService::sealed_through(200).await;
    let service = ctx.service.with_max_concurrent_ranges(NonZeroUsize::new(1));
    let _held = service.encoded_range(range(0, 200), false).await.unwrap();

    let started = tokio::time::Instant::now();
    let error = service.encoded_range(range(0, 200), false).await.err().unwrap();
    assert_eq!(started.elapsed(), RANGE_ADMISSION_TIMEOUT);
    assert_eq!(error.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn range_stream_survives_a_durable_advance_mid_flight() {
    let ctx = EphemeralService::sealed_through(200).await;

    let mut stream = ctx
        .service
        .encoded_range(range(0, 200), false)
        .await
        .unwrap()
        .map(|block| block.map(|block| block.into_decoded().unwrap()));
    assert_eq!(stream.next().await.unwrap().unwrap().height, 0);

    // A durable advance lands under the stream.
    let mut builder = OrderedBuilder::new(ctx.state, 1024 * 1024).unwrap();
    for height in 201..=205 {
        builder.push(parsed(height)).unwrap();
    }
    let batch = builder
        .build_batch(Some(205), Some(205), 1024 * 1024)
        .unwrap()
        .expect("the builder holds five blocks");
    let advanced = ctx.index.write(batch).unwrap();
    assert_eq!(advanced.generation(), ctx.state.generation() + 1);

    let mut heights = vec![0];
    while let Some(block) = stream.next().await {
        heights.push(block.unwrap().height);
    }
    assert_eq!(heights, (0..=200).collect::<Vec<_>>());
}

#[tokio::test]
async fn range_stream_refuses_a_reorg_it_cannot_chain_onto() {
    let ctx = EphemeralService::sealed_through(200).await;

    let mut stream = ctx
        .service
        .encoded_range(range(0, 200), false)
        .await
        .unwrap()
        .map(|block| block.map(|block| block.into_decoded().unwrap()));
    assert_eq!(stream.next().await.unwrap().unwrap().height, 0);

    // A competing chain replaces everything above 30; the next chunk cannot link onto 63.
    let fork_hash = |height: u32| {
        let mut digest = hash(height);
        digest.0[31] = 0xff;
        digest
    };
    let fork: Vec<CompactBlockRecord> = (31..=205)
        .map(|height| {
            let mut forked = record(height);
            forked.hash = fork_hash(height);
            if height > 31 {
                forked.previous_hash = fork_hash(height - 1);
            }
            forked
        })
        .collect();
    ctx.index
        .replace_mutable_suffix(ctx.state.generation(), BlockId::new(30, hash(30)), fork, None)
        .unwrap();

    let mut heights = vec![0];
    let mut refusal = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(block) => heights.push(block.height),
            Err(status) => {
                refusal = Some(status);
                break;
            }
        }
    }
    assert_eq!(heights, (0..=63).collect::<Vec<_>>());
    assert_eq!(refusal.expect("the boundary is refused").code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn descending_range_from_the_volatile_head_survives_an_advance() {
    let ctx = EphemeralService::sealed_through(200).await;
    ctx.follower.publish_head(ctx.state, vec![record(201), record(202)]).unwrap();

    // The first chunk is volatile, the rest is durable and read after an advance.
    let mut stream = ctx
        .service
        .encoded_range(range(202, 100), false)
        .await
        .unwrap()
        .map(|block| block.map(|block| block.into_decoded().unwrap()));
    assert_eq!(stream.next().await.unwrap().unwrap().height, 202);
    let mut builder = OrderedBuilder::new(ctx.state, 1024 * 1024).unwrap();
    for height in 201..=205 {
        builder.push(parsed(height)).unwrap();
    }
    let batch = builder
        .build_batch(Some(205), Some(205), 1024 * 1024)
        .unwrap()
        .expect("the builder holds five blocks");
    ctx.index.write(batch).unwrap();

    let mut heights = vec![202];
    while let Some(block) = stream.next().await {
        heights.push(block.unwrap().height);
    }
    assert_eq!(heights, (100..=202).rev().collect::<Vec<_>>());
}

/// A chain of six blocks. Blocks 4 and 5 hold the transactions of mainnet block 1,687,121 after
/// their own coinbase. Returns the blocks, the records that the index makes of them, and the
/// tree state after block 5.
fn shielded_chain() -> (Vec<Arc<Block>>, Vec<CompactBlockRecord>, TreeState) {
    let mut chain = Vec::new();
    grow_chain(&mut chain, 5, 0);
    let shielded: Block = BLOCK_MAINNET_1687121_BYTES.zcash_deserialize_into().unwrap();
    // A block hash covers only the header, so the extra transactions keep the chain linked.
    for block in &mut chain[4..] {
        block.transactions.extend(shielded.transactions[1..].iter().cloned());
    }
    let mut sizes = TreeSizes::default();
    let records: Vec<_> = chain
        .iter()
        .map(|block| {
            let raw = RawIndexBlock::try_from(block).unwrap();
            let record = parse_block(&raw).unwrap().into_record(sizes).unwrap();
            sizes = record.end_tree_sizes;
            record
        })
        .collect();
    assert!(sizes.sapling > 0 && sizes.orchard > 0 && sizes.ironwood == 0, "{sizes:?}");
    let sapling_leaf = sapling::tree::NoteCommitmentUpdate::from_bytes(&[1; 32]).unwrap();
    let sapling: sapling::tree::NoteCommitmentTree =
        vec![sapling_leaf; sizes.sapling as usize].into();
    let orchard: orchard::tree::NoteCommitmentTree =
        vec![orchard::tree::NoteCommitmentUpdate::from(1); sizes.orchard as usize].into();
    let trees = TreeState {
        sapling: Some(Arc::new(sapling)),
        orchard: Some(Arc::new(orchard)),
        ironwood: None,
    };
    (chain.into_iter().map(Arc::new).collect(), records, trees)
}

fn at_height(height: u32) -> Request<proto::BlockId> {
    Request::new(proto::BlockId { height: u64::from(height), hash: Vec::new() })
}

#[tokio::test]
async fn block_above_the_tip_comes_from_the_node_as_the_index_serves_it() {
    let (chain, records, trees) = shielded_chain();
    let node = NodeBlocks::default().with_block(Arc::clone(&chain[5]), trees);
    let ctx = EphemeralService::with_backend(&records[..5], None, node).await;
    let indexed = EphemeralService::with_records(&records, None).await;

    let block = ctx.service.get_block(at_height(5)).await.unwrap().into_inner();
    let expected = indexed.service.get_block(at_height(5)).await.unwrap().into_inner();
    assert_eq!(block.to_bytes(), expected.to_bytes());

    // The node has no block 6. A hash outside the index does not reach the node.
    assert_eq!(
        ctx.service.get_block(at_height(6)).await.unwrap_err().code(),
        tonic::Code::OutOfRange
    );
    let by_hash = proto::BlockId { height: 0, hash: chain[5].hash().0.to_vec() };
    assert_eq!(
        ctx.service.get_block(Request::new(by_hash)).await.unwrap_err().code(),
        tonic::Code::NotFound
    );
}
