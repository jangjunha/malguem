use chrono::{DateTime, TimeDelta, Utc};
use futures::StreamExt;
use heek_chat_lib::{
    Channel, ChannelVisibility, ChatService, EncryptionKey, Message, Pagination,
    PaginationDirection, User, VoiceCallSession,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tarpc::{
    context,
    server::{self, Channel as TarpcChannel},
};
use tokio::sync::RwLock;
use uuid::Uuid;

mod firebase_verify;
use firebase_verify::verify_firebase_token as verify_id_token;

struct Invitation {
    pub inviter_id: Option<String>,
    pub invited_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

struct UserWithCredentials {
    pub user: User,
    pub firebase_uid: String,
}

#[derive(Clone)]
struct ChatServer {
    // In-memory storage for now (will be replaced with PostgreSQL)
    invitations: Arc<RwLock<HashMap<String, Invitation>>>,
    users: Arc<RwLock<HashMap<String, UserWithCredentials>>>,
    channels: Arc<RwLock<HashMap<String, Channel>>>,
    messages: Arc<RwLock<HashMap<String, Vec<Message>>>>,
    voice_calls: Arc<RwLock<HashMap<String, VoiceCallSession>>>,
}

impl ChatServer {
    fn new() -> Self {
        Self {
            invitations: Arc::new(RwLock::new({
                let mut invitations = HashMap::new();
                invitations.insert(
                    "test-inv-token".to_string(),
                    Invitation {
                        inviter_id: None,
                        invited_at: Utc::now(),
                        expires_at: Utc::now() + TimeDelta::days(7),
                    },
                ); // FIXME:
                invitations
            })),
            users: Arc::new(RwLock::new(HashMap::new())),
            channels: Arc::new(RwLock::new(HashMap::new())),
            messages: Arc::new(RwLock::new(HashMap::new())),
            voice_calls: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn authenticate_id_token_only(&self, id_token: &str) -> Result<String, String> {
        let firebase_uid = verify_id_token(&id_token)
            .await
            .map_err(|_| "Invalid token")?;
        Ok(firebase_uid)
    }

    async fn authenticate(&self, id_token: &str) -> Result<User, String> {
        let firebase_uid = self.authenticate_id_token_only(id_token).await?;
        if let Some(user_with_credentials) = self
            .users
            .read()
            .await
            .values()
            .find(|u| u.firebase_uid == firebase_uid)
        {
            Ok(user_with_credentials.user.clone())
        } else {
            Err("Invalid token".to_string())
        }
    }
}

impl ChatService for ChatServer {
    async fn join(
        self,
        _: context::Context,
        id_token: String,
        invitation_token: String,
        username: String,
    ) -> Result<String, String> {
        let firebase_uid = self.authenticate_id_token_only(&id_token).await?;
        let now = Utc::now();

        let mut invitations = self.invitations.write().await;
        if let Some(invitation) = invitations.get(&invitation_token) {
            if invitation.expires_at < now {
                return Err("Invitation expired".to_string());
            }
        } else {
            return Err("Invalid invitation".to_string());
        };

        let mut users = self.users.write().await;
        if users.values().any(|uc| uc.firebase_uid == firebase_uid) {
            return Err("Already joined".to_string());
        }
        if users.values().any(|uc| uc.user.username == *username) {
            return Err("Username already exists".to_string());
        }

        let user_id = Uuid::new_v4().to_string();
        if users.contains_key(&user_id) {
            return Err("Unexpected error".to_string()); // collision
        }

        invitations.remove_entry(&invitation_token);
        users.insert(
            user_id.clone(),
            UserWithCredentials {
                user: User {
                    user_id: user_id.clone(),
                    username,
                    profile_image: None,
                },
                firebase_uid,
            },
        );
        Ok(user_id)
    }

    async fn get_me(self, _: context::Context, id_token: String) -> Result<User, String> {
        let authenticated_user = self.authenticate(&id_token).await?;
        Ok(authenticated_user)
    }

    async fn get_user(
        self,
        _: context::Context,
        id_token: String,
        user_id: String,
    ) -> Result<User, String> {
        let _ = self.authenticate(&id_token).await?;
        self.users
            .read()
            .await
            .get(&user_id)
            .map(|u| &u.user)
            .cloned()
            .ok_or_else(|| "User not found".to_string())
    }

    async fn update_profile(
        self,
        _: context::Context,
        id_token: String,
        username: Option<String>,
        profile_image: Option<Vec<u8>>,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let mut users = self.users.write().await;
        if let Some(username) = &username {
            if users.values().any(|uc| uc.user.username == *username) {
                return Err("Username already exists".to_string());
            }
        }

        let user_with_credentials = users
            .get_mut(&authenticated_user.user_id)
            .ok_or("User not found")?;
        if let Some(username) = username {
            user_with_credentials.user.username = username;
        }
        if let Some(img) = profile_image {
            if img.len() > 5 * 1024 * 1024 {
                return Err("Profile image too large (max 5MiB)".to_string());
            }
            user_with_credentials.user.profile_image = Some(img);
        }

        Ok(())
    }

    async fn create_channel(
        self,
        _: context::Context,
        id_token: String,
        name: String,
        visibility: ChannelVisibility,
    ) -> Result<Channel, String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channel = Channel {
            channel_id: Uuid::new_v4().to_string(),
            name,
            visibility,
            member_ids: vec![authenticated_user.user_id],
        };

        self.channels
            .write()
            .await
            .insert(channel.channel_id.clone(), channel.clone());
        Ok(channel)
    }

    async fn list_channels(
        self,
        _: context::Context,
        id_token: String,
    ) -> Result<Vec<Channel>, String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        Ok(channels
            .values()
            .filter(|c| {
                c.member_ids.contains(&authenticated_user.user_id)
                    || c.visibility == ChannelVisibility::Public
            })
            .cloned()
            .collect())
    }

    async fn join_channel(
        self,
        _: context::Context,
        id_token: String,
        channel_id: String,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let mut channels = self.channels.write().await;
        let channel = channels.get_mut(&channel_id).ok_or("Channel not found")?;

        if channel.visibility == ChannelVisibility::Private
            && !channel.member_ids.contains(&authenticated_user.user_id)
        {
            return Err("Cannot join private channel without invitation".to_string());
        }

        if !channel.member_ids.contains(&authenticated_user.user_id) {
            channel.member_ids.push(authenticated_user.user_id);
        }

        Ok(())
    }

    async fn leave_channel(
        self,
        _: context::Context,
        id_token: String,
        channel_id: String,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let mut channels = self.channels.write().await;
        let channel = channels.get_mut(&channel_id).ok_or("Channel not found")?;

        channel
            .member_ids
            .retain(|id| id != &authenticated_user.user_id);
        Ok(())
    }

    async fn invite_to_channel(
        self,
        _: context::Context,
        id_token: String,
        invitee_id: String,
        channel_id: String,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;
        let inviter_id = &authenticated_user.user_id;

        let mut channels = self.channels.write().await;
        let channel = channels.get_mut(&channel_id).ok_or("Channel not found")?;
        if !channel.member_ids.contains(inviter_id) {
            return Err("Inviter is not a channel member".to_string());
        }

        if channel.visibility == ChannelVisibility::Private
            && !channel.member_ids.contains(inviter_id)
        {
            return Err("Only members can invite to private channels".to_string());
        }

        if !channel.member_ids.contains(&invitee_id) {
            channel.member_ids.push(invitee_id);
        }

        Ok(())
    }

    async fn send_message(
        self,
        _: context::Context,
        id_token: String,
        channel_id: String,
        content: Vec<u8>,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;
        let now = Utc::now();

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if !channel.member_ids.contains(&authenticated_user.user_id) {
            return Err("Not a channel member".to_string());
        }

        let mut all_messages = self.messages.write().await;
        let next_cursor = all_messages
            .get(&channel_id)
            .map_or(0, |ms| ms.len() as u64);

        let message = Message {
            cursor: next_cursor,
            sender_id: authenticated_user.user_id.clone(),
            content,
            timestamp: now,
        };
        all_messages
            .entry(channel_id)
            .or_insert_with(Vec::new)
            .push(message);
        Ok(())
    }

    async fn get_messages(
        self,
        _: context::Context,
        id_token: String,
        channel_id: String,
        pagination: Pagination,
    ) -> Result<Vec<Message>, String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if !channel.member_ids.contains(&authenticated_user.user_id) {
            return Err("Not a channel member".to_string());
        }

        let all_messages = self.messages.read().await;
        if let Some(messages) = all_messages.get(&channel_id) {
            Ok(match pagination.direction {
                PaginationDirection::After(after) => messages
                    .iter()
                    .skip(after.map_or(0, |n| n + 1) as usize)
                    .take(pagination.limit as usize),
                PaginationDirection::Before(before) => messages
                    .iter()
                    .skip((before - pagination.limit) as usize)
                    .take(pagination.limit as usize),
            }
            .cloned()
            .collect())
        } else {
            Ok(vec![])
        }
    }

    async fn start_voice_call(
        self,
        _: context::Context,
        id_token: String,
        channel_id: String,
    ) -> Result<VoiceCallSession, String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if !channel.member_ids.contains(&authenticated_user.user_id) {
            return Err("Not a channel member".to_string());
        }

        let mut voice_calls = self.voice_calls.write().await;

        if voice_calls.contains_key(&channel_id) {
            return Err("Voice call already exists for this channel".to_string());
        }

        let session = VoiceCallSession {
            session_id: Uuid::new_v4().to_string(),
            channel_id: channel_id.clone(),
            participants: vec![authenticated_user.user_id.clone()],
            encryption_keys: vec![EncryptionKey {
                sender_id: authenticated_user.user_id,
                key: vec![0u8; 32], // TODO: Generate and encrypt actual key
            }],
        };

        voice_calls.insert(channel_id, session.clone());
        Ok(session)
    }

    async fn get_voice_call(
        self,
        _: context::Context,
        id_token: String,
        channel_id: String,
    ) -> Result<Option<VoiceCallSession>, String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if !channel.member_ids.contains(&authenticated_user.user_id) {
            return Err("Not a channel member".to_string());
        }

        Ok(self.voice_calls.read().await.get(&channel_id).cloned())
    }

    async fn end_voice_call(
        self,
        _: context::Context,
        id_token: String,
        channel_id: String,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if !channel.member_ids.contains(&authenticated_user.user_id) {
            return Err("Not a channel member".to_string());
        }

        self.voice_calls.write().await.remove(&channel_id);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;

    let server = ChatServer::new();

    tracing::info!("Starting heek-chat-server on {}", addr);

    let mut listener =
        tarpc::serde_transport::tcp::listen(&addr, tokio_serde::formats::Cbor::default).await?;

    tracing::info!("Listening for connections");

    while let Some(Ok(transport)) = listener.next().await {
        let server = server.clone();
        tokio::spawn(async move {
            let channel = server::BaseChannel::with_defaults(transport);
            let handler = channel.execute(server.serve());
            tokio::pin!(handler);
            while let Some(fut) = handler.next().await {
                fut.await;
            }
        });
    }

    Ok(())
}
