use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wg_2024::network::NodeId;

/// Shared type for GUI-to-client message queue
type GuiMessageBuffer = HashMap<NodeId, Vec<String>>;
pub type SharedGuiInput = Arc<Mutex<GuiMessageBuffer>>;

/// Create a new shared input buffer
pub fn new_gui_input_queue() -> SharedGuiInput {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Push a message from the GUI into the buffer for a given client
pub fn push_gui_message(queue: &SharedGuiInput, from: NodeId, msg: String) {
    if let Ok(mut map) = queue.lock() {
        //map.entry(from).or_default().push((to, msg));
        map.entry(from).or_default().push(msg);

    }
}


/// Pop and return all pending messages for a client
pub fn pop_all_gui_messages(queue: &SharedGuiInput, client_id: NodeId) -> Vec<String> {
    if let Ok(mut map) = queue.lock() {
        map.remove(&client_id).unwrap_or_default()
    } else {
        vec![]
    }
}
