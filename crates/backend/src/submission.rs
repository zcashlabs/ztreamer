//! [`Submission`]: the node's answer to a transaction that a wallet submits.

use zakura_chain::transaction;

/// The node's answer to [`StateBackend::submit_transaction`](crate::StateBackend).
///
/// A node that refuses the transaction answers, so a refusal is a [`Submission`] and not a
/// [`BackendError`](crate::BackendError). The service reports it to the wallet in a successful
/// `SendResponse`, as `lightwalletd` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submission {
    /// The node verified the transaction and queued it in its mempool. Holds its id.
    Accepted(transaction::Hash),
    /// The node refused the transaction.
    Rejected(Rejection),
}

/// Why the node refused a transaction, as its `sendrawtransaction` RPC would report it.
///
/// # Examples
///
/// ```
/// use ztreamer_backend::Rejection;
///
/// // zakura answers a transaction that fails verification with the zcashd legacy code -25.
/// let rejection = Rejection::new(-25, "transaction did not pass consensus validation");
/// assert_eq!(rejection.code, -25);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    /// The JSON-RPC error code of the node, a zcashd legacy code.
    pub code: i32,
    /// The error message of the node.
    pub message: String,
}

impl Rejection {
    /// A rejection with this code and message.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}
