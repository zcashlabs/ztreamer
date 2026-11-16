//! [`PoolLeaves`]: the note commitments that a run of blocks adds to one pool, and the hashes
//! that turn them into stored tree rows.

use std::collections::BTreeMap;

use incrementalmerkletree::Level;

use super::{DEPTH, LowFrontier, STORED_FROM, TreeNode, par_map};

/// The note commitments that a run of blocks adds to the tree of one pool.
#[derive(Clone, Debug, Default)]
pub(crate) struct PoolLeaves {
    /// The tree size before the run.
    start: u64,
    /// The commitments in position order.
    commitments: Vec<[u8; 32]>,
    /// The height and the tree size at the end of each block of the run that adds commitments.
    ends: Vec<(u32, u64)>,
}

impl PoolLeaves {
    /// No commitments yet, after a tree of `start` leaves.
    pub(crate) const fn new(start: u64) -> Self {
        Self { start, commitments: Vec::new(), ends: Vec::new() }
    }

    /// The tree size after the commitments so far.
    pub(crate) const fn end(&self) -> u64 {
        self.start + self.commitments.len() as u64
    }

    /// Adds the commitments of the block at `height`.
    pub(crate) fn push_block(&mut self, height: u32, commitments: impl Iterator<Item = [u8; 32]>) {
        let before = self.commitments.len();
        self.commitments.extend(commitments);
        if self.commitments.len() > before {
            self.ends.push((height, self.end()));
        }
    }

    /// Hashes each chunk that the commitments touch, on up to `threads` threads. `start` is the
    /// low frontier at the end of the tree before the run, if that chunk is not full. Returns the
    /// low frontier at the end of each block that adds commitments, and the node at level
    /// [`STORED_FROM`] of each chunk that the run fills. Fails with the position of a commitment
    /// that is not a leaf of the pool.
    pub(crate) fn hash<H: TreeNode>(
        &self,
        start: Option<LowFrontier<H>>,
        threads: usize,
    ) -> Result<ChunkRows<H>, u64> {
        let Some(last) = self.end().checked_sub(1).filter(|_| !self.commitments.is_empty()) else {
            return Ok(ChunkRows::default());
        };
        let first_chunk = self.start >> STORED_FROM;
        let chunks = (last >> STORED_FROM) - first_chunk + 1;
        let parts = par_map(chunks as usize, threads, |offset| {
            let chunk = first_chunk + offset as u64;
            let carried = start.clone().filter(|low| low.position() >> STORED_FROM == chunk);
            self.hash_chunk(chunk, carried)
        });
        let mut rows = ChunkRows::default();
        for part in parts {
            let part = part?;
            rows.lows.extend(part.lows);
            rows.roots.extend(part.roots);
        }
        Ok(rows)
    }

    /// Hashes the commitments of the run in `chunk`, from the low frontier `carried` that a
    /// previous run left in the chunk.
    fn hash_chunk<H: TreeNode>(
        &self,
        chunk: u64,
        carried: Option<LowFrontier<H>>,
    ) -> Result<ChunkRows<H>, u64> {
        let first = (chunk << STORED_FROM).max(self.start);
        let end = ((chunk + 1) << STORED_FROM).min(self.end());
        // The blocks whose last commitment is in the chunk.
        let ends_from = self.ends.partition_point(|&(_, size)| size <= first);
        let ends_to = self.ends.partition_point(|&(_, size)| size <= end);
        let mut ends = self.ends[ends_from..ends_to].iter().peekable();

        let mut low = carried;
        let mut lows = Vec::new();
        for position in first..end {
            let leaf =
                H::leaf(&self.commitments[(position - self.start) as usize]).ok_or(position)?;
            match &mut low {
                Some(low) => low.append(leaf),
                None => low = Some(LowFrontier::first(position, leaf)),
            }
            if let Some(&(height, _)) = ends.next_if(|&&(_, size)| size == position + 1) {
                lows.push((height, low.clone().expect("a leaf was added")));
            }
        }
        let roots = low.and_then(|low| low.chunk_root()).map(|root| (chunk, root)).into_iter();
        Ok(ChunkRows { lows, roots: roots.collect() })
    }
}

/// The low frontier at the end of each block that adds commitments, by height.
pub(crate) type Lows<H> = Vec<(u32, LowFrontier<H>)>;

/// Complete nodes from level [`STORED_FROM`] up, as `(level, index, node)`.
pub(crate) type Nodes<H> = Vec<(u8, u64, H)>;

/// What hashing the chunks of one pool gives: the rows of the low frontiers, and the nodes at
/// level [`STORED_FROM`].
#[derive(Debug)]
pub(crate) struct ChunkRows<H> {
    /// The low frontier at the end of each block that adds commitments, by height.
    pub(crate) lows: Lows<H>,
    /// The node of each chunk that the run fills, by its index at level [`STORED_FROM`].
    pub(crate) roots: Vec<(u64, H)>,
}

impl<H> Default for ChunkRows<H> {
    fn default() -> Self {
        Self { lows: Vec::new(), roots: Vec::new() }
    }
}

/// The complete nodes above the chunk nodes `roots`, which are in index order: each node whose
/// right child is new. `stored` holds the earlier nodes that a new node has on its left, by level
/// and index. Returns every new node, `roots` too, or `None` if `stored` lacks a node.
pub(crate) fn climb<H: TreeNode>(
    roots: Vec<(u64, H)>,
    stored: &BTreeMap<(u8, u64), H>,
) -> Option<Nodes<H>> {
    let mut nodes = BTreeMap::new();
    for (index, root) in roots {
        let (mut level, mut index, mut node) = (STORED_FROM, index, root);
        nodes.insert((level, index), node.clone());
        while index & 1 == 1 && level + 1 < DEPTH {
            let left =
                nodes.get(&(level, index - 1)).or_else(|| stored.get(&(level, index - 1)))?;
            node = H::combine(Level::from(level), left, &node);
            (level, index) = (level + 1, index / 2);
            nodes.insert((level, index), node.clone());
        }
    }
    Some(nodes.into_iter().map(|((level, index), node)| (level, index, node)).collect())
}
