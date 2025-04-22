use egui::{Color32, RichText, ScrollArea, TextEdit};
use wg_2024::network::NodeId;

#[derive(Clone)]
pub struct ChatMessage {
    pub from: NodeId,
    pub content: String,
}

pub struct ChatUIState {
    pub online_clients: Vec<NodeId>,
    pub all_clients: Vec<NodeId>, // includes offline
    pub servers: Vec<NodeId>,
    pub selected_sender: Option<NodeId>,
    pub selected_recipient: Option<NodeId>,
    pub messages: Vec<ChatMessage>,
    pub input: String,
}

impl ChatUIState {
    pub fn new() -> Self {
        ChatUIState {
            online_clients: vec![],
            all_clients: vec![],
            servers: vec![],
            selected_sender: None,
            selected_recipient: None,
            messages: vec![],
            input: String::new(),
        }
    }

    pub fn render(&mut self, ui: &mut egui::Ui, on_send: impl Fn(NodeId, NodeId, Option<String>)) {
        ui.heading("Online Clients");
        ui.horizontal_wrapped(|ui| {
            for &client_id in &self.all_clients {
                let is_online = self.online_clients.contains(&client_id);
                let dot_color = if is_online { Color32::GREEN } else { Color32::GRAY };
                ui.horizontal(|ui| {
                    ui.colored_label(dot_color, "●");
                    ui.label(format!("Client #{}", client_id));
                });
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("From:");
            egui::ComboBox::from_id_source("sender_combo")
                .selected_text(self.selected_sender.map_or("Select...".into(), |id| format!("Client #{}", id)))
                .show_ui(ui, |ui| {
                    for &id in &self.online_clients {
                        if ui.selectable_label(self.selected_sender == Some(id), format!("Client #{}", id)).clicked() {
                            self.selected_sender = Some(id);
                        }
                    }
                });

            ui.label("To:");
            egui::ComboBox::from_id_source("recipient_combo")
                .selected_text(self.selected_recipient.map_or("Select...".into(), |id| format!("Node #{}", id)))
                .show_ui(ui, |ui| {
                    for &id in &self.all_clients {
                        if ui.selectable_label(self.selected_recipient == Some(id), format!("Client #{}", id)).clicked() {
                            self.selected_recipient = Some(id);
                        }
                    }
                    for &id in &self.servers {
                        if ui.selectable_label(self.selected_recipient == Some(id), format!("Server #{}", id)).clicked() {
                            self.selected_recipient = Some(id);
                        }
                    }
                });
        });

        let needs_input = self.selected_recipient.map_or(false, |id| self.all_clients.contains(&id));

        if needs_input {
            ui.horizontal(|ui| {
                let input = ui.add(TextEdit::singleline(&mut self.input).hint_text("Type message...")).lost_focus();
                if ui.button("Send").clicked() || (input && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    if let (Some(from), Some(to)) = (self.selected_sender, self.selected_recipient) {
                        if !self.input.trim().is_empty() {
                            on_send(from, to, Some(self.input.trim().to_string()));
                            self.input.clear();
                        }
                    }
                }
            });
        } else {
            if ui.button("Request Client List").clicked() {
                if let (Some(from), Some(to)) = (self.selected_sender, self.selected_recipient) {
                    on_send(from, to, None); // message with no input body
                }
            }
        }

        ui.separator();
        ui.heading("Chat");
        ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                for msg in &self.messages {
                    ui.label(format!("From Client #{}: {}", msg.from, msg.content));
                }
            });
    }
}