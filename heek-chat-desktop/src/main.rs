use eframe::egui;
use heek_chat_lib::*;
use std::sync::Arc;

struct HeekChatApp {
    // Connection state
    connected: bool,
    server_address: String,
    connection_error: Option<String>,

    // User state
    current_user: Option<User>,

    // UI state
    username_input: String,
    nickname_input: String,
    message_input: String,

    // Data
    channels: Vec<Channel>,
    current_channel: Option<String>,
    messages: Vec<TextMessage>,

    // Runtime
    runtime: Arc<tokio::runtime::Runtime>,
    client: Option<ChatServiceClient>,
}

impl Default for HeekChatApp {
    fn default() -> Self {
        Self {
            connected: false,
            server_address: "127.0.0.1:8080".to_string(),
            connection_error: None,
            current_user: None,
            username_input: String::new(),
            nickname_input: String::new(),
            message_input: String::new(),
            channels: Vec::new(),
            current_channel: None,
            messages: Vec::new(),
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

    fn connect_to_server(&mut self) {
        let addr = self.server_address.clone();
        let runtime = self.runtime.clone();
        let username = self.username_input.clone();
        let nickname = self.nickname_input.clone();

        self.connection_error = None;

        // Connect to server and register user
        match runtime.block_on(async move {
            // Parse address
            let socket_addr: std::net::SocketAddr = addr.parse()
                .map_err(|e| format!("Invalid address: {}", e))?;

            // Connect to server
            let transport = tarpc::serde_transport::tcp::connect(
                socket_addr,
                tokio_serde::formats::Cbor::default,
            )
            .await
            .map_err(|e| format!("Connection failed: {}", e))?;

            let client = ChatServiceClient::new(tarpc::client::Config::default(), transport).spawn();

            // Register user (using dummy Firebase token for now)
            let auth = UserAuth {
                user_id: username.clone(),
                firebase_token: "dummy_token".to_string(),
            };

            let user = client
                .register_user(tarpc::context::current(), auth, username, nickname)
                .await
                .map_err(|e| format!("RPC failed: {}", e))?
                .map_err(|e| format!("Registration failed: {}", e))?;

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

        ui.horizontal(|ui| {
            ui.label("Server:");
            ui.text_edit_singleline(&mut self.server_address);
        });

        ui.horizontal(|ui| {
            ui.label("Username:");
            ui.text_edit_singleline(&mut self.username_input);
        });

        ui.horizontal(|ui| {
            ui.label("Nickname:");
            ui.text_edit_singleline(&mut self.nickname_input);
        });

        ui.add_space(10.0);

        if ui.button("Connect").clicked() {
            self.connect_to_server();
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
                    ui.label(format!("@{}", user.nickname));
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
                            for msg in &self.messages {
                                if msg.channel_id == *channel_id {
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
        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.connected {
                // ui.centered_and_justified(|ui| {
                self.render_login_screen(ui);
                // });
            } else {
                self.render_main_screen(ui);
            }
        });
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
