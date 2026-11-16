//! [`TreeRun`]: the note commitment trees of a run of blocks, built in memory from where the trees
//! end before the run. [`Index::extend_trees`](super::Index::extend_trees) starts a run from the
//! trees of the index, and a snapshot import from the frontiers of its manifest.

use std::{collections::BTreeMap, ops::RangeInclusive};

use heed::RwTxn;
use incrementalmerkletree::{Level, frontier::Frontier};
use sapling_crypto::Node as SaplingNode;
use zakura_chain::{
    orchard,
    subtree::{NoteCommitmentSubtreeIndex, TRACKED_SUBTREE_HEIGHT},
};
use ztreamer_chain::codec::{CompactBlockRecord, TreeSizes};
use ztreamer_snapshot::pool::{Pool, SubtreeRoot};

use super::{
    BlockId, IndexError, IndexSnapshot, TreeError, TreeRows, tables::Tables, tree_tables::pool_size,
};
use crate::trees::{DEPTH, LowFrontier, PoolLeaves, STORED_FROM, TreeExtension, TreeNode, climb};

pub(crate) type OrchardNode = orchard::tree::Node;

/// The note commitment trees of a run of blocks after a known end of the trees.
///
/// [`TreeRun::push`] takes the commitments of each block, and [`TreeRun::hash`] hashes them into
/// the [`TreeRows`] of the run, on up to the given threads.
///
/// ```no_run
/// # use incrementalmerkletree::frontier::Frontier;
/// # use ztreamer_chain::codec::{CompactBlockRecord, TreeSizes};
/// # use ztreamer_index::index::{StartFrontiers, TreeError, TreeRun};
/// # fn demo(blocks: &[CompactBlockRecord]) -> Result<(), TreeError> {
/// let genesis = StartFrontiers {
///     sapling: Frontier::empty(),
///     orchard: Frontier::empty(),
///     ironwood: Frontier::empty(),
/// };
/// let mut run = TreeRun::from_frontiers(None, TreeSizes::default(), genesis).unwrap();
/// blocks.iter().try_for_each(|block| run.push(block))?;
/// let rows = run.hash(4)?;
/// # Ok(())
/// # }
/// ```
pub struct TreeRun {
    /// The last block before the run, or `None` if the run starts at genesis.
    base: Option<u32>,
    start_sizes: TreeSizes,
    /// The tree sizes at the end of the blocks so far.
    sizes: TreeSizes,
    /// The last block so far.
    end: Option<BlockId>,
    sapling: PoolRun<SaplingNode>,
    orchard: PoolRun<OrchardNode>,
    ironwood: PoolRun<OrchardNode>,
}

impl std::fmt::Debug for TreeRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeRun")
            .field("base", &self.base)
            .field("sizes", &self.sizes)
            .field("end", &self.end)
            .finish_non_exhaustive()
    }
}

impl TreeRun {
    /// A run after the block `base` whose tree sizes are `sizes`, with the frontier of each pool
    /// at its end. Returns `None` if a frontier does not have the size of its pool in `sizes`.
    pub fn from_frontiers(
        base: Option<u32>,
        sizes: TreeSizes,
        frontiers: StartFrontiers,
    ) -> Option<Self> {
        let size = |position: Option<u64>| position.map_or(0, |position| position + 1);
        let StartFrontiers { sapling, orchard, ironwood } = frontiers;
        let matches = size(sapling.value().map(|f| f.position().into()))
            == u64::from(sizes.sapling)
            && size(orchard.value().map(|f| f.position().into())) == u64::from(sizes.orchard)
            && size(ironwood.value().map(|f| f.position().into())) == u64::from(sizes.ironwood);
        matches.then(|| {
            let (sapling, orchard) =
                (PoolStart::from_frontier(sapling), PoolStart::from_frontier(orchard));
            Self::new(base, sizes, sapling, orchard, PoolStart::from_frontier(ironwood))
        })
    }

    /// A run after the block `base` whose tree sizes are `sizes`, where the trees of the index
    /// end.
    pub(crate) fn from_index(
        snapshot: &IndexSnapshot<'_>,
        base: Option<u32>,
        sizes: TreeSizes,
    ) -> Result<Self, IndexError> {
        Ok(Self::new(
            base,
            sizes,
            PoolStart::read(snapshot, Pool::Sapling, base, sizes)?,
            PoolStart::read(snapshot, Pool::Orchard, base, sizes)?,
            PoolStart::read(snapshot, Pool::Ironwood, base, sizes)?,
        ))
    }

    fn new(
        base: Option<u32>,
        sizes: TreeSizes,
        sapling: PoolStart<SaplingNode>,
        orchard: PoolStart<OrchardNode>,
        ironwood: PoolStart<OrchardNode>,
    ) -> Self {
        Self {
            base,
            start_sizes: sizes,
            sizes,
            end: None,
            sapling: PoolRun::new(sapling, sizes.sapling),
            orchard: PoolRun::new(orchard, sizes.orchard),
            ironwood: PoolRun::new(ironwood, sizes.ironwood),
        }
    }

    /// Adds the next block. Refuses a block whose commitments do not add up to its tree sizes.
    pub fn push(&mut self, block: &CompactBlockRecord) -> Result<(), TreeError> {
        let height = block.height;
        if self.sizes.checked_add(block.additions()) != Some(block.end_tree_sizes) {
            return Err(TreeError::Sizes { height });
        }
        let transactions = &block.transactions;
        let sapling = transactions.iter().flat_map(|tx| &tx.sapling_outputs).map(|o| o.cmu);
        let orchard = transactions.iter().flat_map(|tx| &tx.orchard_actions);
        let ironwood = transactions.iter().flat_map(|tx| &tx.ironwood_actions);
        self.sapling.leaves.push_block(height, sapling);
        self.orchard.leaves.push_block(height, orchard.map(|action| action.commitment));
        self.ironwood.leaves.push_block(height, ironwood.map(|action| action.commitment));
        self.sizes = block.end_tree_sizes;
        self.end = Some(BlockId::new(height, block.hash));
        Ok(())
    }

    /// Hashes the chunks of each pool on up to `threads` threads, then the complete nodes above
    /// them. Fails with [`TreeError::Commitment`] at the block of a commitment that is not a leaf
    /// of its pool. A run needs at least one block.
    pub fn hash(self, threads: usize) -> Result<TreeRows, TreeError> {
        let end = self.end.expect("a run has at least one block");
        let extension = TreeExtension {
            start_height: self.base.map_or(0, |through| through + 1),
            end_height: end.height,
            commitments: self.sizes.checked_sub(self.start_sizes).map_or(0, TreeSizes::total),
        };
        Ok(TreeRows {
            base: self.base,
            end,
            extension,
            sapling: self.sapling.hash(Pool::Sapling, threads)?,
            orchard: self.orchard.hash(Pool::Orchard, threads)?,
            ironwood: self.ironwood.hash(Pool::Ironwood, threads)?,
        })
    }
}

/// The frontier of each pool where the trees end before a run.
#[derive(Clone, Debug)]
pub struct StartFrontiers {
    /// The Sapling frontier.
    pub sapling: Frontier<SaplingNode, DEPTH>,
    /// The Orchard frontier.
    pub orchard: Frontier<OrchardNode, DEPTH>,
    /// The Ironwood frontier.
    pub ironwood: Frontier<OrchardNode, DEPTH>,
}

/// One pool of a run: where its tree starts, and the commitments of the run.
struct PoolRun<H> {
    start: PoolStart<H>,
    leaves: PoolLeaves,
}

impl<H: TreeNode> PoolRun<H> {
    /// A pool with `size` commitments before the run, which ends at `start`.
    fn new(start: PoolStart<H>, size: u32) -> Self {
        Self { start, leaves: PoolLeaves::new(u64::from(size)) }
    }

    /// Hashes the chunks of the run from its start, then the complete nodes above them.
    fn hash(self, pool: Pool, threads: usize) -> Result<PoolHashed<H>, TreeError> {
        let Self { start, leaves } = self;
        let rows = leaves.hash(start.low.clone(), threads).map_err(|position| {
            TreeError::Commitment { pool: pool.as_str(), height: leaves.height_of(position) }
        })?;
        let nodes = climb(rows.roots, &start.left).ok_or(TreeError::Row { pool: pool.as_str() })?;
        Ok(PoolHashed { lows: rows.lows, nodes, ends: leaves.into_ends(), start })
    }
}

/// What hashing one pool of a run gives, and what [`TreeRows`] needs to read frontiers from it.
pub(crate) struct PoolHashed<H> {
    pub(crate) lows: Vec<(u32, LowFrontier<H>)>,
    /// The new complete nodes, as `(level, index, node)`, in that order.
    pub(crate) nodes: Vec<(u8, u64, H)>,
    /// The height and the tree size at the end of each block of the run that adds commitments.
    pub(crate) ends: Vec<(u32, u64)>,
    pub(crate) start: PoolStart<H>,
}

impl<H: TreeNode> PoolHashed<H> {
    /// The RPC encoding of the tree at the end of the block at `height`, or `None` if the run
    /// adds no commitment through `height` and does not know its start frontier.
    pub(crate) fn rpc_bytes(&self, height: u32) -> Option<Vec<u8>> {
        let at = self.lows.partition_point(|(low_height, _)| *low_height <= height);
        let Some((_, low)) = at.checked_sub(1).map(|at| &self.lows[at]) else {
            return self.start.frontier.clone().map(H::rpc_bytes);
        };
        low.frontier(|level, index| self.node(level, index).ok_or(())).ok().map(H::rpc_bytes)
    }

    /// The new node, or the earlier node on the left of the new ones, at `level` and `index`.
    fn node(&self, level: u8, index: u64) -> Option<H> {
        let new = self.nodes.binary_search_by_key(&(level, index), |(l, i, _)| (*l, *i));
        new.ok()
            .map(|at| self.nodes[at].2.clone())
            .or_else(|| self.start.left.get(&(level, index)).cloned())
    }

    /// The subtrees of `pool` that the blocks at `heights` complete, each with its block.
    pub(crate) fn subtrees(
        &self,
        pool: Pool,
        heights: &RangeInclusive<u32>,
    ) -> Vec<(u32, SubtreeRoot)> {
        self.nodes
            .iter()
            .filter(|(level, ..)| *level == TRACKED_SUBTREE_HEIGHT)
            .filter_map(|(_, index, node)| {
                let last = ((index + 1) << TRACKED_SUBTREE_HEIGHT) - 1;
                let height = self.ends[self.ends.partition_point(|&(_, size)| size <= last)].0;
                let index = NoteCommitmentSubtreeIndex(u16::try_from(*index).ok()?);
                heights
                    .contains(&height)
                    .then(|| (height, SubtreeRoot { index, root: H::subtree(pool, node.clone()) }))
            })
            .collect()
    }

    /// Writes the rows of the pool.
    pub(crate) fn put(
        &self,
        txn: &mut RwTxn<'_>,
        tables: Tables,
        pool: Pool,
    ) -> Result<(), IndexError> {
        self.lows
            .iter()
            .try_for_each(|(height, low)| tables.tree_lows.put(txn, pool, *height, low))?;
        self.nodes.iter().try_for_each(|(level, index, node)| {
            tables.tree_nodes.put(txn, pool, *level, *index, node)
        })
    }
}

/// Where the tree of one pool ends before a run: the low frontier, if its chunk is not full, the
/// stored nodes that the new nodes have on their left, and the whole frontier if it is known.
pub(crate) struct PoolStart<H> {
    pub(crate) low: Option<LowFrontier<H>>,
    /// By level and index.
    pub(crate) left: BTreeMap<(u8, u64), H>,
    pub(crate) frontier: Option<Frontier<H, DEPTH>>,
}

impl<H: TreeNode> PoolStart<H> {
    /// The start that `frontier` gives. A new node at level `l` has an earlier node on its left
    /// exactly when the tree size has a 1 bit at `l`. That node is the ommer of the last leaf at
    /// `l`, or, when the last leaf ends it, the leaf hashed with its ommers below `l`.
    fn from_frontier(frontier: Frontier<H, DEPTH>) -> Self {
        let Some(last) = frontier.value() else {
            return Self { low: None, left: BTreeMap::new(), frontier: Some(frontier) };
        };
        let (position, leaf, ommers) = (u64::from(last.position()), last.leaf(), last.ommers());
        let below = |level: u8| (position & ((1 << level) - 1)).count_ones() as usize;
        let size = position + 1;
        let left = (STORED_FROM..DEPTH)
            .filter(|level| (size >> level) & 1 == 1)
            .map(|level| {
                let node = if (position >> level) & 1 == 1 {
                    ommers[below(level)].clone()
                } else {
                    (0..)
                        .zip(&ommers[..usize::from(level)])
                        .fold(leaf.clone(), |node, (at, ommer)| {
                            H::combine(Level::from(at), ommer, &node)
                        })
                };
                ((level, (size >> level) - 1), node)
            })
            .collect();
        let low =
            LowFrontier::from_parts(position, leaf.clone(), ommers[..below(STORED_FROM)].to_vec());
        Self { low: (!low.ends_chunk()).then_some(low), left, frontier: Some(frontier) }
    }

    /// Reads where the tree of `pool` ends in the index, at the end of `base`, whose tree sizes
    /// are `sizes`. Refuses a missing or wrong row.
    fn read(
        snapshot: &IndexSnapshot<'_>,
        pool: Pool,
        base: Option<u32>,
        sizes: TreeSizes,
    ) -> Result<Self, IndexError> {
        let (Some(base), size) = (base, pool_size(sizes, pool)) else {
            return Ok(Self {
                low: None,
                left: BTreeMap::new(),
                frontier: Some(Frontier::empty()),
            });
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
        Ok(Self { low: low.filter(|low| !low.ends_chunk()), left, frontier: None })
    }
}
