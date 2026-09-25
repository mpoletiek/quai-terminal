//! What the engine says about private messages.

/// The messaging account, its conversations and the requests waiting, as one view.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MessagingView {
    pub status: wallet_core::messaging::service::Status,
    pub conversations: Vec<wallet_core::messaging::service::Conversation>,
    pub requests: Vec<wallet_core::messaging::service::Conversation>,
}
