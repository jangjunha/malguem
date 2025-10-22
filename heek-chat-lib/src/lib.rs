use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PaginationDirection {
    Before(u64),
    After(Option<u64>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
    pub direction: PaginationDirection,
    pub limit: u64,
}

// User types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub user_id: String,
    pub username: String,
    pub profile_image: Option<Vec<u8>>, // Under 5MiB
}

// Channel types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelVisibility {
    Public,
    Private,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub channel_id: String,
    pub name: String,
    pub visibility: ChannelVisibility,
    pub member_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub cursor: u64,
    pub sender_id: String,
    pub content: Vec<u8>, // E2E encrypted with server key
    pub timestamp: DateTime<Utc>,
}

// Voice call types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceCallSession {
    pub session_id: String,
    pub channel_id: String,
    pub participants: Vec<String>,
    pub encryption_keys: Vec<EncryptionKey>, // Per-sender keys
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionKey {
    pub sender_id: String,
    pub key: Vec<u8>, // AEGIS-256 key encrypted with server key
}

// WebRTC signaling types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalingMessage {
    Offer {
        sdp: String,
        sender_id: String,
    },
    Answer {
        sdp: String,
        sender_id: String,
    },
    IceCandidate {
        candidate: String,
        sender_id: String,
    },
    JoinCall {
        channel_id: String,
    },
    LeaveCall {
        channel_id: String,
    },
}

// RPC service definition
#[tarpc::service]
pub trait ChatService {
    // User operations
    async fn join(
        id_token: String,
        invitation_token: String,
        username: String,
    ) -> Result<String, String>;
    async fn get_me(id_token: String) -> Result<User, String>;
    async fn get_user(id_token: String, user_id: String) -> Result<User, String>;
    async fn update_profile(
        id_token: String,
        username: Option<String>,
        profile_image: Option<Vec<u8>>,
    ) -> Result<(), String>;

    // Channel operations
    async fn create_channel(
        id_token: String,
        name: String,
        visibility: ChannelVisibility,
    ) -> Result<Channel, String>;
    async fn list_channels(id_token: String) -> Result<Vec<Channel>, String>;
    async fn join_channel(id_token: String, channel_id: String) -> Result<(), String>;
    async fn leave_channel(id_token: String, channel_id: String) -> Result<(), String>;
    async fn invite_to_channel(
        id_token: String,
        invitee_id: String,
        channel_id: String,
    ) -> Result<(), String>;

    // Message operations
    async fn send_message(
        id_token: String,
        channel_id: String,
        content: Vec<u8>,
    ) -> Result<(), String>;
    async fn get_messages(
        id_token: String,
        channel_id: String,
        pagination: Pagination,
    ) -> Result<Vec<Message>, String>;

    // Voice call operations
    async fn start_voice_call(
        id_token: String,
        channel_id: String,
    ) -> Result<VoiceCallSession, String>;
    async fn get_voice_call(
        id_token: String,
        channel_id: String,
    ) -> Result<Option<VoiceCallSession>, String>;
    async fn end_voice_call(id_token: String, channel_id: String) -> Result<(), String>;
}
