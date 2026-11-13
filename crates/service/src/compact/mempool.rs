//! Mempool requests: submitting transactions, streaming the mempool, and compact mempool
//! transactions.

use std::{collections::HashSet, sync::Arc, time::Duration};

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;
use tracing::{Instrument, Span};
use zakura_chain::{
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{self, Transaction},
};
use zakura_rpc::server::error::LegacyCode;
use ztreamer_backend::{Rejection, Submission};
use ztreamer_protocol::proto;

use super::{CompactService, RpcStream, ensure_tip_ready};
use crate::project::{PoolSelection, compact_mempool_txs, excluded_txids};

/// The largest raw transaction that `SendTransaction` takes, in bytes.
///
/// A transaction cannot be larger than a block, so a larger one can never be mined. Thus the
/// service refuses it before it parses it, as `lightwalletd` does since GHSA-6ppp-r2gc-9q6v.
pub(crate) const MAX_RAW_TRANSACTION_BYTES: usize = 2_000_000;

impl CompactService {
    /// Submits a raw transaction to the node, as `lightwalletd` does.
    ///
    /// Empty data and data over [`MAX_RAW_TRANSACTION_BYTES`] fail with `INVALID_ARGUMENT`. A
    /// node that refuses the transaction gives a `SendResponse` with its code and message. Bytes
    /// that are not a transaction give the answer of Zakura's `sendrawtransaction` for them:
    /// code -22 and the parse error. An accepted transaction gives code 0 and its txid.
    pub(crate) async fn send_transaction(
        &self,
        request: proto::RawTransaction,
    ) -> Result<proto::SendResponse, Status> {
        if request.data.is_empty() {
            return Err(Status::invalid_argument("bad transaction data"));
        }
        if request.data.len() > MAX_RAW_TRANSACTION_BYTES {
            return Err(Status::invalid_argument(format!(
                "transaction is too large: {} bytes (limit {MAX_RAW_TRANSACTION_BYTES})",
                request.data.len()
            )));
        }
        // Zakura answers `sendrawtransaction` for bytes that do not parse with this code and
        // the parse error, and `lightwalletd` passes that on.
        let submission = match Transaction::zcash_deserialize(request.data.as_slice()) {
            Ok(transaction) => self.backend.submit_transaction(transaction).await?,
            Err(error) => Submission::Rejected(Rejection::new(
                LegacyCode::Deserialization.into(),
                error.to_string(),
            )),
        };
        Ok(send_response(submission))
    }

    pub(crate) async fn mempool_stream(&self) -> Result<RpcStream<proto::RawTransaction>, Status> {
        let backend = Arc::clone(&self.backend);
        let mut snapshots = self.subscribe();
        let tip = {
            let snapshot = snapshots.borrow();
            ensure_tip_ready(&snapshot)?;
            snapshot.visible_tip
        };
        let mut transactions = backend.mempool_transactions().await?;
        let (sender, receiver) = mpsc::channel(1);
        let producer = async move {
            // The stream ends when the head follower publishes a new visible tip. While the head
            // is stale or in deep reorg recovery, no new tip comes. Thus the stream fails.
            let tip_changed = async move {
                let snapshot = snapshots
                    .wait_for(|snapshot| {
                        snapshot.visible_tip != tip || ensure_tip_ready(snapshot).is_err()
                    })
                    .await
                    .map_err(|_| Status::unavailable("the service stopped"))?;
                ensure_tip_ready(&snapshot)
            };
            tokio::pin!(tip_changed);
            let mut seen = HashSet::new();
            loop {
                for transaction in transactions.drain(..) {
                    if !seen.insert(transaction.id()) {
                        continue;
                    }
                    let transaction = match transaction.transaction().zcash_serialize_to_vec() {
                        Ok(data) => proto::RawTransaction { data, height: 0 },
                        Err(error) => {
                            let _ = sender.send(Err(Status::internal(error.to_string()))).await;
                            return;
                        }
                    };
                    tokio::select! {
                        biased;
                        changed = &mut tip_changed => {
                            if let Err(status) = changed {
                                let _ = sender.send(Err(status)).await;
                            }
                            return;
                        }
                        result = sender.send(Ok(transaction)) => {
                            if result.is_err() {
                                return;
                            }
                        }
                    }
                }

                transactions = tokio::select! {
                    biased;
                    changed = &mut tip_changed => {
                        if let Err(status) = changed {
                            let _ = sender.send(Err(status)).await;
                        }
                        return;
                    }
                    // Stop when the client closes the stream. An idle stream sends nothing, thus
                    // no send can find the closed channel.
                    () = sender.closed() => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        match backend.mempool_transactions().await {
                            Ok(transactions) => transactions,
                            Err(error) => {
                                let _ = sender.send(Err(Status::from(error))).await;
                                return;
                            }
                        }
                    }
                };
            }
        };
        tokio::spawn(producer.in_current_span());
        Ok(Box::pin(ReceiverStream::new(receiver)))
    }

    pub(crate) async fn mempool_tx(
        &self,
        request: proto::GetMempoolTxRequest,
    ) -> Result<RpcStream<proto::CompactTx>, Status> {
        PoolSelection::from_mempool_request(&request.pool_types)?;
        let transactions = self.backend.mempool_transactions().await?;
        let span = Span::current();
        let compact = tokio::task::spawn_blocking(move || {
            span.in_scope(|| mempool_compact(transactions, &request))
        })
        .await
        .map_err(|error| Status::internal(format!("mempool conversion failed: {error}")))??;
        Ok(Box::pin(tokio_stream::iter(compact.into_iter().map(Ok))))
    }
}

/// The `SendResponse` for the answer of the node: code 0 and the txid, or the code and the
/// message of a rejection.
fn send_response(submission: Submission) -> proto::SendResponse {
    match submission {
        Submission::Accepted(txid) => {
            proto::SendResponse { error_code: 0, error_message: txid.to_string() }
        }
        Submission::Rejected(Rejection { code, message }) => {
            tracing::debug!(code, %message, "the node rejected a transaction");
            proto::SendResponse { error_code: code, error_message: message }
        }
    }
}

fn mempool_compact(
    transactions: Vec<transaction::UnminedTx>,
    request: &proto::GetMempoolTxRequest,
) -> Result<Vec<proto::CompactTx>, Status> {
    let pools = PoolSelection::from_mempool_request(&request.pool_types)?;
    let excluded = excluded_txids(
        transactions.iter().map(|tx| tx.id().mined_id()),
        &request.exclude_txid_suffixes,
    );
    let transactions = transactions
        .into_iter()
        .filter(|tx| !excluded.contains(&tx.id().mined_id()))
        .map(|transaction| {
            transaction
                .transaction()
                .zcash_serialize_to_vec()
                .map(|bytes| (bytes, transaction.id().mined_id()))
                .map_err(|error| Status::internal(error.to_string()))
        })
        .collect::<Result<Vec<_>, Status>>()?;
    compact_mempool_txs(&transactions, pools, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;
    use serde_json::json;
    use tokio::sync::watch;
    use zakura_chain::{block, parameters::Network};
    use zakura_rpc::client::GetBlockchainInfoResponse;
    use zakura_test::vectors::{BLOCK_MAINNET_949496_BYTES, BLOCK_TESTNET_1842421_BYTES};
    use ztreamer_index::index::{BlockId, Index};
    use ztreamer_remote::{
        RemoteBackend, RpcClient, ZakuraClient,
        test_utils::{MockNode, Reply},
    };
    use ztreamer_sync::head::ServingSnapshot;

    /// A service over an empty index and a remote node with an empty mempool, and the sender of
    /// its snapshots. Its head is fresh.
    async fn remote_service()
    -> (CompactService, watch::Sender<ServingSnapshot>, MockNode, tempfile::TempDir) {
        let mock = MockNode::start(|_, seen| match seen.body["method"].as_str().unwrap() {
            "getblockchaininfo" => {
                Reply::Result(serde_json::to_value(GetBlockchainInfoResponse::default()).unwrap())
            }
            "getblockhash" => Reply::Result(json!(Network::Mainnet.genesis_hash().to_string())),
            _ => Reply::Result(json!([])),
        })
        .await;
        let node = ZakuraClient::new(RpcClient::new(mock.config()).unwrap());
        let backend = RemoteBackend::connect(node, Network::Mainnet).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let index =
            Arc::new(Index::open(dir.path(), 10 << 20, "Mainnet", block::Hash([9; 32])).unwrap());
        let state = index.state().unwrap();
        let (snapshots, receiver) = watch::channel(ServingSnapshot::from(state));
        let service = CompactService::with_backend(index, receiver, "main", backend);
        snapshots.send_modify(|snapshot| snapshot.tip_fresh = true);
        (service, snapshots, mock, dir)
    }

    const WAIT: Duration = Duration::from_millis(300);

    fn raw(data: Vec<u8>) -> proto::RawTransaction {
        proto::RawTransaction { data, height: 0 }
    }

    #[tokio::test]
    async fn send_refuses_empty_and_oversized_data() {
        let (service, _snapshots, mock, _dir) = remote_service().await;
        let error = service.send_transaction(raw(Vec::new())).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(error.message(), "bad transaction data");

        let error = service
            .send_transaction(raw(vec![0; MAX_RAW_TRANSACTION_BYTES + 1]))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("too large"), "{}", error.message());
        assert!(mock.seen().iter().all(|seen| seen.body["method"] != "sendrawtransaction"));
    }

    #[tokio::test]
    async fn send_answers_undecodable_bytes_like_zakura() {
        let (service, _snapshots, _mock, _dir) = remote_service().await;
        let response = service.send_transaction(raw(vec![0xde, 0xad, 0xbe, 0xef])).await.unwrap();
        assert_eq!(response.error_code, -22);
        let expected = Transaction::zcash_deserialize(&[0xde, 0xad, 0xbe, 0xef][..]).unwrap_err();
        assert_eq!(response.error_message, expected.to_string());
    }

    #[tokio::test]
    async fn mempool_stream_ends_when_the_visible_tip_changes() {
        let (service, snapshots, _mock, _dir) = remote_service().await;
        let mut stream = service.mempool_stream().await.unwrap();

        // A new snapshot with the same visible tip does not end the stream.
        snapshots.send_modify(|snapshot| snapshot.generation += 1);
        assert!(tokio::time::timeout(WAIT, stream.next()).await.is_err());

        snapshots.send_modify(|snapshot| {
            snapshot.visible_tip = Some(BlockId::new(1, block::Hash([1; 32])));
        });
        assert!(tokio::time::timeout(10 * WAIT, stream.next()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mempool_stream_fails_while_the_head_is_stale() {
        let (service, snapshots, _mock, _dir) = remote_service().await;
        let mut stream = service.mempool_stream().await.unwrap();

        // No new tip comes while the head is stale, so the stream fails.
        snapshots.send_modify(|snapshot| snapshot.tip_fresh = false);
        let error = tokio::time::timeout(10 * WAIT, stream.next()).await.unwrap().unwrap();
        assert_eq!(error.unwrap_err().code(), tonic::Code::Unavailable);
        assert!(stream.next().await.is_none());

        // While the head is stale, the service also refuses a new stream.
        let error = service.mempool_stream().await.err().unwrap();
        assert_eq!(error.code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn mempool_stream_task_ends_when_the_client_is_gone() {
        let (service, snapshots, _mock, _dir) = remote_service().await;
        let stream = service.mempool_stream().await.unwrap();
        // The task of the stream holds the only snapshot receiver beside the service.
        assert_eq!(snapshots.receiver_count(), 2);

        drop(stream);
        tokio::time::timeout(10 * WAIT, async {
            while snapshots.receiver_count() > 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn mempool_requests_select_their_pools_and_exclusions() {
        let sapling = shielded_mempool_transaction(&BLOCK_MAINNET_949496_BYTES, |transaction| {
            transaction.sapling_outputs().next().is_some()
        });
        let orchard = shielded_mempool_transaction(&BLOCK_TESTNET_1842421_BYTES, |transaction| {
            transaction.orchard_actions().next().is_some()
        });
        let sapling_txid = sapling.id().mined_id().0.to_vec();
        let orchard_txid = orchard.id().mined_id().0.to_vec();
        let mempool = vec![sapling, orchard];
        let txids = |compact: Vec<proto::CompactTx>| {
            compact.into_iter().map(|transaction| transaction.txid).collect::<Vec<_>>()
        };

        let compact =
            mempool_compact(mempool.clone(), &proto::GetMempoolTxRequest::default()).unwrap();
        assert_eq!(txids(compact.clone()), [sapling_txid.clone(), orchard_txid.clone()]);
        assert!(!compact[0].outputs.is_empty());
        assert!(!compact[1].actions.is_empty());

        let orchard_only = proto::GetMempoolTxRequest {
            pool_types: vec![proto::PoolType::Orchard as i32],
            ..Default::default()
        };
        assert_eq!(
            txids(mempool_compact(mempool.clone(), &orchard_only).unwrap()),
            vec![orchard_txid.clone()]
        );

        let excluded = proto::GetMempoolTxRequest {
            exclude_txid_suffixes: vec![orchard_txid[30..].to_vec()],
            ..Default::default()
        };
        assert_eq!(txids(mempool_compact(mempool.clone(), &excluded).unwrap()), [sapling_txid]);

        let transparent = proto::GetMempoolTxRequest {
            pool_types: vec![proto::PoolType::Transparent as i32],
            ..Default::default()
        };
        let compact = mempool_compact(mempool, &transparent).unwrap();
        assert!(!compact.is_empty());
        assert!(compact.iter().all(|tx| tx.spends.is_empty()
            && tx.outputs.is_empty()
            && tx.actions.is_empty()
            && tx.ironwood_actions.is_empty()));
    }

    fn shielded_mempool_transaction(
        block: &[u8],
        shielded: impl Fn(&transaction::Transaction) -> bool,
    ) -> transaction::UnminedTx {
        block::Block::zcash_deserialize(block)
            .unwrap()
            .transactions
            .iter()
            .find(|candidate| shielded(candidate))
            .cloned()
            .expect("the block vector holds a shielded transaction")
            .into()
    }
}
