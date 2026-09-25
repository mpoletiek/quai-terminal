//! Core error type. Messages are safe to print: they never include secret material.

use thiserror::Error;

/// Result alias for wallet core operations.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Errors surfaced to the CLI and TUI. Each maps to a stable exit code.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Invalid user input or arguments.
    #[error("{0}")]
    Invalid(String),
    /// The requested wallet, account, token or record does not exist.
    #[error("{0}")]
    NotFound(String),
    /// The wallet must be unlocked or the password was rejected.
    #[error("{0}")]
    Locked(String),
    /// A policy or confirmation check refused the action.
    #[error("{0}")]
    Rejected(String),
    /// Not enough funds or fee policy exceeded.
    #[error("{0}")]
    Insufficient(String),
    /// The network, node or capability is unavailable or mismatched.
    #[error("{0}")]
    Network(String),
    /// Submission outcome is unknown; the operation must be reconciled.
    #[error("{0}")]
    Ambiguous(String),
    /// Waiting timed out.
    #[error("{0}")]
    Timeout(String),
    /// Storage or filesystem failure, or a conflicting concurrent change.
    #[error("{0}")]
    Storage(String),
    /// The chain executed but reverted or failed.
    #[error("{0}")]
    Execution(String),
    /// An exact approval of `token` must be signed first. Multi-step flows branch on this
    /// variant, never on the message's wording.
    #[error("{message}")]
    ApprovalNeeded { message: String, token: String },
}

impl CoreError {
    /// Stable process exit code per the CLI contract.
    pub fn exit_code(&self) -> i32 {
        match self {
            CoreError::Invalid(_) | CoreError::NotFound(_) => 2,
            CoreError::Locked(_) => 3,
            CoreError::Rejected(_) | CoreError::ApprovalNeeded { .. } => 4,
            CoreError::Insufficient(_) => 5,
            CoreError::Network(_) => 6,
            CoreError::Ambiguous(_) => 7,
            CoreError::Timeout(_) => 8,
            CoreError::Storage(_) => 9,
            CoreError::Execution(_) => 10,
        }
    }

    /// Short machine-readable category.
    pub fn kind(&self) -> &'static str {
        match self {
            CoreError::Invalid(_) => "invalid",
            CoreError::NotFound(_) => "not_found",
            CoreError::Locked(_) => "locked",
            CoreError::Rejected(_) => "rejected",
            CoreError::Insufficient(_) => "insufficient",
            CoreError::Network(_) => "network",
            CoreError::Ambiguous(_) => "ambiguous",
            CoreError::Timeout(_) => "timeout",
            CoreError::Storage(_) => "storage",
            CoreError::Execution(_) => "execution",
            CoreError::ApprovalNeeded { .. } => "approval_needed",
        }
    }
}

/// An approval of `token` is needed before the action (see [`CoreError::ApprovalNeeded`]).
pub fn approval_needed(token: impl Into<String>, message: impl Into<String>) -> CoreError {
    CoreError::ApprovalNeeded { message: message.into(), token: token.into() }
}

/// Convenience constructor for invalid input.
pub fn invalid(msg: impl Into<String>) -> CoreError {
    CoreError::Invalid(msg.into())
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Storage(format!("filesystem: {e}"))
    }
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        CoreError::Storage(format!("database: {e}"))
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::Storage(format!("json: {e}"))
    }
}

impl From<quai_sdk::wallet::storage::StorageError> for CoreError {
    fn from(e: quai_sdk::wallet::storage::StorageError) -> Self {
        CoreError::Storage(format!("wallet state: {e}"))
    }
}

/// The node's own message for a JSON-RPC error, cleaned for display: control and direction
/// characters removed, at most 160 characters. The SDK keeps it out of `Display` because it can
/// echo request values; the wallet shows it only to its owner, next to the code.
fn remote_detail(e: &quai_sdk::rpc::RpcError) -> Option<String> {
    let quai_sdk::rpc::RpcError::Remote(remote) = e else { return None };
    let text = crate::text::clean(&remote.message, 160);
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn provider_rpc(e: &quai_sdk::ProviderError) -> Option<&quai_sdk::rpc::RpcError> {
    match e {
        quai_sdk::ProviderError::Rpc(r) => Some(r),
        _ => None,
    }
}

/// A node error with its message: "insufficient funds" is a balance problem, the rest network.
fn node_error(prefix: &str, e: &dyn std::fmt::Display, rpc: Option<&quai_sdk::rpc::RpcError>) -> CoreError {
    match rpc.and_then(remote_detail) {
        Some(detail) if detail.to_lowercase().contains("insufficient funds") => {
            CoreError::Insufficient(format!("the balance does not cover the amount plus the fee (node: {detail})"))
        }
        Some(detail) => CoreError::Network(format!("{prefix}{e}: {detail}")),
        None => CoreError::Network(format!("{prefix}{e}")),
    }
}

impl From<quai_sdk::ProviderError> for CoreError {
    fn from(e: quai_sdk::ProviderError) -> Self {
        node_error("node: ", &e, provider_rpc(&e))
    }
}

impl From<quai_sdk::wallet::WalletError> for CoreError {
    fn from(e: quai_sdk::wallet::WalletError) -> Self {
        CoreError::Invalid(format!("wallet: {e}"))
    }
}

/// The SDK's own verdict on a failure, for everything a conversion does not treat specially:
/// moved state, network trouble and a node on another chain are the network's; a possibly
/// accepted submission is ambiguous; the rest is the request's.
fn by_class(class: quai_sdk::ErrorClass, text: String) -> CoreError {
    use quai_sdk::ErrorClass as C;
    match class {
        C::Transient | C::Stale => CoreError::Network(text),
        C::NetworkMismatch => CoreError::Network(format!("{text} (the node is on a different network than this profile)")),
        C::Ambiguous => CoreError::Ambiguous(text),
        C::Storage => CoreError::Storage(text),
        C::Cancelled => CoreError::Rejected(text),
        _ => CoreError::Invalid(text),
    }
}

impl From<quai_sdk::accounts::AccountError> for CoreError {
    fn from(e: quai_sdk::accounts::AccountError) -> Self {
        use quai_sdk::accounts::AccountError as A;
        match &e {
            A::InsufficientBalance | A::FeeLimit => CoreError::Insufficient(e.to_string()),
            A::Broadcast(_) => CoreError::Ambiguous(format!("submission: {e}")),
            A::Provider(p) => node_error("", &e, provider_rpc(p)),
            _ => by_class(e.class(), e.to_string()),
        }
    }
}

impl From<quai_sdk::qi::QiError> for CoreError {
    fn from(e: quai_sdk::qi::QiError) -> Self {
        use quai_sdk::qi::QiError as Q;
        if let Q::Provider(p) = &e
            && provider_rpc(p).and_then(remote_detail).is_some()
        {
            return node_error("", &e, provider_rpc(p));
        }
        let text = e.to_string();
        // Coin selection says when the balance or the fee cap is the problem; its class alone
        // would call that an invalid request.
        use quai_sdk::wallet::SelectionError as Sel;
        if matches!(e, Q::Selection(Sel::InsufficientFunds | Sel::FeeBudgetExceeded)) {
            return CoreError::Insufficient(text);
        }
        // A fee the node accepts as valid but no miner will take. Its class calls that an invalid
        // request, which sends the user looking for a typo; it is a fee that is too small, and it
        // belongs with the other fee problems. The shape is worth naming because it, not the fee,
        // is usually the thing to change.
        if matches!(e, Q::FeeBelowInclusionFloor) {
            return CoreError::Insufficient(format!(
                "{text}: a conversion is priced by how many Quai-ledger outputs it creates, so raise the fee or build fewer of them"
            ));
        }
        if matches!(e, Q::Broadcast(_)) {
            return CoreError::Ambiguous(text);
        }
        by_class(e.class(), text)
    }
}

impl From<quai_sdk::contracts::ContractError> for CoreError {
    fn from(e: quai_sdk::contracts::ContractError) -> Self {
        if let quai_sdk::contracts::ContractError::Provider(p) = &e
            && let Some(detail) = provider_rpc(p).and_then(remote_detail)
        {
            // Reverts and failed calls carry their reason in the message.
            return CoreError::Invalid(format!("contract: {e}: {detail}"));
        }
        CoreError::Invalid(format!("contract: {e}"))
    }
}

impl From<quai_sdk::payments::PaymentError> for CoreError {
    fn from(e: quai_sdk::payments::PaymentError) -> Self {
        CoreError::Invalid(format!("payment code: {e}"))
    }
}

impl From<quai_sdk::primitives::AmountError> for CoreError {
    fn from(e: quai_sdk::primitives::AmountError) -> Self {
        CoreError::Invalid(format!("amount: {e}"))
    }
}

impl From<quai_sdk::consensus::TransactionError> for CoreError {
    fn from(e: quai_sdk::consensus::TransactionError) -> Self {
        CoreError::Invalid(format!("transaction: {e}"))
    }
}

impl From<quai_sdk::rpc::RouteError> for CoreError {
    fn from(e: quai_sdk::rpc::RouteError) -> Self {
        CoreError::Invalid(format!("rpc route: {e}"))
    }
}

impl From<quai_sdk::rpc::RpcError> for CoreError {
    fn from(e: quai_sdk::rpc::RpcError) -> Self {
        node_error("rpc: ", &e, Some(&e))
    }
}

impl From<quai_sdk::crypto::CryptoError> for CoreError {
    fn from(e: quai_sdk::crypto::CryptoError) -> Self {
        CoreError::Invalid(format!("key: {e}"))
    }
}

impl From<quai_sdk::signer::SignerError> for CoreError {
    fn from(e: quai_sdk::signer::SignerError) -> Self {
        CoreError::Invalid(format!("signer: {e}"))
    }
}

#[cfg(test)]
mod class_tests {
    use super::*;
    use quai_sdk::accounts::AccountError;
    use quai_sdk::qi::QiError;

    /// A node on another chain, moved state and missing reads are the network's (exit 6), and
    /// never the user's input; balance problems stay balance problems.
    #[test]
    fn sdk_failures_map_by_their_class() {
        let code = |e: CoreError| e.exit_code();
        assert_eq!(code(AccountError::NetworkMismatch.into()), 6);
        assert_eq!(code(AccountError::ObservationChanged.into()), 6);
        assert_eq!(code(AccountError::InsufficientBalance.into()), 5);
        assert_eq!(code(AccountError::FeeLimit.into()), 5);
        assert_eq!(code(AccountError::InvalidOperation.into()), 2);
        assert_eq!(code(QiError::NetworkMismatch.into()), 6);
        assert_eq!(code(QiError::StaleSnapshot.into()), 6);
        assert_eq!(code(QiError::IncompleteObservation.into()), 6);
        assert_eq!(code(QiError::InvalidPolicy.into()), 2);
        assert_eq!(code(QiError::MailboxUnreadable.into()), 2);
        use quai_sdk::wallet::SelectionError as Sel;
        assert_eq!(code(QiError::Selection(Sel::InsufficientFunds).into()), 5);
        assert_eq!(code(QiError::Selection(Sel::FeeBudgetExceeded).into()), 5, "over the fee cap is a funds problem");
        assert_eq!(code(QiError::Selection(Sel::LimitExceeded).into()), 2);
        let mismatch: CoreError = QiError::NetworkMismatch.into();
        assert!(mismatch.to_string().contains("different network"), "{mismatch}");
    }
}

#[cfg(test)]
mod node_error_tests {
    use super::*;

    fn remote(message: &str) -> quai_sdk::rpc::RpcError {
        let e: quai_sdk::rpc::RemoteError = serde_json::from_value(serde_json::json!({"code": -32000, "message": message})).unwrap();
        quai_sdk::rpc::RpcError::Remote(e)
    }

    #[test]
    fn node_messages_are_shown_cleaned() {
        let e = CoreError::from(quai_sdk::ProviderError::Rpc(remote("insufficient funds for transfer")));
        assert!(matches!(&e, CoreError::Insufficient(t) if t.contains("insufficient funds for transfer")), "{e}");
        let e = CoreError::from(remote("execution reverted: \u{202E}spoof\u{7}"));
        assert!(matches!(&e, CoreError::Network(t) if t.ends_with("execution reverted: spoof")), "{e}");
        let long = "x".repeat(500);
        assert!(CoreError::from(remote(&long)).to_string().len() < 260);
        assert!(!CoreError::from(quai_sdk::rpc::RpcError::Timeout).to_string().contains(": :"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A needed approval says so by its variant, and carries the token that needs it, so a flow
    /// never depends on how the message happens to be worded.
    #[test]
    fn a_needed_approval_is_typed_and_names_its_token() {
        let e = approval_needed("0x00aa", "approve the LP for the gauge first (step 1 of 2)");
        assert!(matches!(&e, CoreError::ApprovalNeeded { token, .. } if token == "0x00aa"));
        assert_eq!((e.exit_code(), e.kind()), (4, "approval_needed"));
        assert_eq!(e.to_string(), "approve the LP for the gauge first (step 1 of 2)");
    }
}
