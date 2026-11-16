//! [`LowFrontier`]: the part of a tree frontier below the stored nodes.

use std::mem;

use incrementalmerkletree::{Level, Position, frontier::Frontier};

use super::{DEPTH, STORED_FROM, TreeNode};

/// The positions of one chunk differ only in these low bits.
const CHUNK_MASK: u64 = (1 << STORED_FROM) - 1;

/// The bytes of an encoded low frontier before its nodes: the position, a big-endian `u32`.
const POSITION_BYTES: usize = 4;

/// The part of a tree frontier below [`STORED_FROM`]: the last leaf, and the ommers of that leaf
/// below level [`STORED_FROM`]. With the stored nodes from [`STORED_FROM`] up it gives the whole
/// frontier.
///
/// All its nodes are in one chunk: the chunk of the last leaf. [`LowFrontier::append`] adds the
/// next leaf of the chunk, and [`LowFrontier::chunk_root`] gives the node of a full chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LowFrontier<H> {
    /// The position of the last leaf.
    position: u64,
    /// The last leaf.
    leaf: H,
    /// The ommer at each level below [`STORED_FROM`] where `position` has a 1 bit, lowest first.
    ommers: Vec<H>,
}

impl<H: TreeNode> LowFrontier<H> {
    /// The frontier of the first leaf of a chunk, at `position`.
    pub(crate) fn first(position: u64, leaf: H) -> Self {
        debug_assert_eq!(position & CHUNK_MASK, 0, "a chunk starts at a multiple of its size");
        Self { position, leaf, ommers: Vec::new() }
    }

    /// The position of the last leaf.
    pub(crate) const fn position(&self) -> u64 {
        self.position
    }

    /// Returns `true` if the last leaf is the last leaf of its chunk.
    pub(crate) const fn ends_chunk(&self) -> bool {
        self.position & CHUNK_MASK == CHUNK_MASK
    }

    /// Adds `leaf` at the next position. The chunk must not be full: the next position must be in
    /// the same chunk.
    pub(crate) fn append(&mut self, leaf: H) {
        debug_assert!(!self.ends_chunk(), "the next leaf starts a new chunk");
        // The trailing 1 bits of the position are the complete subtrees that the old leaf
        // completes. Their ommers are the lowest ones.
        let complete = (self.position & CHUNK_MASK).trailing_ones() as usize;
        let mut carry = mem::replace(&mut self.leaf, leaf);
        for (level, ommer) in (0..).zip(self.ommers.drain(..complete)) {
            carry = H::combine(Level::from(level), &ommer, &carry);
        }
        self.ommers.insert(0, carry);
        self.position += 1;
    }

    /// The node at level [`STORED_FROM`] of the chunk, if the last leaf ends the chunk.
    pub(crate) fn chunk_root(&self) -> Option<H> {
        self.ends_chunk().then(|| {
            (0..).zip(&self.ommers).fold(self.leaf.clone(), |carry, (level, ommer)| {
                H::combine(Level::from(level), ommer, &carry)
            })
        })
    }

    /// The whole frontier. `node` gives the stored node at a level from [`STORED_FROM`] up and an
    /// index at that level.
    pub(crate) fn frontier<E>(
        &self,
        mut node: impl FnMut(u8, u64) -> Result<H, E>,
    ) -> Result<Frontier<H, DEPTH>, E> {
        let mut ommers = self.ommers.clone();
        for level in STORED_FROM..DEPTH {
            if (self.position >> level) & 1 == 1 {
                ommers.push(node(level, (self.position >> level) - 1)?);
            }
        }
        Ok(Frontier::from_parts(Position::from(self.position), self.leaf.clone(), ommers)
            .expect("there is one ommer for each 1 bit of the position"))
    }

    /// The bytes of the low frontier: the position as a big-endian `u32`, the leaf, then each
    /// ommer, lowest first.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let position = u32::try_from(self.position).expect("a tree size fits in u32");
        let mut bytes = Vec::with_capacity(POSITION_BYTES + 32 * (1 + self.ommers.len()));
        bytes.extend_from_slice(&position.to_be_bytes());
        bytes.extend_from_slice(&self.leaf.to_bytes());
        self.ommers.iter().for_each(|ommer| bytes.extend_from_slice(&ommer.to_bytes()));
        bytes
    }

    /// Reads what [`LowFrontier::encode`] writes. Returns `None` if the length does not match the
    /// position, or if a node is not a node of the pool.
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let (position, nodes) = bytes.split_first_chunk::<POSITION_BYTES>()?;
        let position = u64::from(u32::from_be_bytes(*position));
        let (nodes, rest) = nodes.as_chunks::<32>();
        let ommers = (position & CHUNK_MASK).count_ones() as usize;
        if !rest.is_empty() || nodes.len() != 1 + ommers {
            return None;
        }
        let mut nodes = nodes.iter().map(|bytes| H::from_bytes(*bytes));
        let leaf = nodes.next()??;
        Some(Self { position, leaf, ommers: nodes.collect::<Option<_>>()? })
    }
}
