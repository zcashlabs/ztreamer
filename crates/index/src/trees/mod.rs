//! The note commitment trees of the index. For each pool it stores every complete node from
//! level [`STORED_FROM`] up, and for each block that adds commitments the low frontier: the last
//! leaf and its ommers below that level. A tree state is then a few reads and no hash.
//! [`Index::extend_trees`](crate::index::Index::extend_trees) builds the trees after the blocks,
//! hashing chunks of 256 commitments in parallel.

mod extend;
mod low;
mod node;
mod parallel;

pub(crate) use extend::{PoolLeaves, climb};
pub(crate) use low::LowFrontier;
pub(crate) use node::TreeNode;
pub(crate) use parallel::par_map;

/// The depth of each note commitment tree.
pub(crate) const DEPTH: u8 = 32;

/// The lowest level of the stored nodes. A chunk of `2^STORED_FROM` commitments has one node at
/// this level.
pub const STORED_FROM: u8 = 8;

/// One extension of the trees of an index: the blocks that it covered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeExtension {
    /// The first block that the extension covered.
    pub start_height: u32,
    /// The last block that the extension covered. The trees now reach it.
    pub end_height: u32,
    /// The note commitments that the blocks added, in the three pools.
    pub commitments: u64,
}
