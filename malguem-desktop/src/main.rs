use eframe::egui;
use malguem_lib::{
    Channel, ChannelID, ChannelType, ChatServiceClient, Event, Message, User, UserID,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

mod firebase_auth;
use firebase_auth::FirebaseAuth;

use crate::audio_capture::AudioCapture;
use crate::connection::Connection;
use crate::rtc::RTCSessionManager;
use crate::screen_capture::ScreenCapture;
use crate::video_playback::VideoPlayback;

mod audio_capture;
mod audio_playback;
mod codec;
mod connection;
mod rtc;
mod screen_capture;
mod video_playback;

struct CurrentMessageChannel {
    channel_id: ChannelID,
    messages: Vec<Message>,
}

impl CurrentMessageChannel {
    fn new(channel_id: &ChannelID) -> Self {
        Self {
            channel_id: channel_id.clone(),
            messages: vec![],
        }
    }
}

struct CurrentRTCSession {
    channel_id: ChannelID,
    manager: Arc<RTCSessionManager>,
    #[allow(dead_code)]
    audio_capture: Option<AudioCapture>,
    screen_capture: Option<ScreenCapture>,
    video_playbacks: HashMap<UserID, Arc<VideoPlayback>>,
}

impl CurrentRTCSession {
    fn new(
        channel_id: &ChannelID,
        manager: Arc<RTCSessionManager>,
        audio_capture: Option<AudioCapture>,
    ) -> Self {
        Self {
            channel_id: channel_id.clone(),
            manager,
            audio_capture,
            screen_capture: None,
            video_playbacks: HashMap::new(),
        }
    }
}

struct App {
    // Connection state
    connected: bool,
    server_address: String,
    connection_error: Option<String>,
    screen_share_error: Option<String>,

    // Firebase Auth
    firebase_auth: FirebaseAuth,

    // User state
    current_user: Option<User>,
    firebase_token: Option<String>,

    // UI state
    invitation_token_input: String,
    server_key: String,

    email_input: String,
    password_input: String,
    username_input: String,
    message_input: String,

    // OAuth
    oauth_receiver: Option<std::sync::mpsc::Receiver<String>>,
    oauth_redirect_uri: Option<String>,

    // Data
    channels: Vec<Channel>,
    current_message_channel: Option<CurrentMessageChannel>,
    current_rtc_session: Option<CurrentRTCSession>,

    // Runtime
    runtime: Arc<tokio::runtime::Runtime>,
    rpc: Option<ChatServiceClient>,
    event_rx: Option<mpsc::UnboundedReceiver<Event>>,

    // Video playback
    video_playback_tx: mpsc::UnboundedSender<Arc<VideoPlayback>>,
    video_playback_rx: mpsc::UnboundedReceiver<Arc<VideoPlayback>>,
}

impl Default for App {
    fn default() -> Self {
        let firebase_auth = FirebaseAuth::new();
        let (video_playback_tx, video_playback_rx) = mpsc::unbounded_channel();

        Self {
            connected: false,
            server_address: "ws://127.0.0.1:8080".to_string(),
            connection_error: None,
            screen_share_error: None,
            firebase_auth,
            current_user: None,
            firebase_token: None,
            invitation_token_input: String::new(),
            server_key: String::new(),
            email_input: "a@heek.kr".to_string(),   // FIXME:
            password_input: "qwer1234".to_string(), // FIXME:
            username_input: String::new(),
            message_input: String::new(),
            oauth_receiver: None,
            oauth_redirect_uri: None,
            channels: Vec::new(),
            current_message_channel: None,
            current_rtc_session: None,
            runtime: Arc::new(
                tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"),
            ),
            rpc: None,
            event_rx: None,
            video_playback_tx,
            video_playback_rx,
        }
    }
}

impl App {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Default::default()
    }

    fn authenticate_with_email_password(&mut self) {
        let email = self.email_input.clone();
        let password = self.password_input.clone();
        let firebase_auth = self.firebase_auth.clone();
        let runtime = self.runtime.clone();

        self.connection_error = None;

        match runtime.block_on(async move {
            firebase_auth
                .sign_in_with_email_password(email, password)
                .await
        }) {
            Ok(auth_response) => {
                self.firebase_token = Some(auth_response.id_token.clone());
                tracing::info!("Firebase authentication successful");
                // Now connect to chat server
                self.connect_to_server_with_firebase_token(auth_response.id_token);
            }
            Err(e) => {
                self.connection_error = Some(format!("Authentication failed: {}", e));
                tracing::error!("Firebase auth failed: {:?}", self.connection_error);
            }
        }
    }

    fn start_google_login(&mut self) {
        match firebase_auth::start_oauth_callback_server() {
            Ok((redirect_uri, receiver)) => {
                self.oauth_redirect_uri = Some(redirect_uri.clone());
                self.oauth_receiver = Some(receiver);

                let oauth_url = self.firebase_auth.get_google_oauth_url(&redirect_uri);

                // Open browser
                if let Err(e) = webbrowser::open(&oauth_url) {
                    self.connection_error = Some(format!("Failed to open browser: {}", e));
                } else {
                    tracing::info!("Opened browser for Google OAuth");
                }
            }
            Err(e) => {
                self.connection_error = Some(format!("Failed to start OAuth server: {}", e));
            }
        }
    }

    fn check_oauth_callback(&mut self) {
        if let Some(receiver) = &self.oauth_receiver {
            if let Ok(token) = receiver.try_recv() {
                let redirect_uri = self.oauth_redirect_uri.take().unwrap_or_default();
                let firebase_auth = self.firebase_auth.clone();
                let runtime = self.runtime.clone();

                self.oauth_receiver = None;

                match runtime.block_on(async move {
                    firebase_auth
                        .sign_in_with_google_oauth(token, redirect_uri)
                        .await
                }) {
                    Ok(auth_response) => {
                        self.firebase_token = Some(auth_response.id_token.clone());
                        tracing::info!("Google authentication successful");
                        self.connect_to_server_with_firebase_token(auth_response.id_token);
                    }
                    Err(e) => {
                        self.connection_error = Some(format!("Google auth failed: {}", e));
                    }
                }
            }
        }
    }

    fn fetch_channels(&mut self) {
        if let (Some(client), Some(firebase_token)) = (&self.rpc, &self.firebase_token) {
            let runtime = self.runtime.clone();
            let client = client.clone();
            let firebase_token = firebase_token.clone();

            match runtime.block_on(async move {
                client
                    .list_channels(tarpc::context::current(), firebase_token)
                    .await
                    .map_err(|e| format!("RPC failed: {}", e))?
            }) {
                Ok(channels) => {
                    self.channels = channels;
                    tracing::info!("Fetched {} channels", self.channels.len());
                }
                Err(e) => {
                    tracing::error!("Failed to fetch channels: {}", e);
                }
            }
        }
    }

    fn join_rtc_session(&mut self, channel_id: ChannelID) {
        if let (Some(client), Some(firebase_token), Some(current_user)) =
            (&self.rpc, &self.firebase_token, &self.current_user)
        {
            let runtime = self.runtime.clone();
            let client = client.clone();
            let client_for_manager = client.clone();
            let channel_id_clone = channel_id.clone();
            let firebase_token = firebase_token.clone();
            let firebase_token_for_manager = firebase_token.clone();
            let current_user_id = current_user.user_id.clone();

            match runtime.block_on(async move {
                client
                    .join_rtc_session(tarpc::context::current(), firebase_token, channel_id_clone)
                    .await
            }) {
                Ok(Ok(rtc_session)) => {
                    // Start audio capture
                    let (audio_capture, audio_track) =
                        match AudioCapture::start(self.runtime.clone()) {
                            Ok(capture) => {
                                let track = capture.track();
                                tracing::info!("Audio capture started successfully");
                                (Some(capture), Some(track))
                            }
                            Err(e) => {
                                tracing::error!("Failed to start audio capture: {}", e);
                                (None, None)
                            }
                        };

                    // Create RTCSessionManager with audio track and video playback channel
                    let manager = Arc::new(RTCSessionManager::new(
                        channel_id.clone(),
                        current_user_id,
                        client_for_manager,
                        firebase_token_for_manager,
                        self.runtime.clone(),
                        audio_track,
                        self.video_playback_tx.clone(),
                    ));

                    // Start ICE candidate sender task
                    manager.start_ice_candidate_sender();

                    // Handle existing participants (users who were already in the session)
                    for participant_id in &rtc_session.participants {
                        let manager_clone = Arc::clone(&manager);
                        let participant_id = participant_id.clone();
                        let runtime_clone = self.runtime.clone();

                        runtime_clone.spawn(async move {
                            if let Err(e) = manager_clone.handle_user_joined(participant_id).await {
                                tracing::error!("Failed to handle existing participant: {}", e);
                            }
                        });
                    }

                    self.current_rtc_session =
                        Some(CurrentRTCSession::new(&channel_id, manager, audio_capture));
                    tracing::info!(
                        "Joined RTC session with {} participants",
                        rtc_session.participants.len()
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!("Failed to join rtc session: {}", e);
                }
                Err(e) => {
                    tracing::error!("Failed to join rtc session: {}", e);
                }
            }
        }
    }

    fn leave_rtc_session(&mut self) {
        if let (Some(client), Some(firebase_token), Some(session)) =
            (&self.rpc, &self.firebase_token, &self.current_rtc_session)
        {
            let runtime = self.runtime.clone();
            let client = client.clone();
            let firebase_token = firebase_token.clone();
            let manager = Arc::clone(&session.manager);
            let channel_id = session.channel_id.clone();

            let _ = runtime.block_on(async move {
                // Close all WebRTC connections
                if let Err(e) = manager.close_all().await {
                    tracing::error!("Failed to close WebRTC connections: {}", e);
                }

                // Notify server
                client
                    .leave_rtc_session(tarpc::context::current(), firebase_token, channel_id)
                    .await
            });

            self.current_rtc_session = None;
            tracing::info!("Left RTC session");
        }
    }

    fn start_screen_share(&mut self) {
        // Clear previous error
        self.screen_share_error = None;

        if let (Some(client), Some(firebase_token), Some(session)) = (
            &self.rpc,
            &self.firebase_token,
            &mut self.current_rtc_session,
        ) {
            let runtime = self.runtime.clone();
            let client = client.clone();
            let firebase_token = firebase_token.clone();
            let manager = Arc::clone(&session.manager);
            let channel_id = session.channel_id.clone();

            // Start screen capture
            match ScreenCapture::start(runtime.clone()) {
                Ok(screen_capture) => {
                    let screen_track = screen_capture.track();

                    // Add screen track to all peer connections
                    let result = runtime.block_on(async move {
                        if let Err(e) = manager.set_screen_track(screen_track).await {
                            return Err(format!("Failed to set screen track: {}", e));
                        }

                        // Notify server
                        client
                            .start_screen_share(
                                tarpc::context::current(),
                                firebase_token,
                                channel_id,
                            )
                            .await
                            .map_err(|e| format!("RPC error: {}", e))?
                    });

                    match result {
                        Ok(()) => {
                            session.screen_capture = Some(screen_capture);
                            tracing::info!("Started screen sharing");
                        }
                        Err(e) => {
                            let error_msg = format!("Failed to start screen sharing: {}", e);
                            tracing::error!("{}", error_msg);
                            self.screen_share_error = Some(error_msg);
                        }
                    }
                }
                Err(e) => {
                    let error_msg = format!("Failed to initialize screen capture: {}", e);
                    tracing::error!("{}", error_msg);
                    self.screen_share_error = Some(error_msg);
                }
            }
        }
    }

    fn stop_screen_share(&mut self) {
        if let (Some(client), Some(firebase_token), Some(session)) = (
            &self.rpc,
            &self.firebase_token,
            &mut self.current_rtc_session,
        ) {
            let runtime = self.runtime.clone();
            let client = client.clone();
            let firebase_token = firebase_token.clone();
            let manager = Arc::clone(&session.manager);
            let channel_id = session.channel_id.clone();

            let _ = runtime.block_on(async move {
                // Remove screen track from peer connections
                if let Err(e) = manager.remove_screen_track().await {
                    tracing::error!("Failed to remove screen track: {}", e);
                }

                // Notify server
                client
                    .stop_screen_share(tarpc::context::current(), firebase_token, channel_id)
                    .await
            });

            // Drop screen capture (will stop capture thread)
            session.screen_capture = None;
            tracing::info!("Stopped screen sharing");
        }
    }

    fn connect_to_server_with_firebase_token(&mut self, firebase_token: String) {
        let addr = self.server_address.clone();
        let runtime = self.runtime.clone();
        let username = self.username_input.clone();
        let invitation_token = self.invitation_token_input.clone();

        self.connection_error = None;

        // Connect to server and register user
        match runtime.block_on(async move {
            let conn = Connection::establish(&addr).await?;
            let rpc = conn.rpc;
            let event_rx = conn.event_rx;

            // Join
            if !invitation_token.is_empty() {
                let _ = rpc
                    .join(
                        tarpc::context::current(),
                        firebase_token.clone(),
                        invitation_token.clone(),
                        username,
                    )
                    .await
                    .map_err(|e| format!("RPC failed: {}", e))?
                    .map_err(|e| format!("Join failed: {}", e))?;
            }

            let user = rpc
                .get_me(tarpc::context::current(), firebase_token.clone())
                .await
                .map_err(|e| format!("RPC failed: {}", e))?
                .map_err(|e| format!("Get me failed: {}", e))?;

            Ok::<(ChatServiceClient, mpsc::UnboundedReceiver<Event>, User), String>((
                rpc, event_rx, user,
            ))
        }) {
            Ok((rpc, event_rx, user)) => {
                self.rpc = Some(rpc);
                self.event_rx = Some(event_rx);
                self.current_user = Some(user);
                self.connected = true;
                tracing::info!("Connected to server successfully");

                // Fetch channel list
                self.fetch_channels();
            }
            Err(e) => {
                self.connection_error = Some(e);
                tracing::error!("Failed to connect: {:?}", self.connection_error);
            }
        }
    }

    fn render_login_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Malguem");

        ui.add_space(20.0);

        // Server address
        ui.horizontal(|ui| {
            ui.label("Server:");
            ui.text_edit_singleline(&mut self.server_address);
        });
        ui.horizontal(|ui| {
            ui.label("Server Key:");
            ui.text_edit_singleline(&mut self.server_key);
        });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Invitation Token:");
            ui.text_edit_singleline(&mut self.invitation_token_input);
        });

        // Email/Password login
        ui.horizontal(|ui| {
            ui.label("Email:");
            ui.text_edit_singleline(&mut self.email_input);
        });

        ui.horizontal(|ui| {
            ui.label("Password:");
            ui.add(egui::TextEdit::singleline(&mut self.password_input).password(true));
        });

        ui.horizontal(|ui| {
            ui.label("Username:");
            ui.text_edit_singleline(&mut self.username_input);
        });

        ui.add_space(10.0);

        if ui.button("Sign In with Email").clicked() {
            self.authenticate_with_email_password();
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);

        // Google login
        if ui.button("Sign In with Google").clicked() {
            self.start_google_login();
        }

        // Show waiting message if OAuth is in progress
        if self.oauth_receiver.is_some() {
            ui.add_space(10.0);
            ui.label("Waiting for Google authentication...");
        }

        // Show connection error if any
        if let Some(error) = &self.connection_error {
            ui.add_space(10.0);
            ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
        }
    }

    fn render_main_screen(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Malguem");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(user) = &self.current_user {
                    ui.label(format!("@{}", user.username));
                }
            });
        });

        ui.separator();

        // Main content area - use available height
        let available_height = ui.available_height();

        ui.horizontal(|ui| {
            // Channel list sidebar
            ui.vertical(|ui| {
                ui.set_width(200.0);
                ui.set_height(available_height);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    // Collect channels to avoid borrow checker issues
                    let text_channels: Vec<_> = self.channels.iter()
                        .filter(|c| matches!(c.r#type, ChannelType::Text))
                        .cloned()
                        .collect();
                    let rtc_channels: Vec<_> = self.channels.iter()
                        .filter(|c| matches!(c.r#type, ChannelType::RTC))
                        .cloned()
                        .collect();

                    // Text Channels Section
                    ui.heading("Text Channels");
                    ui.separator();

                    for channel in text_channels {
                        let selected = self.current_message_channel.as_ref().map(|c| c.channel_id)
                            == Some(channel.channel_id);

                        if ui.selectable_label(selected, format!("# {}", &channel.name)).clicked() {
                            self.current_message_channel =
                                Some(CurrentMessageChannel::new(&channel.channel_id));
                        }
                    }

                    ui.add_space(10.0);
                    if ui.button("+ Text Channel").clicked() {
                        // TODO: Show create text channel dialog
                    }

                    ui.add_space(20.0);

                    // RTC Channels Section
                    ui.heading("Voice Channels");
                    ui.separator();

                    for channel in rtc_channels {
                        let in_rtc_session =
                            self.current_rtc_session.as_ref().map(|s| s.channel_id)
                                == Some(channel.channel_id);

                        let rtc_button_text = if in_rtc_session { "🔊" } else { "🔇" };
                        let channel_label = format!("{} {}", rtc_button_text, &channel.name);

                        if ui.button(channel_label).clicked() {
                            if in_rtc_session {
                                self.leave_rtc_session();
                            } else {
                                self.join_rtc_session(channel.channel_id.clone());
                            }
                        }
                    }

                    ui.add_space(10.0);
                    if ui.button("+ Voice Channel").clicked() {
                        // TODO: Show create voice channel dialog
                    }
                });
            });

            ui.separator();

            // Screen sharing controls (only show when in RTC session)
            let is_in_rtc_session = self.current_rtc_session.is_some();
            let is_sharing = self.current_rtc_session.as_ref()
                .map(|s| s.screen_capture.is_some())
                .unwrap_or(false);

            if is_in_rtc_session {
                ui.vertical(|ui| {
                    ui.heading("Screen Sharing");

                    let button_text = if is_sharing {
                        "⏹ Stop Sharing Screen"
                    } else {
                        "▶ Share Screen"
                    };

                    if ui.button(button_text).clicked() {
                        if is_sharing {
                            self.stop_screen_share();
                        } else {
                            self.start_screen_share();
                        }
                    }

                    // Show screen share error if any
                    if let Some(error) = &self.screen_share_error {
                        ui.add_space(5.0);
                        ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
                        ui.label("Note: On macOS, you may need to grant Screen Recording permission in System Settings > Privacy & Security");
                    }
                });
                ui.separator();
            }

            ui.separator();

            // Chat area
            ui.vertical(|ui| {
                ui.set_height(available_height);

                if let Some(channel) = &self.current_message_channel {
                    // Messages
                    let message_area_height = available_height - 50.0; // Reserve space for input
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .max_height(message_area_height)
                        .show(ui, |ui| {
                            for msg in &channel.messages {
                                ui.horizontal(|ui| {
                                    ui.label(&msg.sender_id.to_string());
                                    ui.label(":");
                                    // TODO: Decrypt message content
                                    ui.label("<encrypted>");
                                });
                            }
                        });

                    // Message input
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let response = ui.text_edit_singleline(&mut self.message_input);

                        if ui.button("Send").clicked()
                            || (response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            // TODO: Send message
                            self.message_input.clear();
                        }
                    });
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label("Select a channel to start chatting");
                    });
                }
            });
        });
    }

    fn check_events(&mut self) {
        let mut events = Vec::new();
        if let Some(event_rx) = &mut self.event_rx {
            while let Ok(event) = event_rx.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            self.handle_event(event);
        }
    }

    fn check_video_playbacks(&mut self) {
        // Collect new video playbacks from the channel
        while let Ok(playback) = self.video_playback_rx.try_recv() {
            if let Some(session) = &mut self.current_rtc_session {
                tracing::info!("Adding video playback for user {}", playback.user_id);
                session
                    .video_playbacks
                    .insert(playback.user_id.clone(), playback);
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
        use malguem_lib::{Event, RTCSessionEvent};

        match event {
            Event::RTCSession { channel_id, event } => {
                tracing::info!("Received RTC event for channel {}: {:?}", channel_id, event);

                // Get the RTC session manager for this channel
                let manager = if let Some(session) = &self.current_rtc_session {
                    if session.channel_id == channel_id {
                        Some(Arc::clone(&session.manager))
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(manager) = manager {
                    let runtime = self.runtime.clone();

                    match event {
                        RTCSessionEvent::UserJoined { user_id } => {
                            tracing::info!(
                                "User {} joined RTC session in channel {}",
                                user_id,
                                channel_id
                            );

                            runtime.spawn(async move {
                                if let Err(e) = manager.handle_user_joined(user_id).await {
                                    tracing::error!("Failed to handle user joined: {}", e);
                                }
                            });
                        }
                        RTCSessionEvent::UserLeft { user_id } => {
                            tracing::info!(
                                "User {} left RTC session in channel {}",
                                user_id,
                                channel_id
                            );

                            runtime.spawn(async move {
                                if let Err(e) = manager.handle_user_left(user_id).await {
                                    tracing::error!("Failed to handle user left: {}", e);
                                }
                            });
                        }
                        RTCSessionEvent::OfferReceived { from_user_id, sdp } => {
                            tracing::info!(
                                "Received offer from {} in channel {}",
                                from_user_id,
                                channel_id
                            );

                            runtime.spawn(async move {
                                if let Err(e) =
                                    manager.handle_offer_received(from_user_id, sdp).await
                                {
                                    tracing::error!("Failed to handle offer: {}", e);
                                }
                            });
                        }
                        RTCSessionEvent::AnswerReceived { from_user_id, sdp } => {
                            tracing::info!(
                                "Received answer from {} in channel {}",
                                from_user_id,
                                channel_id
                            );

                            runtime.spawn(async move {
                                if let Err(e) =
                                    manager.handle_answer_received(from_user_id, sdp).await
                                {
                                    tracing::error!("Failed to handle answer: {}", e);
                                }
                            });
                        }
                        RTCSessionEvent::IceCandidateReceived {
                            from_user_id,
                            candidate,
                        } => {
                            tracing::debug!(
                                "Received ICE candidate from {} in channel {}",
                                from_user_id,
                                channel_id
                            );

                            runtime.spawn(async move {
                                if let Err(e) = manager
                                    .handle_ice_candidate_received(from_user_id, candidate)
                                    .await
                                {
                                    tracing::error!("Failed to handle ICE candidate: {}", e);
                                }
                            });
                        }
                        RTCSessionEvent::ScreenShareStarted { user_id } => {
                            tracing::info!(
                                "User {} started screen sharing in channel {}",
                                user_id,
                                channel_id
                            );
                            // Screen share started event - peers will receive the video track
                            // through the normal WebRTC negotiation
                        }
                        RTCSessionEvent::ScreenShareStopped { user_id } => {
                            tracing::info!(
                                "User {} stopped screen sharing in channel {}",
                                user_id,
                                channel_id
                            );
                            // Screen share stopped event
                        }
                    }
                } else {
                    tracing::warn!(
                        "Received RTC event for channel {} but no active session",
                        channel_id
                    );
                }
            }
            Event::Error { message } => {
                tracing::error!("Server error: {}", message);
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check for OAuth callback
        self.check_oauth_callback();

        // Check for server events
        self.check_events();

        // Check for new video playbacks
        self.check_video_playbacks();

        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.connected {
                self.render_login_screen(ui);
            } else {
                self.render_main_screen(ui);
            }
        });

        // Render video playback windows
        if let Some(session) = &self.current_rtc_session {
            for playback in session.video_playbacks.values() {
                playback.render_window(ctx);
            }
        }

        // Request repaint if waiting for OAuth (to check callback)
        if self.oauth_receiver.is_some() {
            ctx.request_repaint();
        }

        // Request repaint if connected to check for events
        if self.connected {
            ctx.request_repaint();
        }
    }
}

fn main() -> eframe::Result<()> {
    env_logger::init();

    // tracing_subscriber::fmt()
    //     .with_target(false)
    //     .with_thread_ids(false)
    //     .with_level(true)
    //     .with_writer(std::io::stdout)
    //     .init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 768.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Malguem",
        native_options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
