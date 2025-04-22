use egui::{Color32, RichText, ScrollArea, TextEdit};
use wg_2024::network::NodeId;

#[derive(Clone)]
pub struct ChatMessage {
    pub from: NodeId,
    pub content: String,
}

pub struct ChatUIState {
    pub online_clients: Vec<NodeId>,
    pub selected_recipient: Option<NodeId>,
    pub messages: Vec<ChatMessage>,
    pub input: String,
}

impl ChatUIState {
    pub fn new() -> Self {
        ChatUIState {
            online_clients: vec![],
            selected_recipient: None,
            messages: vec![],
            input: String::new(),
        }
    }

    pub fn render(&mut self, ui: &mut egui::Ui, current_client: NodeId, on_send: impl Fn(NodeId, String)) {
        ui.heading("Online Clients");
        ui.horizontal_wrapped(|ui| {
            for &client_id in &self.online_clients {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::GREEN, "●");
                    ui.label(format!("Client #{}", client_id));
                });
            }
        });

        ui.separator();
        ui.heading("Chat");
        ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                for msg in &self.messages {
                    ui.label(format!("From Client #{}: {}", msg.from, msg.content));
                }
            });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("To:");
            egui::ComboBox::from_id_source("recipient_combo")
                .selected_text(self.selected_recipient.map_or("Select...".into(), |id| format!("Client #{}", id)))
                .show_ui(ui, |ui| {
                    for &id in &self.online_clients {
                        if ui.selectable_label(self.selected_recipient == Some(id), format!("Client #{}", id)).clicked() {
                            self.selected_recipient = Some(id);
                        }
                    }
                });
        });

        ui.horizontal(|ui| {
            let input = ui.add(TextEdit::singleline(&mut self.input).hint_text("Type message...")).lost_focus();
            if ui.button("Send").clicked() || input && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some(to) = self.selected_recipient {
                    if !self.input.trim().is_empty() {
                        on_send(to, self.input.trim().to_string());
                        self.input.clear();
                    }
                }
            }
        });
    }
}
