use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wg_2024::network::NodeId;
use crate::network::initializer::ParsedConfig;

type GuiMessageBuffer = HashMap<NodeId, Vec<String>>;
pub type SharedGuiInput = Arc<Mutex<GuiMessageBuffer>>;

pub fn new_gui_input_queue() -> SharedGuiInput {
    Arc::new(Mutex::new(HashMap::new()))
}

pub fn push_gui_message(queue: &SharedGuiInput, from: NodeId, msg: String) {
    if let Ok(mut map) = queue.lock() {
        println!("📤 Inserting message for NodeId: {}", from);
        map.entry(from).or_default().push(msg);
    } else {
        println!("❌ Could not lock GUI input queue");
    }
}

pub fn broadcast_topology_change(
    gui_input: &SharedGuiInput,
    config: &Arc<Mutex<ParsedConfig>>,
    message: &str,
) {
   if let Ok(cfg) = config.lock() {
        for client in &cfg.client {
            push_gui_message(gui_input, client.id, message.to_string());
        }
        for server in &cfg.server {
            push_gui_message(gui_input, server.id, message.to_string());
        }
    }
}



