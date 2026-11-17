//! [`NodeSubtrees`]: a [`StateBackend`] whose node has completed note commitment subtrees.

use std::{
    collections::{BTreeMap, HashSet},
    ops::RangeInclusive,
    sync::{Arc, Mutex},
};

use sapling_crypto::Node as SaplingNode;
use zakura_chain::{
    amount::{Amount, NonNegative},
    block, ironwood, orchard,
    parameters::Network,
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction::{self, Transaction, UnminedTx},
    transparent,
};
use ztreamer_backend::{
    AddressUtxo, BackendError, BlockHandle, MinedTransaction, StandardFee, StateBackend,
    Submission, Subtrees, TreeState,
};
use ztreamer_snapshot::pool::Pool;

use super::EmptyChain;

/// A [`StateBackend`] whose node has completed note commitment subtrees in each pool. It answers
/// the subtree requests as `z_getsubtreesbyindex` does: the subtrees from the start index on, at
/// most the limit of them. It answers every other request as [`EmptyChain`] does.
///
/// Clones share the subtrees and the count of subtree requests. Thus a test keeps a clone, and
/// changes the subtrees of the node after the service took the backend.
///
/// ```
/// # use ztreamer_service::test_utils::NodeSubtrees;
/// # use ztreamer_snapshot::pool::Pool;
/// let node = NodeSubtrees::default();
/// node.complete(Pool::Sapling, 1_000);
/// node.complete(Pool::Sapling, 1_200);
/// assert_eq!(node.count(Pool::Sapling), 2);
/// ```
#[derive(Clone, Debug, Default)]
pub struct NodeSubtrees(Arc<Mutex<PoolSubtrees>>);

impl NodeSubtrees {
    /// Completes the next subtree of `pool` in the block at `end_height`. Its root is
    /// [`NodeSubtrees::root`] of the pool and its index.
    pub fn complete(&self, pool: Pool, end_height: u32) {
        let mut subtrees = self.0.lock().unwrap();
        let ends = subtrees.ends_mut(pool);
        assert!(ends.len() <= usize::from(u16::MAX), "a pool has at most 65,536 subtrees");
        ends.push(end_height);
    }

    /// Drops the subtrees of `pool` from index `len` on, as a reorg below their blocks does.
    pub fn truncate(&self, pool: Pool, len: usize) {
        self.0.lock().unwrap().ends_mut(pool).truncate(len);
    }

    /// The number of completed subtrees of `pool`.
    pub fn count(&self, pool: Pool) -> usize {
        self.0.lock().unwrap().ends_mut(pool).len()
    }

    /// The number of subtree requests that the node answered, in all pools.
    pub fn requests(&self) -> usize {
        self.0.lock().unwrap().requests
    }

    /// The root of subtree `index` of `pool`, in the byte order of the pool: the little-endian
    /// field element `index + 2^16 * tag`, where the tag is 1 for Sapling, 2 for Orchard and 3
    /// for Ironwood. Each root is thus a canonical node of its pool.
    pub fn root(pool: Pool, index: u16) -> [u8; 32] {
        let tag = match pool {
            Pool::Sapling => 1,
            Pool::Orchard => 2,
            Pool::Ironwood => 3,
        };
        let mut root = [0; 32];
        root[..2].copy_from_slice(&index.to_le_bytes());
        root[2] = tag;
        root
    }

    fn subtrees<Node>(
        &self,
        pool: Pool,
        start: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
        node: impl Fn([u8; 32]) -> Node,
    ) -> Subtrees<Node> {
        let mut subtrees = self.0.lock().unwrap();
        subtrees.requests += 1;
        let end = limit.map_or(usize::MAX, |limit| usize::from(start.0) + usize::from(limit.0));
        subtrees
            .ends_mut(pool)
            .iter()
            .enumerate()
            .take(end)
            .skip(usize::from(start.0))
            .map(|(index, &end_height)| {
                let index = u16::try_from(index).expect("complete checked the index");
                let root = node(Self::root(pool, index));
                (index.into(), NoteCommitmentSubtreeData::new(block::Height(end_height), root))
            })
            .collect::<BTreeMap<_, _>>()
    }
}

/// The end heights of the completed subtrees of each pool, by index, and the count of subtree
/// requests.
#[derive(Debug, Default)]
struct PoolSubtrees {
    sapling: Vec<u32>,
    orchard: Vec<u32>,
    ironwood: Vec<u32>,
    requests: usize,
}

impl PoolSubtrees {
    const fn ends_mut(&mut self, pool: Pool) -> &mut Vec<u32> {
        match pool {
            Pool::Sapling => &mut self.sapling,
            Pool::Orchard => &mut self.orchard,
            Pool::Ironwood => &mut self.ironwood,
        }
    }
}

#[tonic::async_trait]
impl StateBackend for NodeSubtrees {
    fn network(&self) -> Network {
        EmptyChain.network()
    }

    async fn tip_height(&self) -> Result<Option<block::Height>, BackendError> {
        EmptyChain.tip_height().await
    }

    async fn block(&self, hash: block::Hash) -> Result<Option<BlockHandle>, BackendError> {
        EmptyChain.block(hash).await
    }

    async fn block_at_height(
        &self,
        height: block::Height,
    ) -> Result<Option<BlockHandle>, BackendError> {
        EmptyChain.block_at_height(height).await
    }

    async fn transaction(
        &self,
        hash: transaction::Hash,
    ) -> Result<Option<MinedTransaction>, BackendError> {
        EmptyChain.transaction(hash).await
    }

    async fn tree_state(&self, hash: block::Hash) -> Result<TreeState, BackendError> {
        EmptyChain.tree_state(hash).await
    }

    async fn sapling_subtrees(
        &self,
        start: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> Result<Subtrees<SaplingNode>, BackendError> {
        Ok(self.subtrees(Pool::Sapling, start, limit, |root| {
            SaplingNode::from_bytes(root).expect("the root is canonical")
        }))
    }

    async fn orchard_subtrees(
        &self,
        start: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> Result<Subtrees<orchard::tree::Node>, BackendError> {
        Ok(self.subtrees(Pool::Orchard, start, limit, |root| {
            orchard::tree::Node::try_from(root).expect("the root is canonical")
        }))
    }

    async fn ironwood_subtrees(
        &self,
        start: NoteCommitmentSubtreeIndex,
        limit: Option<NoteCommitmentSubtreeIndex>,
    ) -> Result<Subtrees<ironwood::tree::Node>, BackendError> {
        Ok(self.subtrees(Pool::Ironwood, start, limit, |root| {
            ironwood::tree::Node::try_from(root).expect("the root is canonical")
        }))
    }

    async fn address_transaction_ids(
        &self,
        address: transparent::Address,
        heights: RangeInclusive<block::Height>,
    ) -> Result<Vec<transaction::Hash>, BackendError> {
        EmptyChain.address_transaction_ids(address, heights).await
    }

    async fn address_balance(
        &self,
        addresses: HashSet<transparent::Address>,
    ) -> Result<Amount<NonNegative>, BackendError> {
        EmptyChain.address_balance(addresses).await
    }

    async fn address_utxos(
        &self,
        addresses: HashSet<transparent::Address>,
    ) -> Result<Vec<AddressUtxo>, BackendError> {
        EmptyChain.address_utxos(addresses).await
    }

    async fn mempool_transactions(&self) -> Result<Vec<UnminedTx>, BackendError> {
        EmptyChain.mempool_transactions().await
    }

    async fn submit_transaction(
        &self,
        transaction: Transaction,
    ) -> Result<Submission, BackendError> {
        EmptyChain.submit_transaction(transaction).await
    }

    async fn standard_fee(&self) -> Result<StandardFee, BackendError> {
        EmptyChain.standard_fee().await
    }
}
