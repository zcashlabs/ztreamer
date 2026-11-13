//! Blocks above the visible tip, fetched from the node.
//!
//! A block that the index does not have yet comes from the node, as lightwalletd fetches a block
//! that is not in its cache. The conversion is the one of the index: the compact parser, then
//! [`ParsedCompactBlock::into_record`](ztreamer_chain::types::ParsedCompactBlock::into_record).
//! The record is thus equal to the record that the index stores for the block later. The tree
//! sizes come from the tree state of the node at the block.

use tonic::Status;
use tracing::Span;
use zakura_chain::block;
use ztreamer_backend::{BlockHandle, StateBackend, TreeState};
use ztreamer_chain::{
    codec::{CompactBlockRecord, TreeSizes},
    parser::parse_block,
    types::RawIndexBlock,
};

/// The record of the best-chain block at `height` of the node.
///
/// Fails with `OUT_OF_RANGE` if the node has no block at `height`, as lightwalletd does for a
/// block above the tip of its node, and with `UNAVAILABLE` if the node does not answer.
#[tracing::instrument(
    name = "block.node_fetch",
    level = "debug",
    skip_all,
    fields(block_height = height)
)]
pub(crate) async fn node_record(
    backend: &dyn StateBackend,
    height: u32,
) -> Result<CompactBlockRecord, Status> {
    let block = backend
        .block_at_height(block::Height(height))
        .await?
        .ok_or_else(|| Status::out_of_range(format!("block {height} is above the chain tip")))?;
    let end_tree_sizes = tree_sizes(&backend.tree_state(block.hash()).await?)?;
    let span = Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(|| compact_record(&block, end_tree_sizes)))
        .await
        .map_err(|error| Status::unavailable(format!("block conversion failed: {error}")))?
}

/// Converts `block` as the index does. `end_tree_sizes` are the tree sizes after the block.
fn compact_record(
    block: &BlockHandle,
    end_tree_sizes: TreeSizes,
) -> Result<CompactBlockRecord, Status> {
    let raw = RawIndexBlock::try_from(block.view())
        .map_err(|error| Status::internal(error.to_string()))?;
    let parsed = parse_block(&raw).map_err(|error| Status::internal(error.to_string()))?;
    end_tree_sizes
        .checked_sub(parsed.additions())
        .and_then(|before| parsed.into_record(before))
        .ok_or_else(|| {
            Status::internal(format!(
                "the node trees at block {} are smaller than the commitments of the block",
                raw.height.0
            ))
        })
}

/// The size of each tree, by its leaves. A pool that is not active has the size 0.
fn tree_sizes(trees: &TreeState) -> Result<TreeSizes, Status> {
    let size = |count: Option<u64>| {
        u32::try_from(count.unwrap_or(0))
            .map_err(|_| Status::internal("a node commitment tree exceeds u32::MAX leaves"))
    };
    Ok(TreeSizes {
        sapling: size(trees.sapling.as_ref().map(|tree| tree.count()))?,
        orchard: size(trees.orchard.as_ref().map(|tree| tree.count()))?,
        ironwood: size(trees.ironwood.as_ref().map(|tree| tree.count()))?,
    })
}
