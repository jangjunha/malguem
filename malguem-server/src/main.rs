use chrono::{DateTime, TimeDelta, Utc};
use futures::{StreamExt, future};
use malguem_lib::{
    Channel, ChannelID, ChannelType, ChatService, Event, Message, Pagination, PaginationDirection,
    RTCSession, User, UserID,
};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tarpc::{
    context,
    server::{self, Channel as TarpcChannel},
};
use tokio::sync::{RwLock, mpsc};
use uuid::{Uuid, uuid};

use firebase_verify::verify_id_token;

use crate::connection::listen;

mod connection;
mod firebase_verify;

type ConnectionID = Uuid;

struct Invitation {
    pub inviter_id: Option<String>,
    pub invited_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

struct UserWithCredentials {
    pub user: User,
    pub firebase_uid: String,
}

type InvitationToken = String;

#[derive(Clone)]
struct ChatServer {
    current_connection_id: Option<ConnectionID>,

    // In-memory storage for now (will be replaced with PostgreSQL)
    invitations: Arc<RwLock<HashMap<InvitationToken, Invitation>>>,
    users: Arc<RwLock<HashMap<UserID, UserWithCredentials>>>,
    channels: Arc<RwLock<HashMap<ChannelID, Channel>>>,
    messages: Arc<RwLock<HashMap<ChannelID, Vec<Message>>>>,
    rtc_sessions: Arc<RwLock<HashMap<ChannelID, RTCSession>>>,

    user_connections: Arc<RwLock<HashMap<UserID, HashSet<ConnectionID>>>>,
    event_senders: Arc<RwLock<HashMap<ConnectionID, mpsc::UnboundedSender<Event>>>>,
}

impl ChatServer {
    fn new() -> Self {
        Self {
            current_connection_id: None,
            user_connections: Arc::new(RwLock::new(HashMap::new())),
            event_senders: Arc::new(RwLock::new(HashMap::new())),
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
            users: Arc::new(RwLock::new({
                let mut users = HashMap::new();
                // FIXME:
                users.insert(
                    uuid!("00000000-0000-0000-0000-000000000000"),
                    UserWithCredentials {
                        user: User {
                            user_id: uuid!("00000000-0000-0000-0000-000000000000"),
                            username: "aaaa".to_string(),
                            profile_image: None,
                        },
                        firebase_uid: "Bovit53RRDNLiIxdI3qRsXJPdtL2".to_string(),
                    },
                );
                users.insert(
                    uuid!("00000000-0000-0000-0000-000000000001"),
                    UserWithCredentials {
                        user: User {
                            user_id: uuid!("00000000-0000-0000-0000-000000000001"),
                            username: "bbbb".to_string(),
                            profile_image: None,
                        },
                        firebase_uid: "t4BGVVpEF3UdM3EBocjfnGGLfxy1".to_string(),
                    },
                );
                users.insert(
                    uuid!("00000000-0000-0000-0000-000000000002"),
                    UserWithCredentials {
                        user: User {
                            user_id: uuid!("00000000-0000-0000-0000-000000000002"),
                            username: "cccc".to_string(),
                            profile_image: None,
                        },
                        firebase_uid: "JqaAtpBd3cddYjokqEQKMhD53SF2".to_string(),
                    },
                );
                users
            })),
            channels: Arc::new(RwLock::new({
                let mut channels = HashMap::new();
                channels.insert(
                    uuid!("00000000-0000-0000-0000-000000000000"),
                    Channel {
                        channel_id: uuid!("00000000-0000-0000-0000-000000000000"),
                        name: "random".to_string(),
                        r#type: ChannelType::Text,
                    },
                );
                channels.insert(
                    uuid!("00000000-0000-0000-0000-000000000001"),
                    Channel {
                        channel_id: uuid!("00000000-0000-0000-0000-000000000001"),
                        name: "voice".to_string(),
                        r#type: ChannelType::RTC,
                    },
                );
                channels
            })),
            messages: Arc::new(RwLock::new({
                let mut messages = HashMap::new();
                messages.insert(
                    uuid!("00000000-0000-0000-0000-000000000000"),
                    vec![Message {
                        cursor: 0,
                        sender_id: uuid!("00000000-0000-0000-0000-000000000000"),
                        content: vec![],
                        timestamp: Utc::now(),
                    }],
                );
                messages
            })),
            rtc_sessions: Arc::new(RwLock::new(HashMap::new())),
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
            let user = user_with_credentials.user.clone();

            // Try to register event sender from the first available pending connection
            // This is a simplified approach: first auth call from a connection registers that connection's event_tx
            self.user_connections
                .write()
                .await
                .entry(user.user_id.clone())
                .or_insert_with(HashSet::new)
                .insert(
                    self.current_connection_id
                        .expect("current_connection_id is not set"),
                );

            Ok(user)
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
    ) -> Result<UserID, String> {
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

        let user_id = Uuid::new_v4();
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
        user_id: UserID,
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
        r#type: ChannelType,
    ) -> Result<Channel, String> {
        let _ = self.authenticate(&id_token).await?;

        let channel = Channel {
            channel_id: Uuid::new_v4(),
            name,
            r#type,
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
        let _ = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        Ok(channels.values().cloned().collect())
    }

    async fn send_message(
        self,
        _: context::Context,
        id_token: String,
        channel_id: ChannelID,
        content: Vec<u8>,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;
        let now = Utc::now();

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::Text {
            return Err("Cannot send message to non-text channel".to_string());
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
        channel_id: ChannelID,
        pagination: Pagination,
    ) -> Result<Vec<Message>, String> {
        let _ = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::Text {
            return Err("Cannot get messages from non-text channel".to_string());
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

    async fn join_rtc_session(
        self,
        _: context::Context,
        id_token: String,
        channel_id: ChannelID,
    ) -> Result<RTCSession, String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::RTC {
            return Err("Operation not allowed on non-rtc channel".to_string());
        }

        let mut sessions = self.rtc_sessions.write().await;

        // Get or create session
        let (session, is_new_participant) = if let Some(session) = sessions.get_mut(&channel_id) {
            let is_new = !session.participants.contains(&authenticated_user.user_id);
            session
                .participants
                .insert(authenticated_user.user_id.clone());
            (session.clone(), is_new)
        } else {
            let mut session = RTCSession::new(&channel_id);
            session
                .participants
                .insert(authenticated_user.user_id.clone());
            sessions.insert(channel_id.clone(), session.clone());
            (session.clone(), true)
        };

        // Broadcast UserJoined event to all participants in the channel
        if is_new_participant {
            let event = Event::RTCSession {
                channel_id: channel_id.clone(),
                event: malguem_lib::RTCSessionEvent::UserJoined {
                    user_id: authenticated_user.user_id.clone(),
                },
            };
            let event_senders = self.event_senders.read().await;
            let user_connections = self.user_connections.read().await;

            for participant_id in &session.participants {
                if participant_id == &authenticated_user.user_id {
                    continue;
                }
                // Send event to all connections for this participant
                if let Some(connection_ids) = user_connections.get(participant_id) {
                    for connection_id in connection_ids {
                        if let Some(sender) = event_senders.get(connection_id) {
                            let _ = sender.send(event.clone());
                        }
                    }
                }
            }
        }

        Ok(session)
    }

    async fn leave_rtc_session(
        self,
        _: context::Context,
        id_token: String,
        channel_id: ChannelID,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::RTC {
            return Err("Operation not allowed on non-rtc channel".to_string());
        }

        let mut sessions = self.rtc_sessions.write().await;
        if let Some(session) = sessions.get_mut(&channel_id) {
            let other_participants = session.participants.clone();
            session.participants.remove(&authenticated_user.user_id);
            if session.participants.is_empty() {
                sessions.remove(&channel_id);
            }

            // Broadcast UserJoined event to all participants in the channel
            let event = Event::RTCSession {
                channel_id: channel_id.clone(),
                event: malguem_lib::RTCSessionEvent::UserLeft {
                    user_id: authenticated_user.user_id.clone(),
                },
            };
            let event_senders = self.event_senders.read().await;
            let user_connections = self.user_connections.read().await;

            for participant_id in other_participants {
                // Send event to all connections for this participant
                if let Some(connection_ids) = user_connections.get(&participant_id) {
                    for connection_id in connection_ids {
                        if let Some(sender) = event_senders.get(connection_id) {
                            let _ = sender.send(event.clone());
                        }
                    }
                }
            }
        } else {
            return Err("Channel not found".to_string());
        };

        Ok(())
    }

    async fn offer_rtc_connection(
        self,
        _: context::Context,
        id_token: String,
        channel_id: ChannelID,
        target_user_id: UserID,
        sdp: String,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::RTC {
            return Err("Operation not allowed on non-rtc channel".to_string());
        }

        let event = Event::RTCSession {
            channel_id: channel_id.clone(),
            event: malguem_lib::RTCSessionEvent::OfferReceived {
                from_user_id: authenticated_user.user_id.clone(),
                sdp,
            },
        };
        for target_user_conn_id in self
            .user_connections
            .read()
            .await
            .get(&target_user_id)
            .iter()
            .flat_map(|s| s.iter())
        {
            if let Some(sender) = self.event_senders.read().await.get(target_user_conn_id) {
                let _ = sender.send(event.clone());
            }
        }

        Ok(())
    }

    async fn answer_rtc_connection(
        self,
        _: context::Context,
        id_token: String,
        channel_id: ChannelID,
        target_user_id: UserID,
        sdp: String,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::RTC {
            return Err("Operation not allowed on non-rtc channel".to_string());
        }

        let event = Event::RTCSession {
            channel_id: channel_id.clone(),
            event: malguem_lib::RTCSessionEvent::AnswerReceived {
                from_user_id: authenticated_user.user_id.clone(),
                sdp,
            },
        };
        for target_user_conn_id in self
            .user_connections
            .read()
            .await
            .get(&target_user_id)
            .iter()
            .flat_map(|s| s.iter())
        {
            if let Some(sender) = self.event_senders.read().await.get(target_user_conn_id) {
                let _ = sender.send(event.clone());
            }
        }

        Ok(())
    }

    async fn ice_candidate_rtc_connection(
        self,
        _: context::Context,
        id_token: String,
        channel_id: ChannelID,
        target_user_id: UserID,
        candidate: String,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::RTC {
            return Err("Operation not allowed on non-rtc channel".to_string());
        }

        let event = Event::RTCSession {
            channel_id: channel_id.clone(),
            event: malguem_lib::RTCSessionEvent::IceCandidateReceived {
                from_user_id: authenticated_user.user_id.clone(),
                candidate,
            },
        };
        for target_user_conn_id in self
            .user_connections
            .read()
            .await
            .get(&target_user_id)
            .iter()
            .flat_map(|s| s.iter())
        {
            if let Some(sender) = self.event_senders.read().await.get(target_user_conn_id) {
                let _ = sender.send(event.clone());
            }
        }

        Ok(())
    }

    async fn start_screen_share(
        self,
        _: context::Context,
        id_token: String,
        channel_id: ChannelID,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::RTC {
            return Err("Operation not allowed on non-rtc channel".to_string());
        }

        let mut sessions = self.rtc_sessions.write().await;
        let session = sessions.get_mut(&channel_id).ok_or("Not in RTC session")?;

        // Check if user is already sharing screen
        if session.screen_sharers.contains(&authenticated_user.user_id) {
            return Err("Already sharing screen".to_string());
        }

        // Add to screen sharers
        session
            .screen_sharers
            .insert(authenticated_user.user_id.clone());

        // Broadcast ScreenShareStarted event to all participants in the channel
        let event = Event::RTCSession {
            channel_id: channel_id.clone(),
            event: malguem_lib::RTCSessionEvent::ScreenShareStarted {
                user_id: authenticated_user.user_id.clone(),
            },
        };
        let event_senders = self.event_senders.read().await;
        let user_connections = self.user_connections.read().await;

        for participant_id in &session.participants {
            if participant_id == &authenticated_user.user_id {
                continue;
            }
            // Send event to all connections for this participant
            if let Some(connection_ids) = user_connections.get(participant_id) {
                for connection_id in connection_ids {
                    if let Some(sender) = event_senders.get(connection_id) {
                        let _ = sender.send(event.clone());
                    }
                }
            }
        }

        Ok(())
    }

    async fn stop_screen_share(
        self,
        _: context::Context,
        id_token: String,
        channel_id: ChannelID,
    ) -> Result<(), String> {
        let authenticated_user = self.authenticate(&id_token).await?;

        let channels = self.channels.read().await;
        let channel = channels.get(&channel_id).ok_or("Channel not found")?;
        if channel.r#type != ChannelType::RTC {
            return Err("Operation not allowed on non-rtc channel".to_string());
        }

        let mut sessions = self.rtc_sessions.write().await;
        let session = sessions.get_mut(&channel_id).ok_or("Not in RTC session")?;

        // Check if user is sharing screen
        if !session.screen_sharers.contains(&authenticated_user.user_id) {
            return Err("Not sharing screen".to_string());
        }

        // Remove from screen sharers
        session.screen_sharers.remove(&authenticated_user.user_id);

        // Broadcast ScreenShareStopped event to all participants in the channel
        let event = Event::RTCSession {
            channel_id: channel_id.clone(),
            event: malguem_lib::RTCSessionEvent::ScreenShareStopped {
                user_id: authenticated_user.user_id.clone(),
            },
        };
        let event_senders = self.event_senders.read().await;
        let user_connections = self.user_connections.read().await;

        for participant_id in &session.participants {
            if participant_id == &authenticated_user.user_id {
                continue;
            }
            // Send event to all connections for this participant
            if let Some(connection_ids) = user_connections.get(participant_id) {
                for connection_id in connection_ids {
                    if let Some(sender) = event_senders.get(connection_id) {
                        let _ = sender.send(event.clone());
                    }
                }
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .with_level(true)
        .with_writer(std::io::stdout)
        .init();

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    let incoming = listen(addr).await?;
    tracing::info!("Listening on: {}", addr);

    let server = ChatServer::new();

    incoming
        .filter_map(|r: Result<_, std::io::Error>| future::ready(r.ok()))
        .map(|conn: crate::connection::Connection| {
            let connection_id = Uuid::new_v4();
            let event_senders = server.event_senders.clone();
            let user_connections = server.user_connections.clone();

            // Spawn task to register event sender
            let event_senders_for_register = event_senders.clone();
            tokio::spawn(async move {
                event_senders_for_register
                    .write()
                    .await
                    .insert(connection_id.clone(), conn.event_tx);
            });

            // Spawn task to handle cleanup on disconnect
            let disconnect_rx = conn.disconnect_rx;
            tokio::spawn(async move {
                // Wait for disconnect signal
                let _ = disconnect_rx.await;

                tracing::info!("Connection {} disconnected, cleaning up", connection_id);

                event_senders.write().await.remove(&connection_id);
                user_connections
                    .write()
                    .await
                    .iter_mut()
                    .for_each(|(_, conns)| {
                        conns.remove(&connection_id);
                    });
            });

            (connection_id, conn.rpc)
        })
        .map(|(connection_id, transport)| {
            (connection_id, server::BaseChannel::with_defaults(transport))
        })
        // TODO: Add rate limiting per IP
        // .max_channels_per_key(1, |t| { ... })
        .map(|(connection_id, channel)| {
            let server = {
                let mut server = server.clone();
                server.current_connection_id = Some(connection_id.clone());
                server
            };
            tokio::spawn(
                channel
                    .execute(server.serve())
                    // Handle all requests concurrently.
                    .for_each(|response| async move {
                        tokio::spawn(response);
                    }),
            )
        })
        // Max 32 channels.
        .buffer_unordered(32)
        .for_each(|_| async {})
        .await;

    Ok(())
}
