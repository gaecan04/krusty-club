use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use wg_network::NodeId;
// Import relevant structs from other files (assuming these are public in your files)


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeType {
    Server,
    Client,
    Drone,
}

pub trait SimulationController {
    fn crash(&mut self, crashed: &str);
    fn spawn_node(&mut self, node_id: NodeId, node_type: NodeType /*metadata*/); //includes set pdr
    fn message_sent(source: &str, target: &str /*metadata*/);

}


