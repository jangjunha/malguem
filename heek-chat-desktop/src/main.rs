use eframe::egui;
use heek_chat_lib::*;
use std::{collections::HashMap, sync::Arc};

mod firebase_auth;
use firebase_auth::FirebaseAuth;

struct HeekChatApp {
    // Connection state
    connected: bool,
    server_address: String,
    connection_error: Option<String>,

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
    current_channel: Option<String>,
    channel_messages: HashMap<String, Vec<Message>>,

    // Runtime
    runtime: Arc<tokio::runtime::Runtime>,
    client: Option<ChatServiceClient>,
}

impl Default for HeekChatApp {
    fn default() -> Self {
        let firebase_auth = FirebaseAuth::new();

        Self {
            connected: false,
            server_address: "127.0.0.1:8080".to_string(),
            connection_error: None,
            firebase_auth,
            current_user: None,
            firebase_token: None,
            invitation_token_input: String::new(),
            server_key: String::new(),
            email_input: String::new(),
            password_input: String::new(),
            username_input: String::new(),
            message_input: String::new(),
            oauth_receiver: None,
            oauth_redirect_uri: None,
            channels: Vec::new(),
            current_channel: None,
            channel_messages: HashMap::new(),
            runtime: Arc::new(
                tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"),
            ),
            client: None,
        }
    }
}

impl HeekChatApp {
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

    fn connect_to_server_with_firebase_token(&mut self, firebase_token: String) {
        let addr = self.server_address.clone();
        let runtime = self.runtime.clone();
        let username = self.username_input.clone();
        let invitation_token = self.invitation_token_input.clone();

        self.connection_error = None;

        // Connect to server and register user
        match runtime.block_on(async move {
            // Parse address
            let socket_addr: std::net::SocketAddr = addr
                .parse()
                .map_err(|e| format!("Invalid address: {}", e))?;

            // Connect to server
            let transport = tarpc::serde_transport::tcp::connect(
                socket_addr,
                tokio_serde::formats::Cbor::default,
            )
            .await
            .map_err(|e| format!("Connection failed: {}", e))?;

            let client =
                ChatServiceClient::new(tarpc::client::Config::default(), transport).spawn();

            // Join
            if !invitation_token.is_empty() {
                let _ = client
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

            let user = client
                .get_me(tarpc::context::current(), firebase_token.clone())
                .await
                .map_err(|e| format!("RPC failed: {}", e))?
                .map_err(|e| format!("Get me failed: {}", e))?;

            Ok::<(ChatServiceClient, User), String>((client, user))
        }) {
            Ok((client, user)) => {
                self.client = Some(client);
                self.current_user = Some(user);
                self.connected = true;
                tracing::info!("Connected to server successfully");
            }
            Err(e) => {
                self.connection_error = Some(e);
                tracing::error!("Failed to connect: {:?}", self.connection_error);
            }
        }
    }

    fn render_login_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("heek-chat");

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
            ui.heading("heek-chat");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(user) = &self.current_user {
                    ui.label(format!("@{}", user.username));
                }
            });
        });

        ui.separator();

        // Main content area
        ui.horizontal(|ui| {
            // Channel list sidebar
            ui.vertical(|ui| {
                ui.set_width(200.0);
                ui.heading("Channels");

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for channel in &self.channels {
                        let selected = self.current_channel.as_ref() == Some(&channel.channel_id);
                        if ui.selectable_label(selected, &channel.name).clicked() {
                            self.current_channel = Some(channel.channel_id.clone());
                        }
                    }
                });

                ui.add_space(10.0);
                if ui.button("Create Channel").clicked() {
                    // TODO: Show create channel dialog
                }
            });

            ui.separator();

            // Chat area
            ui.vertical(|ui| {
                if let Some(channel_id) = &self.current_channel {
                    // Messages
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            if let Some(messages) = self.channel_messages.get(channel_id) {
                                for msg in messages {
                                    ui.horizontal(|ui| {
                                        ui.label(&msg.sender_id);
                                        ui.label(":");
                                        // TODO: Decrypt message content
                                        ui.label("<encrypted>");
                                    });
                                }
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
}

impl eframe::App for HeekChatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check for OAuth callback
        self.check_oauth_callback();

        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.connected {
                self.render_login_screen(ui);
            } else {
                self.render_main_screen(ui);
            }
        });

        // Request repaint if waiting for OAuth (to check callback)
        if self.oauth_receiver.is_some() {
            ctx.request_repaint();
        }
    }
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt::init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 768.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Heek Chat",
        native_options,
        Box::new(|cc| Ok(Box::new(HeekChatApp::new(cc)))),
    )
}
