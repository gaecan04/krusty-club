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
    pub server_client_map: HashMap<NodeId, Vec<NodeId>>, // new: server ID -> logged-in clients
    pub gui_input: SharedGuiInput,
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
                        ui.label(format!("    - Client #{}", cid));
                    }
                    if let Some((a, b)) = self.active_chat_pair {
                        if clients.contains(&a) && clients.contains(&b) {
                            ui.label("  🔵 Active chat:");
                            ui.label(format!("    - Client #{} ↔ Client #{}", a, b));
                        }
                    }
                }
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
                                on_send(client_id, server_id, "[Login]".to_string());
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
                                if let Some(clients) = self.server_client_map.get_mut(&server_id) {
                                    clients.retain(|&c| c != client_id);
                                }
                                on_send(client_id, server_id, "[Logout]".to_string());
                            }
                        }

                        if ui.button("Request Client List").clicked() {
                            if let Some(server_id) = self.selected_server {
                                on_send(client_id, server_id, "[ClientListRequest]".to_string());
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
                            on_send(client_id, server_id, format!("[ChatRequest]::{peer_id}"));
                        }
                    }
                }
                ClientStatus::Chatting(peer_id) => {
                    ui.horizontal(|ui| {
                        if ui.button("End Chat").clicked() {
                            self.pending_chat_termination = Some((client_id, peer_id));
                        }
                    });
                }
            }
        }

        if let Some((requester, target)) = self.pending_chat_request {
            if Some(target) == self.selected_client {
                egui::Window::new("Incoming Chat Request")
                    .collapsible(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("Client #{} wants to start a chat.", requester));

                        if ui.button("Accept").clicked() {
                            self.client_status.insert(requester, ClientStatus::Chatting(target));
                            self.client_status.insert(target, ClientStatus::Chatting(requester));
                            self.active_chat_pair = Some((requester, target));
                            self.selected_sender = Some(target);
                            self.pending_chat_request = None;
                        }

                        if ui.button("Decline").clicked() {
                            self.pending_chat_request = None;
                        }
                    });
            }
        }

        if let Some((initiator, peer)) = self.pending_chat_termination {
            if Some(peer) == self.selected_client {
                egui::Window::new("Confirm End Chat")
                    .collapsible(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("Client #{} wants to end the chat.", initiator));

                        if ui.button("Confirm End").clicked() {
                            if let Some(server_id) = self.selected_server {
                                on_send(initiator, server_id, "[ChatFinish]".to_string());
                            }
                            self.client_status.insert(initiator, ClientStatus::Connected);
                            self.client_status.insert(peer, ClientStatus::Connected);
                            self.active_chat_pair = None;
                            self.pending_chat_termination = None;
                        }

                        if ui.button("Cancel").clicked() {
                            self.pending_chat_termination = None;
                        }
                    });
            }
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
                                //here the sender of the msg
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
                            let msg = format!("[MessageTo]::{to}::{}", self.chat_input.trim());
                            //on_send(from, to, msg);
                            push_gui_message(&self.gui_input, from, msg);

                            self.chat_messages.push(ChatMessage { from, content: self.chat_input.clone() });
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
    }
}

//concerning the chat i still need to figure out the conditional print off he msg