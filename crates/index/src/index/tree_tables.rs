//! The tables of the note commitment trees. The pool byte is 0 for Sapling, 1 for Orchard and 2
//! for Ironwood. All integers are big-endian.
//!
//! ```text
//! table       key                                  value
//! tree_nodes  pool (u8) | level (u8) | index (u32)  a complete node from level 8 up, 32 bytes
//! tree_lows   pool (u8) | height (u32)              the low frontier after a block that adds
//!                                                    commitments to the pool
//! ```

use std::ops::Bound;

use heed::{Database, RoTxn, RwTxn, types::Bytes};
use ztreamer_chain::codec::TreeSizes;
use ztreamer_snapshot::pool::Pool;

use super::{IndexError, TreeError};
use crate::trees::{DEPTH, LowFrontier, STORED_FROM, TreeNode};

/// The complete nodes of each tree from level [`STORED_FROM`] up.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TreeNodes(Database<Bytes, Bytes>);

impl TreeNodes {
    pub(crate) const fn new(db: Database<Bytes, Bytes>) -> Self {
        Self(db)
    }

    /// The node of `pool` at `level` and `index`, if it is stored.
    pub(crate) fn get<H: TreeNode>(
        &self,
        txn: &RoTxn<'_>,
        pool: Pool,
        level: u8,
        index: u64,
    ) -> Result<Option<H>, IndexError> {
        let Some(bytes) = self.0.get(txn, &node_key(pool, level, index))? else {
            return Ok(None);
        };
        let node = bytes.try_into().ok().and_then(H::from_bytes);
        Ok(Some(node.ok_or(TreeError::Row { pool: pool.as_str() })?))
    }

    pub(crate) fn put<H: TreeNode>(
        &self,
        txn: &mut RwTxn<'_>,
        pool: Pool,
        level: u8,
        index: u64,
        node: &H,
    ) -> Result<(), IndexError> {
        Ok(self.0.put(txn, &node_key(pool, level, index), &node.to_bytes())?)
    }

    /// Deletes each node of `pool` that is not complete in a tree of `size` leaves.
    pub(crate) fn delete_beyond(
        &self,
        txn: &mut RwTxn<'_>,
        pool: Pool,
        size: u64,
    ) -> Result<(), IndexError> {
        for level in STORED_FROM..DEPTH {
            let (from, to) =
                (node_key(pool, level, size >> level), node_key(pool, level, u64::MAX));
            self.0.delete_range(txn, &(Bound::Included(&from[..]), Bound::Included(&to[..])))?;
        }
        Ok(())
    }

    pub(crate) fn is_empty(&self, txn: &RoTxn<'_>) -> Result<bool, IndexError> {
        Ok(self.0.is_empty(txn)?)
    }
}

/// The low frontier of each pool at the end of each block that adds commitments to it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TreeLows(Database<Bytes, Bytes>);

impl TreeLows {
    pub(crate) const fn new(db: Database<Bytes, Bytes>) -> Self {
        Self(db)
    }

    /// The row of `pool` at the highest height at or below `height`, with that height. It is the
    /// low frontier at the end of `height`.
    pub(crate) fn at_or_below<H: TreeNode>(
        &self,
        txn: &RoTxn<'_>,
        pool: Pool,
        height: u32,
    ) -> Result<Option<(u32, LowFrontier<H>)>, IndexError> {
        let (from, to) = (low_key(pool, 0), low_key(pool, height));
        let mut rows =
            self.0.rev_range(txn, &(Bound::Included(&from[..]), Bound::Included(&to[..])))?;
        let Some((key, bytes)) = rows.next().transpose()? else {
            return Ok(None);
        };
        let row = key
            .get(1..)
            .and_then(|height| height.try_into().ok())
            .map(u32::from_be_bytes)
            .zip(LowFrontier::decode(bytes));
        Ok(Some(row.ok_or(TreeError::Row { pool: pool.as_str() })?))
    }

    pub(crate) fn put<H: TreeNode>(
        &self,
        txn: &mut RwTxn<'_>,
        pool: Pool,
        height: u32,
        low: &LowFrontier<H>,
    ) -> Result<(), IndexError> {
        Ok(self.0.put(txn, &low_key(pool, height), &low.encode())?)
    }

    /// Deletes the rows of `pool` above `height`.
    pub(crate) fn delete_above(
        &self,
        txn: &mut RwTxn<'_>,
        pool: Pool,
        height: u32,
    ) -> Result<(), IndexError> {
        let (from, to) = (low_key(pool, height), low_key(pool, u32::MAX));
        self.0.delete_range(txn, &(Bound::Excluded(&from[..]), Bound::Included(&to[..])))?;
        Ok(())
    }

    pub(crate) fn is_empty(&self, txn: &RoTxn<'_>) -> Result<bool, IndexError> {
        Ok(self.0.is_empty(txn)?)
    }
}

/// The tree size of `pool` in `sizes`.
pub(crate) fn pool_size(sizes: TreeSizes, pool: Pool) -> u64 {
    u64::from(match pool {
        Pool::Sapling => sizes.sapling,
        Pool::Orchard => sizes.orchard,
        Pool::Ironwood => sizes.ironwood,
    })
}

const fn pool_byte(pool: Pool) -> u8 {
    match pool {
        Pool::Sapling => 0,
        Pool::Orchard => 1,
        Pool::Ironwood => 2,
    }
}

/// The key of a node. An index above `u32::MAX` saturates: no tree has that many nodes at a
/// stored level, so it only bounds a range.
fn node_key(pool: Pool, level: u8, index: u64) -> [u8; 6] {
    let index = u32::try_from(index).unwrap_or(u32::MAX).to_be_bytes();
    [pool_byte(pool), level, index[0], index[1], index[2], index[3]]
}

const fn low_key(pool: Pool, height: u32) -> [u8; 5] {
    let height = height.to_be_bytes();
    [pool_byte(pool), height[0], height[1], height[2], height[3]]
}
