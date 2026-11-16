//! The extension of the note commitment trees over the blocks after them. [`TreeRead`] reads
//! the blocks and the end of the trees in one snapshot, a [`TreeRun`] hashes them, and
//! [`TreeRows`] holds the rows that one write adds. Only the read and the write hold an LMDB
//! transaction. The decode and the hashes run on scoped threads between them.

use std::ops::RangeInclusive;

use heed::RwTxn;
use sapling_crypto::Node as SaplingNode;
use zakura_chain::block;
use ztreamer_chain::codec::{CompactBlockRecord, RangeDecoder, TreeSizes};
use ztreamer_snapshot::{
    manifest_entry::Frontiers,
    pool::{Pool, SubtreeRoot},
};

use super::{
    BlockId, IndexError, IndexSnapshot, RANGE_SIZE, TreeError,
    tables::{Tables, range_start},
    tree_run::{OrchardNode, PoolHashed, TreeRun},
};
use crate::trees::{TreeExtension, par_map};

/// The most note commitments that one extension takes, unless one sealed range alone has more.
/// About 64 MiB of commitments.
const MAX_COMMITMENTS: u64 = 1 << 21;

/// The most blocks that one extension takes, rounded up to whole sealed ranges.
const MAX_BLOCKS: u32 = 50 * RANGE_SIZE;

/// The blocks after the trees of the index, and the run that starts where the trees end, as one
/// snapshot gives them. The sealed ranges stay encoded, so that the decode runs with no
/// transaction.
pub(crate) struct TreeRead {
    run: TreeRun,
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
        let run = TreeRun::from_index(snapshot, base, start_sizes)?;
        let mut read = Self { run, sealed: Vec::new(), mutable: Vec::new() };

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

    /// Decodes the blocks, each sealed range on up to `threads` threads, and adds them to the
    /// run in height order.
    pub(crate) fn decode(self, threads: usize) -> Result<TreeRun, IndexError> {
        let Self { mut run, sealed, mutable } = self;
        for (blob, from) in &sealed {
            let decoder = RangeDecoder::new(blob)?;
            let records =
                par_map(RANGE_SIZE as usize - from, threads, |at| decoder.record(from + at));
            for record in records {
                run.push(&record?)?;
            }
        }
        mutable.iter().try_for_each(|record| run.push(record))?;
        Ok(run)
    }
}

/// The rows that a [`TreeRun`] adds to the trees of each pool, and what they give: the frontiers
/// at the end of each block of the run, and the subtrees that it completes.
/// [`Index::write_tree_rows`](super::Index::write_tree_rows) writes them.
pub struct TreeRows {
    /// The last block before the run, where the trees of the index must end.
    pub(crate) base: Option<u32>,
    pub(crate) end: BlockId,
    pub(crate) extension: TreeExtension,
    pub(crate) sapling: PoolHashed<SaplingNode>,
    pub(crate) orchard: PoolHashed<OrchardNode>,
    pub(crate) ironwood: PoolHashed<OrchardNode>,
}

impl std::fmt::Debug for TreeRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeRows").field("extension", &self.extension).finish_non_exhaustive()
    }
}

impl TreeRows {
    /// The blocks that the rows cover.
    pub const fn extension(&self) -> TreeExtension {
        self.extension
    }

    /// The frontier of each pool at the end of the block at `height` of the run, in the RPC
    /// encoding of `GetTreeState`: a pool with no commitments has the encoding of the empty tree.
    /// Returns `None` for a pool that the run did not start from a frontier and that has no
    /// commitments in the run through `height`.
    pub fn frontiers(&self, height: u32) -> Option<Frontiers> {
        Some(Frontiers {
            sapling: self.sapling.rpc_bytes(height)?,
            orchard: self.orchard.rpc_bytes(height)?,
            ironwood: self.ironwood.rpc_bytes(height)?,
        })
    }

    /// The subtrees that the blocks at `heights` complete, in the order of their blocks, and in
    /// one block Sapling, then Orchard, then Ironwood.
    pub fn subtree_roots(&self, heights: RangeInclusive<u32>) -> Vec<SubtreeRoot> {
        let mut roots = self.sapling.subtrees(Pool::Sapling, &heights);
        roots.extend(self.orchard.subtrees(Pool::Orchard, &heights));
        roots.extend(self.ironwood.subtrees(Pool::Ironwood, &heights));
        // A stable sort keeps the pool order within a block.
        roots.sort_by_key(|(height, _)| *height);
        roots.into_iter().map(|(_, root)| root).collect()
    }

    /// Writes the rows and moves the tree height to the last block of the run. Fails with
    /// [`TreeError::Moved`] and writes nothing if the trees do not end at the start of the run,
    /// or if the last block of the run is not in the index.
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
    use ztreamer_chain::{
        test_utils::{mixed_records, record},
        types::{CompactShieldedAction, CompactTransaction},
    };
    use ztreamer_snapshot::manifest_entry::Frontiers;

    use super::*;
    use crate::{
        index::Index,
        test_utils::{open_index, populate},
        trees::{DEPTH, TreeNode},
    };

    /// The frontiers at the end of each record, as the library builds them one leaf at a time.
    fn reference(records: &[CompactBlockRecord]) -> Vec<Frontiers> {
        fn append<H: TreeNode>(frontier: &mut Frontier<H, DEPTH>, commitments: &[[u8; 32]]) {
            commitments.iter().for_each(|cm| assert!(frontier.append(H::leaf(cm).unwrap())));
        }
        let commitments = |record: &CompactBlockRecord| {
            let txs = &record.transactions;
            let actions = |pool: fn(&CompactTransaction) -> &Vec<CompactShieldedAction>| {
                txs.iter().flat_map(pool).map(|action| action.commitment).collect::<Vec<_>>()
            };
            let sapling = txs.iter().flat_map(|tx| &tx.sapling_outputs).map(|o| o.cmu).collect();
            (sapling, actions(|tx| &tx.orchard_actions), actions(|tx| &tx.ironwood_actions))
        };
        let mut sapling = Frontier::<SaplingNode, DEPTH>::empty();
        let (mut orchard, mut ironwood) = (Frontier::empty(), Frontier::empty());
        records
            .iter()
            .map(|record| {
                let (block_sapling, block_orchard, block_ironwood): (Vec<_>, Vec<_>, Vec<_>) =
                    commitments(record);
                append(&mut sapling, &block_sapling);
                append::<OrchardNode>(&mut orchard, &block_orchard);
                append::<OrchardNode>(&mut ironwood, &block_ironwood);
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
