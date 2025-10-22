use serde::{Deserialize, Serialize};

// User types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub user_id: String,
    pub username: String,
    pub nickname: String,
    pub profile_image: Option<Vec<u8>>, // Under 5MiB
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAuth {
    pub user_id: String,
    pub firebase_token: String,
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

// Message types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextMessage {
    pub message_id: String,
    pub channel_id: String,
    pub sender_id: String,
    pub content: Vec<u8>, // E2E encrypted with server key
    pub timestamp: u64,
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
    async fn register_user(
        auth: UserAuth,
        username: String,
        nickname: String,
    ) -> Result<User, String>;
    async fn get_user(user_id: String) -> Result<User, String>;
    async fn update_profile(
        user_id: String,
        nickname: Option<String>,
        profile_image: Option<Vec<u8>>,
    ) -> Result<(), String>;

    // Channel operations
    async fn create_channel(
        creator_id: String,
        name: String,
        visibility: ChannelVisibility,
    ) -> Result<Channel, String>;
    async fn list_channels(user_id: String) -> Result<Vec<Channel>, String>;
    async fn join_channel(user_id: String, channel_id: String) -> Result<(), String>;
    async fn leave_channel(user_id: String, channel_id: String) -> Result<(), String>;
    async fn invite_to_channel(
        inviter_id: String,
        invitee_id: String,
        channel_id: String,
    ) -> Result<(), String>;

    // Message operations
    async fn send_message(message: TextMessage) -> Result<(), String>;
    async fn get_messages(
        channel_id: String,
        user_id: String,
        limit: u32,
    ) -> Result<Vec<TextMessage>, String>;

    // Voice call operations
    async fn start_voice_call(
        channel_id: String,
        initiator_id: String,
    ) -> Result<VoiceCallSession, String>;
    async fn get_voice_call(channel_id: String) -> Result<Option<VoiceCallSession>, String>;
    async fn end_voice_call(channel_id: String, user_id: String) -> Result<(), String>;
}
