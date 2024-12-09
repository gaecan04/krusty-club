use std::collections::HashMap;
use crossbeam_channel::Sender;
use wg_2024::network::NodeId;
use wg_2024::packet::Packet;
use wg_2024::controller::{DroneCommand, DroneEvent};  // Assuming command.rs has the necessary enums

/// Trait that defines the interface for controlling the simulation.
pub trait SimulationController {
    fn crash(&mut self, crashed: NodeId);
    fn spawn_node(&mut self, node_id: NodeId, node_type: NodeType);
    fn message_sent(&self, source: NodeId, target: NodeId);
    fn handle_node_event(&mut self, event: DroneEvent);
}

/// Enum to define node types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeType {
    Server,
    Client,
    Drone,
}

/// The implementation of the SimulationController trait.
pub struct SimulationControllerImpl {
    nodes: Vec<NodeId>,
    // You could add more fields to store the network state or other data
    drone_channels: HashMap<NodeId, Sender<Packet>>, // Map of NodeId to sender channels

}

impl SimulationControllerImpl {
    /// Create a new instance of the SimulationControllerImpl
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            drone_channels: HashMap::new(),
        }
    }
    pub fn add_channel(&mut self, node_id: NodeId, channel: Sender<Packet>) {
        self.drone_channels.insert(node_id, channel);
    }
}

impl SimulationController for SimulationControllerImpl {
    /// Handle a node crash event.
    fn crash(&mut self, crashed: NodeId) {
        println!("Node {} crashed!", crashed);
        // Remove the crashed node
        self.nodes.retain(|&node| node != crashed);
        self.drone_channels.remove(&crashed);
    }

    /// Spawn a new node of a specific type.
    fn spawn_node(&mut self, node_id: NodeId, node_type: NodeType) {
        println!("Spawning node {} of type {:?}", node_id, node_type);
        self.nodes.push(node_id);
    }

    /// Log a message sent event (for debugging or logging purposes).
    fn message_sent(&self, source: NodeId, target: NodeId) {
        println!("Message sent from node {} to node {}", source, target);
    }

    /// Handle events from drones.
    fn handle_node_event(&mut self, event: DroneEvent) {
        match event {
            DroneEvent::ControllerShortcut(packet) => {
                // Handle packets that need to be routed via the controller
                if let Some(&source) = packet.routing_header.hops.first() {
                    // Attempt to find the sender channel for the source
                    if let Some(sender) = self.drone_channels.get(&source) {
                        if let Err(err) = sender.send(packet) {
                            eprintln!("Failed to forward packet to source {}: {}", source, err);
                        }
                    } else {
                        eprintln!("Source drone {} not found in drone_channels.", source);
                    }
                } else {
                    eprintln!("Invalid routing header: no source found.");
                }
            },
            DroneEvent::PacketSent(packet) => {
                println!("Packet sent: {:?}", packet);
                // Additional logic for tracking sent packets can be added here
            },
            DroneEvent::PacketDropped(packet) => {
                println!("Packet dropped: {:?}", packet);
                // Handle dropped packets if necessary
            },
        }
    }
}