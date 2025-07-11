use egui::{Color32, RichText, ScrollArea, TextEdit};
use wg_2024::network::NodeId;
use std::collections::HashMap;
use crate::simulation_controller::gui_input_queue::{push_gui_message, new_gui_input_queue, SharedGuiInput};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use log::info;

#[derive(Clone)]
pub struct ChatMessage {
    pub from: NodeId,
    pub content: String,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum ClientStatus {
    Offline,
    Connected,
    Chatting(NodeId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChatType {
    Normal,
    Temporary,
}

pub struct ChatUIState {
    pub client_status: HashMap<NodeId, ClientStatus>,
    pub servers: Vec<NodeId>,
    pub selected_client: Option<NodeId>,
    pub selected_server: Option<NodeId>,
    pub chat_messages: Vec<ChatMessage>,
    pub chat_input: String,
    pub active_chat_pair: Option<(NodeId, NodeId)>,
    pub pending_chat_request: Option<(NodeId, NodeId)>,
    pub selected_sender: Option<NodeId>,
    pub pending_chat_termination: Option<(NodeId, NodeId)>,
    pub server_client_map: HashMap<NodeId, Vec<NodeId>>,
    pub gui_input: SharedGuiInput,
    pub chat_history: HashMap<(NodeId, NodeId), Vec<ChatMessage>>,
    pub chat_type_map: HashMap<(NodeId, NodeId), ChatType>,
    pub pending_chat_type: Option<ChatType>,
    pub client_server_codes: HashMap<(NodeId, NodeId), String>,
    pub show_history_popup: bool,
    pub history_client_id_input: String,
    pub history_target_id_input: String,

    pub media_files: Vec<(String, String)>, // (media_name, full_path)
    pub broadcast_files: Vec<(String, String)>, // (media_name, full_path)

    pub show_server_popup: Option<NodeId>,
    pub show_upload_media_list: bool,
    pub download_media_name_input: String,
    pub download_result_message: Option<String>,

    pub show_broadcast_list: bool,

    broadcast_result_message: Option <String>,
    pub broadcast_result_time: Option<Instant>,
    pub show_broadcast_media_list: bool,
}

impl ChatUIState {

    pub fn new(gui_input: SharedGuiInput) -> Self {
        ChatUIState {
            client_status: HashMap::new(),
            servers: vec![],
            selected_client: None,
            selected_server: None,
            chat_messages: vec![],
            chat_input: String::new(),
            active_chat_pair: None,
            pending_chat_request: None,
            selected_sender: None,
            pending_chat_termination: None,
            server_client_map: HashMap::new(),
            gui_input,
            chat_history: HashMap::new(),
            chat_type_map: HashMap::new(),
            pending_chat_type: None,
            client_server_codes: HashMap::new(),
            show_history_popup: false,
            history_client_id_input: String::new(),
            history_target_id_input: String::new(),

            media_files: load_media_files(),
            broadcast_files: load_broadcast_files(),
            show_server_popup: None,
            show_upload_media_list: false,
            download_media_name_input: String::new(),
            download_result_message: None,

            show_broadcast_list: false,

            broadcast_result_message: None,
            broadcast_result_time: None,
            show_broadcast_media_list: false,
        }
    }

    fn render_server_info(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.label(RichText::new("Server Overview").strong());

            for &server_id in &self.servers {
                ui.separator();

                let is_logged_in = if let Some(cid) = self.selected_client {
                    self.server_client_map
                        .get(&server_id)
                        .map_or(false, |clients| clients.contains(&cid))
                } else {
                    false
                };

                let mut button = egui::Button::new(format!("Server #{}", server_id));
                if !is_logged_in {
                    button = button.sense(egui::Sense::hover());
                }

                let response = ui.add(button);
                if is_logged_in && response.clicked() {
                    self.show_server_popup = Some(server_id);
                    self.show_upload_media_list = false;
                }

                let clients = self.server_client_map.get(&server_id).cloned().unwrap_or_default();
                if clients.is_empty() {
                    ui.label("  No clients logged in");
                } else {
                    ui.label("  Logged in clients:");
                    for cid in &clients {
                        let code = self.client_server_codes.get(&(*cid, server_id)).map_or("N/A", String::as_str);
                        ui.label(format!("    - Client #{} ", cid));
                    }
                    if let Some((a, b)) = self.active_chat_pair {
                        if clients.contains(&a) && clients.contains(&b) {
                            ui.label("  🔵 Active chat:");
                            ui.label(format!("    - Client #{} ↔ Client #{}", a, b));
                            let key = (a.min(b), a.max(b));
                            if let Some(chat_type) = self.chat_type_map.get(&key) {
                                let label = match chat_type {
                                    ChatType::Normal => "Type: Normal",
                                    ChatType::Temporary => "Type: Temporary",
                                };
                                ui.label(format!("    {}", label));
                            }
                        }
                    }
                }
            }

            ui.separator();
            ui.label(RichText::new("GUI Input Queue").strong());

            if let Ok(map) = self.gui_input.lock() {
                if map.is_empty() {
                    ui.label("No pending messages.");
                } else {
                    for (sender_id, messages) in map.iter() {
                        let label = if self.servers.contains(sender_id) {
                            format!("Server #{} →", sender_id)
                        } else {
                            format!("Client #{} →", sender_id)
                        };
                        ui.label(label);
                        for msg in messages {
                            let short_msg = if msg.starts_with("[MediaBroadcast]:") {
                                let parts: Vec<&str> = msg.splitn(3, "::").collect();
                                if parts.len() == 3 {
                                    format!("[MediaBroadcast]::{}::{}...", parts[1], &parts[2][..5.min(parts[1].len())])
                                } else {
                                    msg.clone()
                                }
                            } else if msg.starts_with("[MediaUpload]:") {
                                let parts: Vec<&str> = msg.splitn(3, "::").collect();
                                if parts.len() == 3 {
                                    format!("[MediaUpload]::{}::{}...", parts[1], &parts[2][..5.min(parts[2].len())])
                                } else {
                                    msg.clone()
                                }
                            } else {
                                msg.clone()
                            };

                            ui.label(format!("    {}", short_msg));
                        }
                    }
                }
            } else {
                ui.label("⚠ Failed to lock GUI input buffer");
            }
        });
    }

    pub fn render(&mut self, ui: &mut egui::Ui, on_send: &mut impl FnMut(NodeId, NodeId, String)) {
        egui::SidePanel::right("server_status_panel").show_inside(ui, |ui| {
            self.render_server_info(ui);
        });

        ui.horizontal_wrapped(|ui| {
            for (&client_id, status) in &self.client_status {
                let color = match status {
                    ClientStatus::Offline => Color32::GRAY,
                    ClientStatus::Connected => Color32::GREEN,
                    ClientStatus::Chatting(_) => Color32::BLUE,
                };
                if ui.add(egui::Button::new(format!("Client #{client_id}")).fill(color)).clicked() {
                    self.selected_client = Some(client_id);
                    if self.active_chat_pair.is_none() {
                        self.selected_server = None;
                    } // hide server-specific rows
                }
            }
        });

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            for &server_id in &self.servers {
                if ui.add(egui::Button::new(format!("Server #{server_id}")).fill(Color32::LIGHT_BLUE)).clicked() {
                    self.selected_server = Some(server_id);
                    self.selected_client = None; // hide client-specific rows
                    self.show_broadcast_list = false;
                }
            }
        });

        ui.separator();
        if let Some(client_id) = self.selected_client {
            let status = self.client_status.get(&client_id).cloned().unwrap_or(ClientStatus::Offline);

            match status {
                ClientStatus::Offline => {
                    ui.horizontal(|ui| {
                        ui.label("Connect to Server:");
                        egui::ComboBox::from_id_source("server_select")
                            .selected_text(self.selected_server.map_or("Select...".into(), |id| format!("Server {id}")))
                            .show_ui(ui, |ui| {
                                for &server_id in &self.servers {
                                    if ui.selectable_label(self.selected_server == Some(server_id), format!("Server #{server_id}")).clicked() {
                                        self.selected_server = Some(server_id);
                                    }
                                }
                            });

                        if ui.button("Login").clicked() {
                            if let Some(server_id) = self.selected_server {
                                self.client_status.insert(client_id, ClientStatus::Connected);
                                self.server_client_map.entry(server_id).or_default().push(client_id);
                                push_gui_message(&self.gui_input, client_id, format!("[Login]::{}", server_id));

                                let code = format!("{:06}", rand::random::<u32>() % 1_000_000);
                                self.client_server_codes.insert((client_id, server_id), code);
                            }
                        }
                    });
                }
                ClientStatus::Connected => {
                    let mut requested_chat_with: Option<NodeId> = None;

                    ui.horizontal(|ui| {
                        ui.label("Start Chat With:");
                        for (&other_id, &other_status) in self.client_status.iter() {
                            if other_id != client_id && other_status == ClientStatus::Connected {
                                let same_server = self.server_client_map.iter().any(|(_, list)| {
                                    list.contains(&client_id) && list.contains(&other_id)
                                });
                                if same_server && ui.button(format!("Client #{other_id}")).clicked() {
                                    requested_chat_with = Some(other_id);
                                }
                            }
                        }
                    });

                    if let Some(peer_id) = requested_chat_with {
                        // find the server they both share
                        if let Some((&server_id, _)) = self.server_client_map
                            .iter()
                            .find(|(_sid, clients)| {
                                clients.contains(&client_id) && clients.contains(&peer_id)
                            })
                        {
                            self.pending_chat_request = Some((client_id, peer_id));
                            self.selected_client = Some(peer_id);
                            // and if you were using selected_server elsewhere:
                            self.selected_server = Some(server_id);
                        }
                    }


                    ui.horizontal(|ui| {
                        ui.label("Interact with server:");
                        for &server_id in &self.servers {
                            // Only display the server the selected client is logged into
                            let is_logged_in = if let Some(cid) = self.selected_client {
                                self.server_client_map
                                    .get(&server_id)
                                    .map_or(false, |clients| clients.contains(&cid))
                            } else {
                                false
                            };

                            if !is_logged_in {
                                continue; // Skip servers the selected client is not connected to
                            }

                            let button = egui::Button::new(format!("Server #{}", server_id));
                            let response = ui.add(button);

                            if response.clicked() {
                                self.show_server_popup = Some(server_id);
                                self.show_upload_media_list = false;
                            }
                        }

                        if let Some(server_id) = self.show_server_popup {
                            egui::Window::new(format!("Server #{} Options", server_id))
                                .collapsible(false)
                                .resizable(false)
                                .show(ui.ctx(), |ui| {
                                    if ui.button("Request Client List").clicked() {
                                        if let Some(client_id) = self.selected_client {
                                            push_gui_message(&self.gui_input, client_id, "[ClientListRequest]".to_string());
                                        }
                                    }

                                    ui.separator();
                                    if ui.button("Upload Media").clicked() {
                                        self.show_upload_media_list = !self.show_upload_media_list;
                                    }

                                    if self.show_upload_media_list {
                                        ui.label("Available Media Files:");
                                        for (media_name, path) in &self.media_files {
                                            if ui.button(media_name).clicked() {
                                                if let Some(client_id) = self.selected_client {
                                                    match std::fs::read(path) {
                                                        Ok(bytes) => {
                                                            let base64_data = base64::encode(bytes);
                                                            let msg = format!("[MediaUpload]::{}::{}", media_name, base64_data);
                                                            push_gui_message(&self.gui_input, client_id, msg);
                                                        }
                                                        Err(e) => {
                                                            eprintln!("Error reading image file '{}': {}", path, e);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    ui.separator();
                                    if ui.button("Broadcast Media").clicked() {
                                        self.show_broadcast_media_list = !self.show_broadcast_media_list;
                                    }
                                    if self.show_broadcast_media_list {
                                        ui.label("Available Media Files:");
                                        for (media_name, path) in &self.broadcast_files {
                                            if ui.button(media_name).clicked() {
                                                if let Some(client_id) = self.selected_client {
                                                    match std::fs::read(path) {
                                                        Ok(bytes) => {
                                                            info!("🐛 GUI encoding started for: {}", media_name);

                                                            let base64_data = base64::encode(bytes);
                                                            info!( "🐻🐻🐻🐻🐻🐻🐻
                                                                → server: {} bytes, prefix = {:?}",
                                                                base64_data.len(),
                                                                &base64_data.as_bytes()[0..20]
                                                            );

                                                            let msg = format!("[MediaBroadcast]::{}::{}", media_name, base64_data);
                                                            push_gui_message(&self.gui_input, client_id, msg);
                                                        }
                                                        Err(e) => {
                                                            eprintln!("Error reading image file '{}': {}", path, e);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    ui.separator();
                                    if ui.button("Request Media List").clicked() {
                                        if let Some(client_id) = self.selected_client {
                                            push_gui_message(&self.gui_input, client_id, "[MediaListRequest]".to_string());
                                        }
                                    }
                                    ui.separator();
                                    ui.label("Download Media:");
                                    ui.horizontal(|ui| {
                                        ui.label("File name:");
                                        ui.text_edit_singleline(&mut self.download_media_name_input);

                                        if ui.button("Download").clicked() {
                                            if let Some(client_id) = self.selected_client {
                                                let trimmed = self.download_media_name_input.trim();
                                                if !trimmed.is_empty() {
                                                    let msg = format!("[MediaDownloadRequest]::{}", trimmed);
                                                    push_gui_message(&self.gui_input, client_id, msg);
                                                    self.download_result_message = Some(format!("Requested \"{}\"", trimmed));
                                                    self.download_media_name_input.clear();
                                                }
                                            }
                                        }
                                    });

                                    if let Some(msg) = &self.download_result_message {
                                        ui.label(egui::RichText::new(msg).color(egui::Color32::LIGHT_GREEN));
                                    }
                                    ui.separator();

                                    if ui.button("Logout").clicked() {
                                        // Find the actual server the client is logged into
                                        let maybe_server_id = self.server_client_map.iter()
                                            .find(|(_, clients)| clients.contains(&client_id))
                                            .map(|(server_id, _)| *server_id);

                                        if let Some(server_id) = maybe_server_id {
                                            // 1. Mark as offline
                                            self.client_status.insert(client_id, ClientStatus::Offline);

                                            // 2. Remove from server's client list
                                            if let Some(clients) = self.server_client_map.get_mut(&server_id) {
                                                clients.retain(|&c| c != client_id);
                                            }

                                            // 3. Clear chat state if this client was involved
                                            if let Some((req, tgt)) = self.pending_chat_request {
                                                if req == client_id || tgt == client_id {
                                                    self.pending_chat_request = None;
                                                }
                                            }
                                            if let Some((c1, c2)) = self.active_chat_pair {
                                                if c1 == client_id || c2 == client_id {
                                                    self.active_chat_pair = None;
                                                }
                                            }

                                            // 4. Clear selected_client if needed
                                            if self.selected_client == Some(client_id) {
                                                self.selected_client = None;
                                            }

                                            // 5. Clear selected_server to avoid showing Broadcast UI
                                            self.selected_server = None;

                                            // 6. Push logout message
                                            push_gui_message(&self.gui_input, client_id, "[Logout]".to_string());
                                            self.show_server_popup = None;
                                            self.show_upload_media_list = false;
                                        }
                                    }
                                    ui.separator();

                                    if ui.button("Close").clicked() {
                                        self.show_server_popup = None;
                                        self.show_upload_media_list = false;
                                        self.download_result_message = None;
                                    }

                                    ui.separator();
                                });
                        }
                    });
                }

                ClientStatus::Chatting(peer_id) => {
                    ui.horizontal(|ui| {
                        if let Some((a, b)) = self.active_chat_pair {
                            if ui.button("End Chat").clicked() {
                                let initiator = a;
                                let peer = b;
                                if let Some(server_id) = self.selected_server {
                                    push_gui_message(&self.gui_input, initiator, format!("[ChatFinish]::{peer}"));
                                    push_gui_message(&self.gui_input, peer, format!("[ChatFinish]::{initiator}"));
                                }

                                let key = (initiator.min(peer), initiator.max(peer));
                                if self.chat_type_map.get(&key) == Some(&ChatType::Temporary) {
                                    self.chat_messages.clear();
                                }
                                self.chat_type_map.remove(&key);

                                self.client_status.insert(initiator, ClientStatus::Connected);
                                self.client_status.insert(peer, ClientStatus::Connected);
                                self.active_chat_pair = None;
                                self.pending_chat_request = None;

                            }
                        }
                    });
                }
            }

            // Show Chat History button is now outside of any active chat condition
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Show Chat History").clicked() {
                    self.show_history_popup = true;
                    self.history_client_id_input.clear();
                    self.history_target_id_input.clear();

                }
            });
        }
        ui.separator();
        if let Some(server_id) = self.selected_server {
            ui.horizontal(|ui| {
                if ui.button("Broadcast Media").clicked() {
                    self.show_broadcast_list = !self.show_broadcast_list;
                }

                if self.show_broadcast_list {
                    egui::Window::new("Broadcast Media Files")
                        .collapsible(false)
                        .resizable(false)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]) // appears in center
                        .show(ui.ctx(), |ui| {
                            ui.label("Choose a file to broadcast:");
                            for (media_name, path) in &self.broadcast_files {
                                if ui.button(media_name).clicked() {
                                    if let Some(server_id) = self.selected_server {
                                        match std::fs::read(path) {
                                            Ok(bytes) => {
                                                let base64_data = base64::encode(bytes);
                                                let msg = format!("[MediaBroadcast]::{}::{}", media_name, base64_data);
                                                push_gui_message(&self.gui_input, server_id, msg);
                                                self.broadcast_result_message =
                                                    Some(format!("📤 Sent '{}' to server {}", media_name, server_id));
                                            }
                                            Err(e) => {
                                                self.broadcast_result_message = Some(format!("❌ Failed to read: {}", e));
                                            }
                                        }
                                        self.broadcast_result_time = Some(Instant::now());
                                        self.show_broadcast_list = false;
                                    }
                                }
                            }

                            ui.separator();
                            if ui.button("Cancel").clicked() {
                                self.show_broadcast_list = false;
                            }
                        });
                }
            });
            if let (Some(msg), Some(time)) = (&self.broadcast_result_message, &self.broadcast_result_time) {
                if time.elapsed().as_secs_f32() < 2.0 {
                    ui.label(RichText::new(msg).color(Color32::LIGHT_GREEN));
                } else {
                    self.broadcast_result_message = None;
                    self.broadcast_result_time = None;
                }
            }
        }

        if let Some((requester, target)) = self.pending_chat_request {
            // Show the chat request popup regardless of the selected client
            egui::Window::new(format!("Incoming Chat Request for Client #{}", target))
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label(format!("Client #{} wants to start a chat with Client #{}.", requester, target));

                    let selected_chat_type = self.pending_chat_type.get_or_insert(ChatType::Normal);

                    ui.horizontal(|ui| {
                        ui.label("Chat Type:");
                        egui::ComboBox::from_id_source("chat_type_selector")
                            .selected_text(match selected_chat_type {
                                ChatType::Normal => "Normal",
                                ChatType::Temporary => "Temporary",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(selected_chat_type, ChatType::Normal, "Normal");
                                ui.selectable_value(selected_chat_type, ChatType::Temporary, "Temporary");
                            });
                    });

                    let mut accept_clicked = false;
                    let mut decline_clicked = false;

                    ui.horizontal(|ui| {
                        // Make the buttons more visible
                        if ui.button("Accept").clicked() {
                            accept_clicked = true;
                        }
                        if ui.button("Decline").clicked() {
                            decline_clicked = true;
                        }
                    });

                    // Only allow responding if the target client is selected
                    if Some(target) != self.selected_client {
                        ui.label(RichText::new(format!("⚠ Click on Client #{} to respond to this request", target)).color(Color32::YELLOW));
                    } else if accept_clicked {
                        let chat_type = self.pending_chat_type.unwrap_or(ChatType::Normal);
                        let key = (requester.min(target), requester.max(target));
                        self.chat_type_map.insert(key, chat_type);

                        self.client_status.insert(requester, ClientStatus::Chatting(target));
                        self.client_status.insert(target, ClientStatus::Chatting(requester));
                        self.active_chat_pair = Some((requester, target));
                        self.selected_sender = Some(target);

                        self.pending_chat_request = None;
                        self.pending_chat_type = None;

                        if chat_type == ChatType::Temporary {
                            self.chat_messages.clear();
                        } else {
                            self.chat_messages = self.chat_history.get(&key).cloned().unwrap_or_default();
                        }
                        push_gui_message(&self.gui_input, requester, format!("[ChatRequest]::{target}"));
                        // push_gui_message(&self.gui_input, target, format!("[ChatRequest]::{requester}"));


                    } else if decline_clicked {
                        self.pending_chat_request = None;
                    }
                });
        }

        ui.separator();
        if let Some((a, b)) = self.active_chat_pair {
            let options = [a, b];
            ui.horizontal(|ui| {
                ui.label("From:");
                egui::ComboBox::from_id_source("chat_from_selector")
                    .selected_text(self.selected_sender.map_or("Select sender".into(), |id| format!("Client #{id}")))
                    .show_ui(ui, |ui| {
                        for &id in &options {
                            if ui.selectable_label(self.selected_sender == Some(id), format!("Client #{id}")).clicked() {
                                self.selected_sender = Some(id);
                            }
                        }
                    });
            });

            ui.horizontal(|ui| {
                let lost_focus = ui.add(TextEdit::singleline(&mut self.chat_input).hint_text("Type message...")).lost_focus();
                if ui.button("Send").clicked() || (lost_focus && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    if let Some(from) = self.selected_sender {
                        let to = if from == a { b } else { a };
                        if !self.chat_input.trim().is_empty() {
                            let msg = ChatMessage { from, content: self.chat_input.clone() };
                            self.chat_messages.push(msg.clone());

                            let key = (from.min(to), from.max(to));
                            if self.chat_type_map.get(&key) == Some(&ChatType::Normal) {
                                self.chat_history.entry(key).or_default().push(msg.clone());
                            }

                            push_gui_message(&self.gui_input, from, format!("[MessageTo]::{to}::{}", msg.content));
                            self.chat_input.clear();
                        }
                    }
                }
            });

            ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                for msg in &self.chat_messages {
                    ui.label(format!("From Client #{}: {}", msg.from, msg.content));
                }
            });
        }

        // History popup is now outside the active chat condition
        if self.show_history_popup {
            egui::Window::new("Retrieve Chat History").collapsible(false).show(ui.ctx(), |ui| {
                ui.label("Client ID (you):");
                ui.add(TextEdit::singleline(&mut self.history_client_id_input).hint_text("e.g. 101"));

                ui.label("See chat with Client ID:");
                ui.add(TextEdit::singleline(&mut self.history_target_id_input).hint_text("e.g. 102"));

                ui.horizontal(|ui| {
                    if ui.button("Submit").clicked() {
                        if let (Ok(client_id), Ok(target_id), Some(server_id)) = (
                            self.history_client_id_input.parse::<NodeId>(),
                            self.history_target_id_input.parse::<NodeId>(),
                            self.selected_server,
                        ) {
                            push_gui_message(&self.gui_input, client_id, format!("[HistoryRequest]::{}::{}", client_id, target_id));
                            self.show_history_popup = false;
                        }
                    }

                    if ui.button("Close").clicked() {
                        self.show_history_popup = false;
                        self.history_client_id_input.clear();
                        self.history_target_id_input.clear();
                    }
                });
            });
        }
    }
}

//🖼️🖼️🖼️loading media🖼️🖼️🖼️
fn load_media_files() -> Vec<(String, String)> {
    let media_dir = "media"; // relative path
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(media_dir) {
        for entry in entries.flatten() {
            let path: PathBuf = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ["jpg", "jpeg", "png"].contains(&ext.to_lowercase().as_str()) {
                    if let Some(fname) = path.file_name().and_then(|f| f.to_str()) {
                        files.push((fname.to_string(), path.to_string_lossy().to_string()));
                    }
                }
            }
        }
    }
    files
}
fn load_broadcast_files() -> Vec<(String, String)> {
    let media_dir = "broadcast media"; // relative path
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(media_dir) {
        for entry in entries.flatten() {
            let path: PathBuf = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ["jpg", "jpeg", "png"].contains(&ext.to_lowercase().as_str()) {
                    if let Some(fname) = path.file_name().and_then(|f| f.to_str()) {
                        files.push((fname.to_string(), path.to_string_lossy().to_string()));
                    }
                }
            }
        }
       }
   files
}
