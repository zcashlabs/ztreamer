//! [`IndexSnapshot`]: the reads of an index in one LMDB read transaction.

use std::fmt;

use heed::{RoTxn, WithTls};
use incrementalmerkletree::frontier::Frontier;
use sapling_crypto::Node as SaplingNode;
use zakura_chain::{block, orchard};

use super::{
    IndexError, IndexState, RANGE_SIZE, TreeError,
    range_meta::RangeMeta,
    tables::{Tables, range_start},
    tree_tables::pool_size,
};
use crate::trees::{LowFrontier, TreeNode};
use ztreamer_chain::codec::{CompactBlockRecord, StoredBlock, TreeSizes, decode_range_record};
use ztreamer_snapshot::{manifest_entry::Frontiers, pool::Pool};

/// The reads of an [`Index`](super::Index) in one LMDB read transaction. All reads of one
/// snapshot see one committed state, also when a write commits during the reads.
///
/// [`Index::snapshot`](super::Index::snapshot) gives a snapshot at a known generation, and
/// [`Index::latest`](super::Index::latest) at the current generation. The LMDB read transaction
/// stays on the thread that opened it. Thus open a snapshot inside the blocking closure that reads
/// with it. One thread holds at most one snapshot of an index: drop a snapshot before you open
/// the next one, or the open fails.
///
/// ```no_run
/// # use ztreamer_chain::codec::CompactBlockRecord;
/// # use ztreamer_index::index::{Index, IndexError};
/// # fn demo(index: &Index) -> Result<(), IndexError> {
/// let snapshot = index.latest()?;
/// let tip = snapshot.state().durable_tip().expect("the index holds blocks");
/// let block: CompactBlockRecord = snapshot.block(tip.height)?;
/// assert_eq!(block.hash, tip.hash);
/// # Ok(())
/// # }
/// ```
pub struct IndexSnapshot<'index> {
    txn: RoTxn<'index, WithTls>,
    tables: Tables,
    state: IndexState,
}

impl fmt::Debug for IndexSnapshot<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexSnapshot").field("state", &self.state).finish_non_exhaustive()
    }
}

impl<'index> IndexSnapshot<'index> {
    /// The snapshot of `txn`, whose state is `state`.
    pub(crate) const fn new(
        txn: RoTxn<'index, WithTls>,
        tables: Tables,
        state: IndexState,
    ) -> Self {
        Self { txn, tables, state }
    }

    /// The state of the index in this snapshot.
    pub const fn state(&self) -> IndexState {
        self.state
    }

    /// Reads the block at `height`.
    pub fn block<T: StoredBlock>(&self, height: u32) -> Result<T, IndexError> {
        self.tables.read(&self.txn, self.state, height)
    }

    /// Resolves and reads one canonical block hash.
    pub fn block_by_hash<T: StoredBlock>(
        &self,
        hash: block::Hash,
    ) -> Result<Option<T>, IndexError> {
        self.tables
            .hash_to_height
            .get(&self.txn, &hash)?
            .map(|height| self.tables.read(&self.txn, self.state, height))
            .transpose()
    }

    /// Reads the blocks from `start` to `end`, both included, one at a time into `emit`. `end`
    /// below `start` reads in descending order.
    ///
    /// Returning `false` from `emit` stops cleanly, allowing a cancelled client to release the
    /// transaction without decoding the rest of the range.
    pub fn read_blocks<T: StoredBlock>(
        &self,
        start: u32,
        end: u32,
        mut emit: impl FnMut(T) -> bool,
    ) -> Result<(), IndexError> {
        let state = self.state;
        let tip = state.durable_tip.ok_or(IndexError::Coverage { height: start })?;
        if start > tip.height {
            return Err(IndexError::Coverage { height: start });
        }
        if end > tip.height {
            return Err(IndexError::Coverage { height: end });
        }

        let ascending = start <= end;
        let mut height = start;
        loop {
            if state.sealed_through.is_some_and(|sealed| height <= sealed) {
                let range = self
                    .tables
                    .sealed_ranges
                    .find(&self.txn, height)?
                    .ok_or(IndexError::Coverage { height })?;
                let chunk_end =
                    if ascending { end.min(range.end()) } else { end.max(range.start()) };
                loop {
                    if !emit(range.read(height)?) {
                        return Ok(());
                    }
                    if height == chunk_end {
                        break;
                    }
                    height = if ascending { height + 1 } else { height - 1 };
                }
            } else if !emit(self.tables.read(&self.txn, state, height)?) {
                return Ok(());
            }

            if height == end {
                return Ok(());
            }
            height = if ascending { height + 1 } else { height - 1 };
        }
    }

    /// Reads the packed blob of the sealed range that starts at `start`. The bytes are the same
    /// as in `sealed_ranges`. Returns `None` if no sealed range starts at `start`.
    pub fn sealed_range(&self, start: u32) -> Result<Option<Vec<u8>>, IndexError> {
        if start != range_start(start) {
            return Err(IndexError::RangeStart { height: start });
        }
        Ok(self.tables.sealed_ranges.get(&self.txn, start)?.map(<[u8]>::to_vec))
    }

    /// Reads the metadata of the sealed range that starts at `start`, if any was written.
    pub fn range_meta(&self, start: u32) -> Result<Option<RangeMeta>, IndexError> {
        self.tables.range_meta.get(&self.txn, start)
    }

    /// Reads the metadata rows of the sealed ranges that start at or above `start`, in height
    /// order. A sealed range with no row is not in the list.
    pub fn range_metas_from(&self, start: u32) -> Result<Vec<RangeMeta>, IndexError> {
        self.tables.range_meta.rows_from(&self.txn, start)
    }

    /// The end height of the `range_meta` rows that follow each other from range 0, also when a
    /// row is [`VerifyState::Rejected`](super::range_meta::VerifyState::Rejected). Returns `None`
    /// if range 0 has no row.
    pub fn range_meta_filled_through(&self) -> Result<Option<u32>, IndexError> {
        self.tables.range_meta.contiguous_rows_end(&self.txn)
    }

    /// The three note commitment tree frontiers at the end of the block at `height`, whose tree
    /// sizes are `sizes`, in the `zcashd` RPC encoding of `GetTreeState`. A pool with no
    /// commitments yet has the encoding of the empty tree. Returns `None` if the trees of the
    /// index do not reach `height` yet: the caller asks the node.
    ///
    /// Each pool takes one low frontier and one stored node for each 1 bit of its position from
    /// level 8 up. It hashes nothing.
    ///
    /// ```no_run
    /// # use ztreamer_chain::codec::CompactBlockRecord;
    /// # use ztreamer_index::index::{Index, IndexError};
    /// # fn demo(index: &Index, height: u32) -> Result<(), IndexError> {
    /// let snapshot = index.latest()?;
    /// let block: CompactBlockRecord = snapshot.block(height)?;
    /// if let Some(frontiers) = snapshot.tree_frontiers(height, block.end_tree_sizes)? {
    ///     assert!(!frontiers.sapling.is_empty());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn tree_frontiers(
        &self,
        height: u32,
        sizes: TreeSizes,
    ) -> Result<Option<Frontiers>, IndexError> {
        if self.trees_through()?.is_none_or(|through| height > through) {
            return Ok(None);
        }
        Ok(Some(Frontiers {
            sapling: self.tree_rpc_bytes::<SaplingNode>(Pool::Sapling, height, sizes)?,
            orchard: self.tree_rpc_bytes::<orchard::tree::Node>(Pool::Orchard, height, sizes)?,
            ironwood: self.tree_rpc_bytes::<orchard::tree::Node>(Pool::Ironwood, height, sizes)?,
        }))
    }

    /// The last height that the note commitment trees reach, or `None` if they reach no block.
    /// [`Index::extend_trees`](super::Index::extend_trees) moves it to the durable tip.
    pub fn trees_through(&self) -> Result<Option<u32>, IndexError> {
        self.tables.metadata.get_trees_through(&self.txn)
    }

    /// The stored node of `pool` at `level` and `index`, if the trees hold it.
    pub(crate) fn tree_node<H: TreeNode>(
        &self,
        pool: Pool,
        level: u8,
        index: u64,
    ) -> Result<Option<H>, IndexError> {
        self.tables.tree_nodes.get(&self.txn, pool, level, index)
    }

    /// The low frontier of `pool` at the end of `height`, if the pool has commitments then.
    pub(crate) fn tree_low<H: TreeNode>(
        &self,
        pool: Pool,
        height: u32,
    ) -> Result<Option<LowFrontier<H>>, IndexError> {
        Ok(self.tables.tree_lows.at_or_below(&self.txn, pool, height)?.map(|(_, low)| low))
    }

    /// The RPC encoding of the tree of `pool` at the end of `height`.
    fn tree_rpc_bytes<H: TreeNode>(
        &self,
        pool: Pool,
        height: u32,
        sizes: TreeSizes,
    ) -> Result<Vec<u8>, IndexError> {
        let damaged = || IndexError::from(TreeError::Row { pool: pool.as_str() });
        let Some(position) = pool_size(sizes, pool).checked_sub(1) else {
            return Ok(H::rpc_bytes(Frontier::empty()));
        };
        let low = self
            .tree_low::<H>(pool, height)?
            .filter(|low| low.position() == position)
            .ok_or_else(damaged)?;
        let frontier =
            low.frontier(|level, index| self.tree_node(pool, level, index)?.ok_or_else(damaged))?;
        Ok(H::rpc_bytes(frontier))
    }

    /// Checks sealed-range summaries and every mutable suffix row without rescanning sealed blocks.
    pub fn verify_continuity(&self) -> Result<(), IndexError> {
        let txn = &self.txn;
        let Tables {
            sealed_ranges,
            mutable_blocks,
            hash_to_height,
            range_meta,
            tree_nodes,
            tree_lows,
            ..
        } = self.tables;
        let state = self.state;
        let trees_through = self.trees_through()?;
        let Some(tip) = state.durable_tip else {
            if self.tables.has_blocks(txn)?
                || !range_meta.starts(txn)?.is_empty()
                || trees_through.is_some()
                || !tree_nodes.is_empty(txn)?
                || !tree_lows.is_empty(txn)?
            {
                return Err(IndexError::Metadata);
            }
            return Ok(());
        };
        if trees_through.is_some_and(|through| through > tip.height) {
            return Err(IndexError::Metadata);
        }
        for start in range_meta.starts(txn)? {
            if start != range_start(start) || sealed_ranges.get(txn, start)?.is_none() {
                return Err(IndexError::RangeMeta { start });
            }
        }

        let sealed_blocks = state.sealed_through.map_or(0, |height| u64::from(height) + 1);
        if sealed_ranges.len(txn)? != sealed_blocks / u64::from(RANGE_SIZE)
            || mutable_blocks.len(txn)? != u64::from(tip.height) + 1 - sealed_blocks
            || hash_to_height.len(txn)? != u64::from(tip.height) + 1
        {
            return Err(IndexError::Continuity { height: 0 });
        }

        let mut next_height = 0u64;
        let mut previous_hash = None;
        for entry in sealed_ranges.iter(txn)? {
            let (start, bytes) = entry?;
            if u64::from(start) != next_height {
                return Err(IndexError::Continuity { height: next_height as u32 });
            }
            let first = decode_range_record(bytes, 0)?;
            let last = decode_range_record(bytes, RANGE_SIZE as usize - 1)?;
            if previous_hash.is_some_and(|hash| first.previous_hash != hash) {
                return Err(IndexError::Continuity { height: start });
            }
            hash_to_height.check(txn, &first)?;
            hash_to_height.check(txn, &last)?;
            previous_hash = Some(last.hash);
            next_height += u64::from(RANGE_SIZE);
        }
        if next_height != sealed_blocks {
            return Err(IndexError::Continuity { height: next_height as u32 });
        }

        for entry in mutable_blocks.iter(txn)? {
            let (height, bytes) = entry?;
            if u64::from(height) != next_height {
                return Err(IndexError::Continuity { height: next_height as u32 });
            }
            let record = CompactBlockRecord::decode(bytes)?;
            if record.height != height
                || previous_hash.is_some_and(|hash| record.previous_hash != hash)
            {
                return Err(IndexError::Continuity { height });
            }
            hash_to_height.check(txn, &record)?;
            previous_hash = Some(record.hash);
            next_height += 1;
        }
        let missing_tip = IndexError::Continuity { height: tip.height };
        let tip_record: CompactBlockRecord = if state
            .sealed_through
            .is_some_and(|sealed| tip.height <= sealed)
        {
            sealed_ranges.find(txn, tip.height)?.ok_or(missing_tip)?.read(tip.height)?
        } else {
            CompactBlockRecord::decode(mutable_blocks.get(txn, tip.height)?.ok_or(missing_tip)?)?
        };
        if next_height != u64::from(tip.height) + 1
            || previous_hash != Some(tip.hash)
            || state.tree_sizes != tip_record.end_tree_sizes
        {
            return Err(IndexError::Continuity { height: tip.height });
        }
        Ok(())
    }
}
