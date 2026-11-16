//! The extension of the note commitment trees over the blocks after them. [`TreeRead`] reads
//! the blocks and the start of the trees in one snapshot, [`TreeBatch`] takes their commitments,
//! and [`TreeRows`] holds the rows that one write adds. Only the read and the write hold an LMDB
//! transaction. The decode and the hashes run on scoped threads between them.

use std::collections::BTreeMap;

use heed::RwTxn;
use sapling_crypto::Node as SaplingNode;
use zakura_chain::{block, orchard};
use ztreamer_chain::codec::{CompactBlockRecord, RangeDecoder, TreeSizes};
use ztreamer_snapshot::pool::Pool;

use super::{
    BlockId, IndexError, IndexSnapshot, RANGE_SIZE, TreeError,
    tables::{Tables, range_start},
    tree_tables::pool_size,
};
use crate::trees::{
    DEPTH, LowFrontier, Lows, Nodes, PoolLeaves, STORED_FROM, TreeExtension, TreeNode, climb,
    par_map,
};

/// The most note commitments that one extension takes, unless one sealed range alone has more.
/// About 64 MiB of commitments.
const MAX_COMMITMENTS: u64 = 1 << 21;

/// The most blocks that one extension takes, rounded up to whole sealed ranges.
const MAX_BLOCKS: u32 = 50 * RANGE_SIZE;

type OrchardNode = orchard::tree::Node;

/// The blocks after the trees and the start of the trees, as one snapshot gives them. The sealed
/// ranges stay encoded, so that the decode runs with no transaction.
pub(crate) struct TreeRead {
    /// The last height that the trees reach in the snapshot.
    base: Option<u32>,
    /// The tree sizes at the end of `base`.
    start_sizes: TreeSizes,
    sapling: PoolStart<SaplingNode>,
    orchard: PoolStart<OrchardNode>,
    ironwood: PoolStart<OrchardNode>,
    /// Whole sealed ranges, each with the index of its first record to take.
    sealed: Vec<(Vec<u8>, usize)>,
    /// The mutable blocks after the sealed ranges.
    mutable: Vec<CompactBlockRecord>,
}

impl TreeRead {
    /// Reads the blocks after the trees in `snapshot`: about [`MAX_BLOCKS`] blocks or
    /// [`MAX_COMMITMENTS`] commitments in the sealed ranges, then the mutable blocks. Returns
    /// `None` if the trees reach the durable tip.
    pub(crate) fn read(snapshot: &IndexSnapshot<'_>) -> Result<Option<Self>, IndexError> {
        let state = snapshot.state();
        let Some(tip) = state.durable_tip() else {
            return Ok(None);
        };
        let base = snapshot.trees_through()?;
        let first = base.map_or(0, |through| through + 1);
        if first > tip.height {
            return Ok(None);
        }
        let start_sizes = base
            .map(|through| snapshot.block::<CompactBlockRecord>(through))
            .transpose()?
            .map_or_else(TreeSizes::default, |record| record.end_tree_sizes);
        let mut read = Self {
            base,
            start_sizes,
            sapling: PoolStart::read(snapshot, Pool::Sapling, base, start_sizes)?,
            orchard: PoolStart::read(snapshot, Pool::Orchard, base, start_sizes)?,
            ironwood: PoolStart::read(snapshot, Pool::Ironwood, base, start_sizes)?,
            sealed: Vec::new(),
            mutable: Vec::new(),
        };

        let last = tip.height.min(first.saturating_add(MAX_BLOCKS - 1));
        let mut height = first;
        while height <= last && state.sealed_through().is_some_and(|sealed| height <= sealed) {
            let start = range_start(height);
            let blob = snapshot.sealed_range(start)?.ok_or(IndexError::Coverage { height })?;
            let end = RangeDecoder::new(&blob)?.record(RANGE_SIZE as usize - 1)?;
            let added = end.end_tree_sizes.checked_sub(start_sizes).map(TreeSizes::total);
            let added = added.ok_or(TreeError::Sizes { height: end.height })?;
            if !read.sealed.is_empty() && added > MAX_COMMITMENTS {
                return Ok(Some(read));
            }
            read.sealed.push((blob, (height - start) as usize));
            height = start + RANGE_SIZE;
        }
        if height <= last {
            snapshot.read_blocks(height, last, |record| {
                read.mutable.push(record);
                true
            })?;
        }
        Ok(Some(read))
    }

    /// Decodes the sealed blocks on up to `threads` threads, and takes the commitments of each
    /// block in height order.
    pub(crate) fn decode(self, threads: usize) -> Result<TreeBatch, IndexError> {
        let decoders = self
            .sealed
            .iter()
            .map(|(blob, from)| Ok((RangeDecoder::new(blob)?, *from)))
            .collect::<Result<Vec<_>, IndexError>>()?;
        let offsets: Vec<usize> = decoders
            .iter()
            .scan(0, |next, (_, from)| {
                let offset = *next;
                *next += RANGE_SIZE as usize - from;
                Some(offset)
            })
            .collect();
        let records = offsets
            .last()
            .zip(decoders.last())
            .map_or(0, |(offset, (_, from))| offset + RANGE_SIZE as usize - from);
        let sealed = par_map(records, threads, |record| {
            let piece = offsets.partition_point(|&offset| offset <= record) - 1;
            let (decoder, from) = &decoders[piece];
            decoder
                .record(from + record - offsets[piece])
                .map(|record| BlockCommitments::of(&record))
        });

        let mut batch = TreeBatch {
            sizes: self.start_sizes,
            end: None,
            sapling: PoolLeaves::new(u64::from(self.start_sizes.sapling)),
            orchard: PoolLeaves::new(u64::from(self.start_sizes.orchard)),
            ironwood: PoolLeaves::new(u64::from(self.start_sizes.ironwood)),
            read: self,
        };
        for block in sealed {
            batch.push(block?)?;
        }
        let mutable = std::mem::take(&mut batch.read.mutable);
        mutable.iter().try_for_each(|record| batch.push(BlockCommitments::of(record)))?;
        Ok(batch)
    }
}

/// Where the tree of one pool ends: the low frontier, if its chunk is not full, and the stored
/// nodes that the new nodes have on their left.
struct PoolStart<H> {
    low: Option<LowFrontier<H>>,
    /// By level and index.
    left: BTreeMap<(u8, u64), H>,
}

impl<H: TreeNode> PoolStart<H> {
    /// Reads the start of the tree of `pool` at the end of `base`, whose tree sizes are `sizes`.
    /// A new node at level `l` has a stored node on its left exactly when the size has a 1 bit
    /// at `l`. Refuses a missing or wrong row.
    fn read(
        snapshot: &IndexSnapshot<'_>,
        pool: Pool,
        base: Option<u32>,
        sizes: TreeSizes,
    ) -> Result<Self, IndexError> {
        let (Some(base), size) = (base, pool_size(sizes, pool)) else {
            return Ok(Self { low: None, left: BTreeMap::new() });
        };
        let damaged = || TreeError::Row { pool: pool.as_str() };
        let low = match size.checked_sub(1) {
            Some(position) => Some(
                snapshot
                    .tree_low::<H>(pool, base)?
                    .filter(|low| low.position() == position)
                    .ok_or_else(damaged)?,
            ),
            None => None,
        };
        let left = (STORED_FROM..DEPTH)
            .filter(|level| (size >> level) & 1 == 1)
            .map(|level| {
                let index = (size >> level) - 1;
                let node = snapshot.tree_node::<H>(pool, level, index)?.ok_or_else(damaged)?;
                Ok(((level, index), node))
            })
            .collect::<Result<_, IndexError>>()?;
        Ok(Self { low: low.filter(|low| !low.ends_chunk()), left })
    }
}

/// The commitments of one block, by pool.
struct BlockCommitments {
    id: BlockId,
    end_tree_sizes: TreeSizes,
    sapling: Vec<[u8; 32]>,
    orchard: Vec<[u8; 32]>,
    ironwood: Vec<[u8; 32]>,
}

impl BlockCommitments {
    fn of(record: &CompactBlockRecord) -> Self {
        let transactions = &record.transactions;
        Self {
            id: BlockId::new(record.height, record.hash),
            end_tree_sizes: record.end_tree_sizes,
            sapling: transactions
                .iter()
                .flat_map(|tx| &tx.sapling_outputs)
                .map(|output| output.cmu)
                .collect(),
            orchard: transactions
                .iter()
                .flat_map(|tx| &tx.orchard_actions)
                .map(|action| action.commitment)
                .collect(),
            ironwood: transactions
                .iter()
                .flat_map(|tx| &tx.ironwood_actions)
                .map(|action| action.commitment)
                .collect(),
        }
    }

    /// The commitments of the block in each pool.
    fn sizes(&self) -> Option<TreeSizes> {
        Some(TreeSizes {
            sapling: self.sapling.len().try_into().ok()?,
            orchard: self.orchard.len().try_into().ok()?,
            ironwood: self.ironwood.len().try_into().ok()?,
        })
    }
}

/// The commitments that a run of blocks adds to each pool, after the trees.
pub(crate) struct TreeBatch {
    read: TreeRead,
    /// The tree sizes at the end of the blocks so far.
    sizes: TreeSizes,
    /// The last block so far.
    end: Option<BlockId>,
    sapling: PoolLeaves,
    orchard: PoolLeaves,
    ironwood: PoolLeaves,
}

impl TreeBatch {
    /// Adds the next block. Refuses a block whose commitments do not add up to its tree sizes.
    fn push(&mut self, block: BlockCommitments) -> Result<(), TreeError> {
        let height = block.id.height;
        let sizes = block.sizes().and_then(|added| self.sizes.checked_add(added));
        if sizes != Some(block.end_tree_sizes) {
            return Err(TreeError::Sizes { height });
        }
        self.sizes = block.end_tree_sizes;
        self.end = Some(block.id);
        self.sapling.push_block(height, block.sapling.into_iter());
        self.orchard.push_block(height, block.orchard.into_iter());
        self.ironwood.push_block(height, block.ironwood.into_iter());
        Ok(())
    }

    /// Hashes the chunks of each pool on up to `threads` threads, then the complete nodes above
    /// them.
    pub(crate) fn hash(self, threads: usize) -> Result<TreeRows, TreeError> {
        let Self { read, sizes, end, sapling, orchard, ironwood } = self;
        let end = end.expect("a read takes at least one block");
        Ok(TreeRows {
            base: read.base,
            end,
            extension: TreeExtension {
                start_height: read.base.map_or(0, |through| through + 1),
                end_height: end.height,
                commitments: sizes.checked_sub(read.start_sizes).map_or(0, TreeSizes::total),
            },
            sapling: extend_pool(Pool::Sapling, &sapling, read.sapling, threads)?,
            orchard: extend_pool(Pool::Orchard, &orchard, read.orchard, threads)?,
            ironwood: extend_pool(Pool::Ironwood, &ironwood, read.ironwood, threads)?,
        })
    }
}

/// The rows that one extension adds to each pool.
pub(crate) struct TreeRows {
    base: Option<u32>,
    end: BlockId,
    extension: TreeExtension,
    sapling: PoolRows<SaplingNode>,
    orchard: PoolRows<OrchardNode>,
    ironwood: PoolRows<OrchardNode>,
}

impl TreeRows {
    /// Writes the rows and moves the tree height to the last block of the extension. Fails with
    /// [`TreeError::Moved`] and writes nothing if the trees or the blocks changed after the read.
    pub(crate) fn write(
        self,
        txn: &mut RwTxn<'_>,
        tables: Tables,
    ) -> Result<TreeExtension, IndexError> {
        let end = self.end;
        if tables.metadata.get_trees_through(txn)? != self.base
            || !is_canonical(txn, tables, end.height, end.hash)?
        {
            return Err(TreeError::Moved.into());
        }
        self.sapling.put(txn, tables, Pool::Sapling)?;
        self.orchard.put(txn, tables, Pool::Orchard)?;
        self.ironwood.put(txn, tables, Pool::Ironwood)?;
        tables.metadata.put_trees_through(txn, Some(end.height))?;
        Ok(self.extension)
    }
}

/// The rows that one extension adds to one pool.
struct PoolRows<H> {
    lows: Lows<H>,
    /// The new complete nodes.
    nodes: Nodes<H>,
}

impl<H: TreeNode> PoolRows<H> {
    fn put(&self, txn: &mut RwTxn<'_>, tables: Tables, pool: Pool) -> Result<(), IndexError> {
        self.lows
            .iter()
            .try_for_each(|(height, low)| tables.tree_lows.put(txn, pool, *height, low))?;
        self.nodes.iter().try_for_each(|(level, index, node)| {
            tables.tree_nodes.put(txn, pool, *level, *index, node)
        })
    }
}

/// Hashes the chunks of `leaves` from `start`, then the complete nodes above them.
fn extend_pool<H: TreeNode>(
    pool: Pool,
    leaves: &PoolLeaves,
    start: PoolStart<H>,
    threads: usize,
) -> Result<PoolRows<H>, TreeError> {
    let rows = leaves
        .hash(start.low, threads)
        .map_err(|position| TreeError::Commitment { pool: pool.as_str(), position })?;
    let nodes = climb(rows.roots, &start.left).ok_or(TreeError::Row { pool: pool.as_str() })?;
    Ok(PoolRows { lows: rows.lows, nodes })
}

/// Returns `true` if the block at `height` has the hash `hash` in the index of `txn`.
fn is_canonical(
    txn: &RwTxn<'_>,
    tables: Tables,
    height: u32,
    hash: block::Hash,
) -> Result<bool, IndexError> {
    Ok(tables.hash_to_height.get(txn, &hash)? == Some(height))
}

#[cfg(test)]
mod tests {
    use incrementalmerkletree::frontier::Frontier;
    use ztreamer_chain::test_utils::{mixed_records, record};
    use ztreamer_snapshot::manifest_entry::Frontiers;

    use super::*;
    use crate::{
        index::Index,
        test_utils::{open_index, populate},
        trees::DEPTH,
    };

    /// The frontiers at the end of each record, as the library builds them one leaf at a time.
    fn reference(records: &[CompactBlockRecord]) -> Vec<Frontiers> {
        fn append<H: TreeNode>(frontier: &mut Frontier<H, DEPTH>, commitments: &[[u8; 32]]) {
            commitments.iter().for_each(|cm| assert!(frontier.append(H::leaf(cm).unwrap())));
        }
        let mut sapling = Frontier::<SaplingNode, DEPTH>::empty();
        let (mut orchard, mut ironwood) = (Frontier::empty(), Frontier::empty());
        records
            .iter()
            .map(|record| {
                let block = BlockCommitments::of(record);
                append(&mut sapling, &block.sapling);
                append::<OrchardNode>(&mut orchard, &block.orchard);
                append::<OrchardNode>(&mut ironwood, &block.ironwood);
                Frontiers {
                    sapling: SaplingNode::rpc_bytes(sapling.clone()),
                    orchard: OrchardNode::rpc_bytes(orchard.clone()),
                    ironwood: OrchardNode::rpc_bytes(ironwood.clone()),
                }
            })
            .collect()
    }

    /// [`mixed_records`] with the Orchard actions of each block above 600 also in Ironwood, so
    /// that each pool has commitments and Ironwood starts late.
    fn records(count: u32) -> Vec<CompactBlockRecord> {
        let mut sizes = TreeSizes::default();
        mixed_records(count)
            .into_iter()
            .map(|mut block| {
                if block.height > 600 {
                    for tx in &mut block.transactions {
                        tx.ironwood_actions = tx.orchard_actions.clone();
                    }
                }
                sizes = sizes.checked_add(block.additions()).unwrap();
                block.end_tree_sizes = sizes;
                block
            })
            .collect()
    }

    /// Extends the trees until they reach the tip. Returns the extensions.
    fn extend_all(index: &Index) -> Vec<TreeExtension> {
        std::iter::from_fn(|| index.extend_trees(4).unwrap()).collect()
    }

    /// Checks the tree state of each block of `records` against [`reference`].
    fn assert_trees(index: &Index, records: &[CompactBlockRecord]) {
        let snapshot = index.latest().unwrap();
        assert_eq!(snapshot.trees_through().unwrap(), records.last().map(|record| record.height));
        for (record, expected) in records.iter().zip(reference(records)) {
            let frontiers = snapshot.tree_frontiers(record.height, record.end_tree_sizes).unwrap();
            assert_eq!(frontiers, Some(expected), "height {}", record.height);
        }
    }

    /// `count` blocks after `ancestor` with other hashes and commitments than `records`.
    fn branch(
        records: &[CompactBlockRecord],
        ancestor: &CompactBlockRecord,
        count: u32,
    ) -> Vec<CompactBlockRecord> {
        let (mut sizes, mut previous) = (ancestor.end_tree_sizes, ancestor.hash);
        (ancestor.height + 1..=ancestor.height + count)
            .map(|height| {
                let source = &records[(height as usize * 7 + 3) % records.len()];
                let mut block = record(height, source.transactions.clone());
                block.hash.0[31] = 0xbb;
                block.previous_hash = previous;
                sizes = sizes.checked_add(block.additions()).unwrap();
                block.end_tree_sizes = sizes;
                previous = block.hash;
                block
            })
            .collect()
    }

    #[test]
    fn extensions_over_sealed_and_mutable_blocks_give_every_tree_state() {
        let dir = tempfile::tempdir().unwrap();
        let index = open_index(dir.path());
        let records = records(1_300);
        assert!(index.extend_trees(4).unwrap().is_none(), "an empty index has no blocks");

        // A first extension over mutable blocks ends inside a chunk. The next one goes on from
        // it over sealed ranges and mutable blocks.
        populate(&index, &records[..450], None);
        let first = extend_all(&index);
        assert_eq!(first.first().unwrap().start_height, 0);
        assert_eq!(first.last().unwrap().end_height, 449);
        assert_trees(&index, &records[..450]);

        populate(&index, &records[450..], Some(999));
        let second = extend_all(&index);
        assert_eq!(second.first().unwrap().start_height, 450);
        assert_eq!(second.last().unwrap().end_height, 1_299);
        let commitments: u64 = first.iter().chain(&second).map(|e| e.commitments).sum();
        let sizes = records.last().unwrap().end_tree_sizes;
        assert!(sizes.sapling > 4_096 && sizes.orchard > 4_096 && sizes.ironwood > 4_096);
        assert_eq!(commitments, records.last().unwrap().end_tree_sizes.total());
        assert_trees(&index, &records);
        index.latest().unwrap().verify_continuity().unwrap();
    }

    #[test]
    fn a_reorg_truncates_the_trees_and_they_extend_over_the_new_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let index = open_index(dir.path());
        let records = records(1_300);
        let state = populate(&index, &records, Some(999));
        extend_all(&index);

        // An ordinary reorg in the mutable blocks.
        let ancestor = &records[1_100];
        let mut canonical = records[..=1_100].to_vec();
        canonical.extend(branch(&records, ancestor, 50));
        let replacement = canonical[1_101..].to_vec();
        let id = BlockId::new(ancestor.height, ancestor.hash);
        let state =
            index.replace_mutable_suffix(state.generation(), id, replacement, Some(999)).unwrap();
        assert_eq!(index.latest().unwrap().trees_through().unwrap(), Some(1_100));
        assert_eq!(extend_all(&index).len(), 1);
        assert_trees(&index, &canonical);

        // A deep reorg into the sealed ranges.
        let ancestor = &canonical[950];
        let mut deep = canonical[..=950].to_vec();
        deep.extend(branch(&records, ancestor, 120));
        let replacement = deep[951..].to_vec();
        let id = BlockId::new(ancestor.height, ancestor.hash);
        index.replace_deep_suffix(state.generation(), id, replacement, None).unwrap();
        assert_eq!(index.latest().unwrap().trees_through().unwrap(), Some(950));
        extend_all(&index);
        assert_trees(&index, &deep);
        index.latest().unwrap().verify_continuity().unwrap();
    }

    #[test]
    fn rows_from_before_a_reorg_are_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let index = open_index(dir.path());
        let records = records(300);
        let state = populate(&index, &records, None);

        let read = TreeRead::read(&index.latest().unwrap()).unwrap().unwrap();
        let rows = read.decode(2).unwrap().hash(2).unwrap();
        let ancestor = &records[200];
        let id = BlockId::new(ancestor.height, ancestor.hash);
        index
            .replace_mutable_suffix(state.generation(), id, branch(&records, ancestor, 20), None)
            .unwrap();

        let mut txn = index.env.write_txn().unwrap();
        let moved = rows.write(&mut txn, index.tables).unwrap_err();
        assert!(matches!(moved, IndexError::Tree(TreeError::Moved)), "{moved}");
        drop(txn);
        assert_eq!(index.latest().unwrap().trees_through().unwrap(), None);
    }
}
