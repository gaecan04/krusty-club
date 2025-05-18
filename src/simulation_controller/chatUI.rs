use egui::{Color32, RichText, ScrollArea, TextEdit};
use wg_2024::network::NodeId;
use std::collections::HashMap;
use crate::simulation_controller::gui_input_queue::{push_gui_message, new_gui_input_queue, SharedGuiInput};

#[derive(Clone)]
pub struct ChatMessage {
    pub from: NodeId,
    pub content: String,
}

#[derive(PartialEq, Eq, Clone, Copy)]
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
    pub history_code_input: String,
    pub history_client_id_input: String,
    pub history_target_id_input: String,
    pub history_code_failed: bool,
    pub history_code_success: bool,
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
            history_code_input: String::new(),
            history_client_id_input: String::new(),
            history_target_id_input: String::new(),
            history_code_failed: false,
            history_code_success: false,
        }
    }

    fn render_server_info(&self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.label(RichText::new("Server Overview").strong());
            for &server_id in &self.servers {
                ui.separator();
                ui.label(format!("Server #{server_id}"));
                let clients = self.server_client_map.get(&server_id).cloned().unwrap_or_default();
                if clients.is_empty() {
                    ui.label("  No clients logged in");
                } else {
                    ui.label("  Logged in clients:");
                    for cid in &clients {
                        let code = self.client_server_codes.get(&(*cid, server_id)).map_or("N/A", String::as_str);
                        ui.label(format!("    - Client #{} [Code: {}]", cid, code));
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
            ui.label(RichText::new("GUI Input Queue (per client)").strong());

            if let Ok(map) = self.gui_input.lock() {
                if map.is_empty() {
                    ui.label("No pending messages.");
                } else {
                    for (client_id, messages) in map.iter() {
                        ui.label(format!("Client #{} →", client_id));
                        for msg in messages {
                            ui.label(format!("    {:?}", msg.1.clone()));
                        }
                    }
                }
            } else {
                ui.label("⚠️ Failed to lock GUI input buffer");
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
                }
            }
        });

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            for &server_id in &self.servers {
                if ui.add(egui::Button::new(format!("Server #{server_id}")).fill(Color32::LIGHT_BLUE)).clicked() {
                    self.selected_server = Some(server_id);
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
                            .selected_text(self.selected_server.map_or("Select...".into(), |id| format!("Server #{id}")))
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
                                push_gui_message(&self.gui_input, client_id, server_id, "[Login]".to_string());
                                let code = format!("{:06}", rand::random::<u32>() % 1_000_000);
                                self.client_server_codes.insert((client_id, server_id), code);
                            }
                        }
                    });
                }
                ClientStatus::Connected => {
                    ui.horizontal(|ui| {
                        if ui.button("Logout").clicked() {
                            if let Some(server_id) = self.selected_server {
                                self.client_status.insert(client_id, ClientStatus::Offline);
                                if let Some(clients) = self.server_client_map.get_mut(&server_id) {
                                    clients.retain(|&c| c != client_id);
                                }
                                push_gui_message(&self.gui_input, client_id, server_id, "[Logout]".to_string());
                            }
                        }

                        if ui.button("Request Client List").clicked() {
                            if let Some(server_id) = self.selected_server {
                                push_gui_message(&self.gui_input, client_id, server_id, "[ClientListRequest]".to_string());
                            }
                        }
                    });

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
                        if let Some(server_id) = self.selected_server {
                            self.pending_chat_request = Some((client_id, peer_id));
                            push_gui_message(&self.gui_input, client_id, server_id, format!("[ChatRequest]::{peer_id}"));
                        }
                    }
                }
                ClientStatus::Chatting(peer_id) => {
                    ui.horizontal(|ui| {
                        if let Some((a, b)) = self.active_chat_pair {
                            if ui.button("End Chat").clicked() {
                                self.pending_chat_termination = Some((a, b));
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
                    self.history_code_input.clear();
                    self.history_client_id_input.clear();
                    self.history_target_id_input.clear();
                    self.history_code_failed = false;
                    self.history_code_success = false;
                }
            });
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
                        ui.label(RichText::new(format!("⚠️ Click on Client #{} to respond to this request", target)).color(Color32::YELLOW));
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
                    } else if decline_clicked {
                        self.pending_chat_request = None;
                    }
                });
        }

        if let Some((initiator, peer)) = self.pending_chat_termination {
            // Show the chat termination popup regardless of the selected client
            egui::Window::new(format!("Confirm End Chat for Client #{}", peer))
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label(format!("Client #{} wants to end the chat with Client #{}.", initiator, peer));

                    let mut confirm_clicked = false;
                    let mut cancel_clicked = false;

                    ui.horizontal(|ui| {
                        if ui.button("Confirm End").clicked() {
                            confirm_clicked = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_clicked = true;
                        }
                    });

                    // Only allow responding if the peer client is selected
                    if Some(peer) != self.selected_client {
                        ui.label(RichText::new(format!("⚠️ Click on Client #{} to respond to this request", peer)).color(Color32::YELLOW));
                    } else if confirm_clicked {
                        if let Some(server_id) = self.selected_server {
                            push_gui_message(&self.gui_input, initiator, server_id, format!("[ChatFinish]::{peer}"));
                        }

                        let key = (initiator.min(peer), initiator.max(peer));
                        if self.chat_type_map.get(&key) == Some(&ChatType::Temporary) {
                            self.chat_messages.clear();
                        }
                        self.chat_type_map.remove(&key);

                        self.client_status.insert(initiator, ClientStatus::Connected);
                        self.client_status.insert(peer, ClientStatus::Connected);
                        self.active_chat_pair = None;
                        self.pending_chat_termination = None;
                    } else if cancel_clicked {
                        self.pending_chat_termination = None;
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

                            push_gui_message(&self.gui_input, from, to, format!("[MessageTo]::{to}::{}", msg.content));
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

                ui.label("Security Code:");
                ui.add(TextEdit::singleline(&mut self.history_code_input).hint_text("123456"));

                ui.horizontal(|ui| {
                    if ui.button("Submit").clicked() {
                        if let (Ok(client_id), Ok(target_id), Some(server_id)) = (
                            self.history_client_id_input.parse::<NodeId>(),
                            self.history_target_id_input.parse::<NodeId>(),
                            self.selected_server,
                        ) {
                            let correct_code = self.client_server_codes.get(&(client_id, server_id));
                            if correct_code == Some(&self.history_code_input) {
                                push_gui_message(&self.gui_input, client_id, target_id, format!("[HistoryRequest]::{}::{}",client_id, target_id));
                                self.history_code_success = true;
                                self.history_code_failed = false;
                                self.show_history_popup = false; // close popup
                            } else {
                                self.history_code_failed = true;
                                self.history_code_success = false;
                            }
                        } else {
                            self.history_code_failed = true;
                            self.history_code_success = false;
                        }
                    }

                    if ui.button("Close").clicked() {
                        self.show_history_popup = false;
                        self.history_code_input.clear();
                        self.history_client_id_input.clear();
                        self.history_target_id_input.clear();
                        self.history_code_failed = false;
                        self.history_code_success = false;
                    }
                });

                if self.history_code_failed {
                    ui.label(RichText::new("❌ Incorrect code or client ID").color(Color32::RED));
                } else if self.history_code_success {
                    ui.label(RichText::new("✔ Code accepted. History request sent.").color(Color32::GREEN));
                }
            });
        }
    }
}


