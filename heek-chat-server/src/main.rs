use futures::StreamExt;
use heek_chat_lib::{
    Channel, ChannelVisibility, ChatService, EncryptionKey, TextMessage, User, UserAuth,
    VoiceCallSession,
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

#[derive(Clone)]
struct ChatServer {
    // In-memory storage for now (will be replaced with PostgreSQL)
    users: Arc<RwLock<HashMap<String, User>>>,
    channels: Arc<RwLock<HashMap<String, Channel>>>,
    messages: Arc<RwLock<HashMap<String, Vec<TextMessage>>>>,
    voice_calls: Arc<RwLock<HashMap<String, VoiceCallSession>>>,
}

impl ChatServer {
    fn new() -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            channels: Arc::new(RwLock::new(HashMap::new())),
            messages: Arc::new(RwLock::new(HashMap::new())),
            voice_calls: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl ChatService for ChatServer {
    async fn register_user(
        self,
        _: context::Context,
        auth: UserAuth,
        username: String,
        nickname: String,
    ) -> Result<User, String> {
        // TODO: Verify Firebase token
        let user = User {
            user_id: auth.user_id.clone(),
            username,
            nickname,
            profile_image: None,
        };

        self.users.write().await.insert(auth.user_id, user.clone());
        Ok(user)
    }

    async fn get_user(self, _: context::Context, user_id: String) -> Result<User, String> {
        self.users
            .read()
            .await
            .get(&user_id)
            .cloned()
            .ok_or_else(|| "User not found".to_string())
    }

    async fn update_profile(
        self,
        _: context::Context,
        user_id: String,
        nickname: Option<String>,
        profile_image: Option<Vec<u8>>,
    ) -> Result<(), String> {
        let mut users = self.users.write().await;
        let user = users.get_mut(&user_id).ok_or("User not found")?;

        if let Some(nick) = nickname {
            user.nickname = nick;
        }
        if let Some(img) = profile_image {
            if img.len() > 5 * 1024 * 1024 {
                return Err("Profile image too large (max 5MiB)".to_string());
            }
            user.profile_image = Some(img);
        }

        Ok(())
    }

    async fn create_channel(
        self,
        _: context::Context,
        creator_id: String,
        name: String,
        visibility: ChannelVisibility,
    ) -> Result<Channel, String> {
        let channel = Channel {
            channel_id: Uuid::new_v4().to_string(),
            name,
            visibility,
            member_ids: vec![creator_id],
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
        user_id: String,
    ) -> Result<Vec<Channel>, String> {
        let channels = self.channels.read().await;
        Ok(channels
            .values()
            .filter(|c| {
                c.member_ids.contains(&user_id) || c.visibility == ChannelVisibility::Public
            })
            .cloned()
            .collect())
    }

    async fn join_channel(
        self,
        _: context::Context,
        user_id: String,
        channel_id: String,
    ) -> Result<(), String> {
        let mut channels = self.channels.write().await;
        let channel = channels.get_mut(&channel_id).ok_or("Channel not found")?;

        if channel.visibility == ChannelVisibility::Private
            && !channel.member_ids.contains(&user_id)
        {
            return Err("Cannot join private channel without invitation".to_string());
        }

        if !channel.member_ids.contains(&user_id) {
            channel.member_ids.push(user_id);
        }

        Ok(())
    }

    async fn leave_channel(
        self,
        _: context::Context,
        user_id: String,
        channel_id: String,
    ) -> Result<(), String> {
        let mut channels = self.channels.write().await;
        let channel = channels.get_mut(&channel_id).ok_or("Channel not found")?;

        channel.member_ids.retain(|id| id != &user_id);
        Ok(())
    }

    async fn invite_to_channel(
        self,
        _: context::Context,
        inviter_id: String,
        invitee_id: String,
        channel_id: String,
    ) -> Result<(), String> {
        let mut channels = self.channels.write().await;
        let channel = channels.get_mut(&channel_id).ok_or("Channel not found")?;

        if !channel.member_ids.contains(&inviter_id) {
            return Err("Inviter is not a channel member".to_string());
        }

        if channel.visibility == ChannelVisibility::Private
            && !channel.member_ids.contains(&inviter_id)
        {
            return Err("Only members can invite to private channels".to_string());
        }

        if !channel.member_ids.contains(&invitee_id) {
            channel.member_ids.push(invitee_id);
        }

        Ok(())
    }

    async fn send_message(self, _: context::Context, message: TextMessage) -> Result<(), String> {
        let mut messages = self.messages.write().await;
        messages
            .entry(message.channel_id.clone())
            .or_insert_with(Vec::new)
            .push(message);
        Ok(())
    }

    async fn get_messages(
        self,
        _: context::Context,
        channel_id: String,
        _user_id: String,
        limit: u32,
    ) -> Result<Vec<TextMessage>, String> {
        let messages = self.messages.read().await;
        Ok(messages
            .get(&channel_id)
            .map(|msgs| {
                msgs.iter()
                    .rev()
                    .take(limit as usize)
                    .rev()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn start_voice_call(
        self,
        _: context::Context,
        channel_id: String,
        initiator_id: String,
    ) -> Result<VoiceCallSession, String> {
        let mut voice_calls = self.voice_calls.write().await;

        if voice_calls.contains_key(&channel_id) {
            return Err("Voice call already exists for this channel".to_string());
        }

        let session = VoiceCallSession {
            session_id: Uuid::new_v4().to_string(),
            channel_id: channel_id.clone(),
            participants: vec![initiator_id.clone()],
            encryption_keys: vec![EncryptionKey {
                sender_id: initiator_id,
                key: vec![0u8; 32], // TODO: Generate and encrypt actual key
            }],
        };

        voice_calls.insert(channel_id, session.clone());
        Ok(session)
    }

    async fn get_voice_call(
        self,
        _: context::Context,
        channel_id: String,
    ) -> Result<Option<VoiceCallSession>, String> {
        Ok(self.voice_calls.read().await.get(&channel_id).cloned())
    }

    async fn end_voice_call(
        self,
        _: context::Context,
        channel_id: String,
        _user_id: String,
    ) -> Result<(), String> {
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
        tarpc::serde_transport::tcp::listen(&addr, tokio_serde::formats::Cbor::default)
            .await?;

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
