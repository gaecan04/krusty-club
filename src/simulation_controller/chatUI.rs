use egui::{Color32, RichText, ScrollArea, TextEdit};
use wg_2024::network::NodeId;
use std::collections::HashMap;

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
    pub messages: Vec<ChatMessage>,
}

impl ChatUIState {
    pub fn new() -> Self {
        ChatUIState {
            client_status: HashMap::new(),
            servers: vec![],
            selected_client: None,
            selected_server: None,
            chat_messages: vec![],
            chat_input: String::new(),
            active_chat_pair: None,
            messages: vec![],
        }
    }

    pub fn render(&mut self, ui: &mut egui::Ui, on_send: &mut impl FnMut(NodeId, NodeId, String)) {
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
                                on_send(client_id, server_id, "[Logout]".to_string());
                            }
                        }

                        if ui.button("Request Client List").clicked() {
                            if let Some(server_id) = self.selected_server {
                                on_send(client_id, server_id, "[ClientListRequest]".to_string());
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Start Chat With:");
                        let mut start_chat_with: Option<NodeId> = None;

                        for (&other_id, other_status) in &self.client_status {
                            if other_id != client_id && *other_status == ClientStatus::Connected {
                                if ui.button(format!("Client #{other_id}")).clicked() {
                                    start_chat_with = Some(other_id);
                                }
                            }
                        }

                        if let Some(peer_id) = start_chat_with {
                            if let Some(server_id) = self.selected_server {
                                on_send(client_id, server_id, format!("[ChatRequest]::{peer_id}"));
                                self.client_status.insert(client_id, ClientStatus::Chatting(peer_id));
                                self.client_status.insert(peer_id, ClientStatus::Chatting(client_id));
                                self.active_chat_pair = Some((client_id, peer_id));
                            }
                        }

                    });
                }
                ClientStatus::Chatting(peer_id) => {
                    ui.horizontal(|ui| {
                        if ui.button("End Chat").clicked() {
                            if let Some(server_id) = self.selected_server {
                                on_send(client_id, server_id, "[ChatFinish]".to_string());
                                self.client_status.insert(client_id, ClientStatus::Connected);
                                self.client_status.insert(peer_id, ClientStatus::Connected);
                                self.active_chat_pair = None;
                            }
                        }
                    });
                }
            }
        }

        ui.separator();
        if let Some((from, to)) = self.active_chat_pair {
            ui.horizontal(|ui| {
                ui.label("From:");
                egui::ComboBox::from_id_source("from_selector")
                    .selected_text(format!("Client #{from}"))
                    .show_ui(ui, |ui| {
                        ui.label(format!("Chat from Client #{from} to #{to}"));
                    });
            });

            ui.horizontal(|ui| {
                let lost_focus = ui.add(TextEdit::singleline(&mut self.chat_input).hint_text("Type message...")).lost_focus();
                if ui.button("Send").clicked() || (lost_focus && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    if !self.chat_input.trim().is_empty() {
                        let msg = format!("[MessageTo]::{to}::{}", self.chat_input.trim());
                        on_send(from, to, msg);
                        self.chat_messages.push(ChatMessage { from, content: self.chat_input.clone() });
                        self.chat_input.clear();
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
