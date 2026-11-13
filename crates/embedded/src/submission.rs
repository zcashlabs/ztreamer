//! [`submission`]: the answer of the embedded mempool, in the codes of Zakura's
//! `sendrawtransaction` RPC.

use zakura_chain::transaction::UnminedTxId;
use zakura_rpc::server::error::LegacyCode;
use ztreamer_backend::{BackendError, Rejection, Submission};

use crate::NodeError;

/// Maps the result of [`NodeClient::submit_transaction`](crate::NodeClient::submit_transaction)
/// to the answer that a wallet would get from Zakura's `sendrawtransaction` RPC.
///
/// This mirrors `send_raw_transaction` in `zakura-rpc` 15.0.0 (`src/methods.rs`): a refusal of
/// the mempool queue and a dropped verification are [`LegacyCode::Misc`], a failed verification
/// is [`LegacyCode::Verify`], and the message is the error of the mempool. A mempool service that
/// fails the request, or that answers what it should not, is a node failure, not a refusal.
pub(crate) fn submission(
    result: Result<UnminedTxId, NodeError>,
) -> Result<Submission, BackendError> {
    let rejection = |code: LegacyCode, error: &dyn ToString| {
        Ok(Submission::Rejected(Rejection::new(code.into(), error.to_string())))
    };
    match result {
        Ok(id) => Ok(Submission::Accepted(id.mined_id())),
        Err(NodeError::Rejected(error)) => rejection(LegacyCode::Misc, &error),
        Err(NodeError::VerifierStopped(error)) => rejection(LegacyCode::Misc, &error),
        Err(NodeError::Verification(error)) => rejection(LegacyCode::Verify, &error),
        Err(
            error @ (NodeError::QueueResults(_)
            | NodeError::UnexpectedMempool(_)
            | NodeError::UnexpectedState(_)),
        ) => Err(BackendError::Unexpected(format!("Zakura mempool: {error}"))),
        Err(error @ (NodeError::Mempool(_) | NodeError::State(_) | NodeError::SyncStopped(_))) => {
            Err(BackendError::Unavailable(format!("Zakura mempool: {error}")))
        }
    }
}
