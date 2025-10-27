use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod tarpc;

pub type UserID = Uuid;
pub type ChannelID = Uuid;

/// Message envelope for multiplexing RPC and Events over a single WebSocket connection
/// This is an application-level multiplexing structure, not part of the transport layer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MultiplexedMessage<Rpc> {
    /// RPC request or response (handled by tarpc)
    Rpc(Rpc),
    /// Server-initiated event (handled by application)
    Event(Event),
}

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
    pub user_id: UserID,
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
    pub channel_id: ChannelID,
    pub name: String,
    pub visibility: ChannelVisibility,
    pub member_ids: HashSet<UserID>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub cursor: u64,
    pub sender_id: UserID,
    pub content: Vec<u8>, // E2E encrypted with server key
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RTCSession {
    pub channel_id: ChannelID,
    pub participants: HashSet<UserID>,
    // pub encryption_keys: Vec<EncryptionKey>, // Per-sender keys
}

impl RTCSession {
    pub fn new(channel_id: &ChannelID) -> Self {
        Self {
            channel_id: channel_id.clone(),
            participants: HashSet::new(),
        }
    }
}

// #[derive(Debug, Clone, Serialize, Deserialize)]
// pub struct EncryptionKey {
//     pub sender_id: UserID,
//     pub key: Vec<u8>, // AEGIS-256 key encrypted with server key
// }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    RTCSession {
        channel_id: ChannelID,
        event: RTCSessionEvent,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RTCSessionEvent {
    UserJoined {
        user_id: UserID,
        // encryption_key: Vec<u8>, // AEGIS-256 key for this user (encrypted with server key)
    },
    UserLeft {
        user_id: UserID,
    },
    OfferReceived {
        from_user_id: UserID,
        sdp: String,
    },
    AnswerReceived {
        from_user_id: UserID,
        sdp: String,
    },
    IceCandidateReceived {
        from_user_id: UserID,
        candidate: String,
    },
}

#[::tarpc::service]
pub trait ChatService {
    // User operations
    async fn join(
        id_token: String,
        invitation_token: String,
        username: String,
    ) -> Result<UserID, String>;
    async fn get_me(id_token: String) -> Result<User, String>;
    async fn get_user(id_token: String, user_id: UserID) -> Result<User, String>;
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
    async fn join_channel(id_token: String, channel_id: ChannelID) -> Result<(), String>;
    async fn leave_channel(id_token: String, channel_id: ChannelID) -> Result<(), String>;
    async fn invite_to_channel(
        id_token: String,
        invitee_id: UserID,
        channel_id: ChannelID,
    ) -> Result<(), String>;

    // Message operations
    async fn send_message(
        id_token: String,
        channel_id: ChannelID,
        content: Vec<u8>,
    ) -> Result<(), String>;
    async fn get_messages(
        id_token: String,
        channel_id: ChannelID,
        pagination: Pagination,
    ) -> Result<Vec<Message>, String>;

    // RTC operations
    async fn join_rtc_session(
        id_token: String,
        channel_id: ChannelID,
    ) -> Result<RTCSession, String>;
    async fn leave_rtc_session(id_token: String, channel_id: ChannelID) -> Result<(), String>;
    async fn offer_rtc_connection(
        id_token: String,
        channel_id: ChannelID,
        target_user_id: UserID,
        sdp: String,
    ) -> Result<(), String>;
    async fn answer_rtc_connection(
        id_token: String,
        channel_id: ChannelID,
        target_user_id: UserID,
        sdp: String,
    ) -> Result<(), String>;
    async fn ice_candidate_rtc_connection(
        id_token: String,
        channel_id: ChannelID,
        target_user_id: UserID,
        candidate: String,
    ) -> Result<(), String>;
}
