//! [`TreeNode`]: the node of the note commitment tree of a pool.

use incrementalmerkletree::{Hashable, frontier::Frontier};
use sapling_crypto::Node as SaplingNode;
use zakura_chain::{orchard, sapling};
use ztreamer_snapshot::pool::{Pool, SubtreeNode};

use super::DEPTH;

/// A node of the note commitment tree of one pool. Sapling has its own node. Orchard and Ironwood
/// share the Orchard node.
///
/// # Implementing
///
/// - [`Hashable::combine`] is the Merkle hash of the pool. Its level is the level of the two
///   children, as `incrementalmerkletree` gives it.
/// - [`TreeNode::leaf`] gives the leaf of a note commitment as a compact block carries it: the
///   Sapling `cmu`, or the Orchard or Ironwood `cmx`.
/// - [`TreeNode::from_bytes`] reads what [`TreeNode::to_bytes`] writes, and refuses bytes that
///   are not a node of the pool.
/// - [`TreeNode::rpc_bytes`] is the `zcashd` RPC encoding of a tree, the encoding of
///   `GetTreeState`.
pub(crate) trait TreeNode: Hashable + Clone + Send + Sync {
    /// The leaf of `commitment`, or `None` if the bytes are not a note commitment of the pool.
    fn leaf(commitment: &[u8; 32]) -> Option<Self>;

    /// The node that `bytes` encode, or `None` if they are not a node of the pool.
    fn from_bytes(bytes: [u8; 32]) -> Option<Self>;

    /// The 32 bytes of the node.
    fn to_bytes(&self) -> [u8; 32];

    /// The `zcashd` RPC encoding of the tree that `frontier` gives.
    fn rpc_bytes(frontier: Frontier<Self, DEPTH>) -> Vec<u8>;

    /// `node` as the root of a subtree of `pool`.
    fn subtree(pool: Pool, node: Self) -> SubtreeNode;
}

impl TreeNode for SaplingNode {
    fn leaf(commitment: &[u8; 32]) -> Option<Self> {
        Option::from(sapling::tree::NoteCommitmentUpdate::from_bytes(commitment))
            .map(|cmu| Self::from_cmu(&cmu))
    }

    fn from_bytes(bytes: [u8; 32]) -> Option<Self> {
        Option::from(Self::from_bytes(bytes))
    }

    fn to_bytes(&self) -> [u8; 32] {
        Self::to_bytes(self)
    }

    fn rpc_bytes(frontier: Frontier<Self, DEPTH>) -> Vec<u8> {
        sapling::tree::NoteCommitmentTree::from_frontier(frontier).to_rpc_bytes()
    }

    fn subtree(_: Pool, node: Self) -> SubtreeNode {
        SubtreeNode::Sapling(node)
    }
}

impl TreeNode for orchard::tree::Node {
    fn leaf(commitment: &[u8; 32]) -> Option<Self> {
        Self::try_from(*commitment).ok()
    }

    fn from_bytes(bytes: [u8; 32]) -> Option<Self> {
        Self::try_from(bytes).ok()
    }

    fn to_bytes(&self) -> [u8; 32] {
        self.to_repr()
    }

    fn rpc_bytes(frontier: Frontier<Self, DEPTH>) -> Vec<u8> {
        orchard::tree::NoteCommitmentTree::from_frontier(frontier).to_rpc_bytes()
    }

    fn subtree(pool: Pool, node: Self) -> SubtreeNode {
        match pool {
            Pool::Ironwood => SubtreeNode::Ironwood(node),
            _ => SubtreeNode::Orchard(node),
        }
    }
}
