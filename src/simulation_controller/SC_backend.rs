use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::{Arc, Mutex};
use crossbeam_channel::{Receiver, Sender};
use wg_2024::packet::Packet;
use wg_2024::controller::{DroneCommand,DroneEvent};
use wg_2024::network::NodeId;
use crate::network::initializer::ParsedConfig;

//Reminder about structs
/*pub enum DroneCommand {
    RemoveSender(usize), // NodeId
    AddSender(usize, Sender<Packet>), // NodeId, Sender
    SetPacketDropRate(f32),
    Crash,
}
pub enum DroneEvent {
    PacketSent(Packet),
    PacketDropped(Packet),
    ControllerShortcut(Packet),
}*/

pub struct SimulationController {
    network_config: Arc<Mutex<ParsedConfig>>,
    event_receiver: Receiver<DroneEvent>,
    command_senders: HashMap<NodeId, Sender<DroneCommand>>, // Map of NodeId -> CommandSender
    network_graph: HashMap<NodeId, HashSet<NodeId>>, // Adjacency list of the network
}

impl SimulationController {
    pub fn new(
        network_config: Arc<Mutex<ParsedConfig>>,
        event_receiver: Receiver<DroneEvent>
    ) -> Self {
        let mut controller = SimulationController {
            network_config,
            event_receiver,
            command_senders: HashMap::new(),
            network_graph: HashMap::new(),
        };

        // Initialize the network graph
        controller.initialize_network_graph();

        controller
    }

    // Initialize network graph from config
    fn initialize_network_graph(&mut self) {
        let config = self.network_config.lock().unwrap();

        // Add drone connections
        for drone in &config.drone {
            let mut neighbors = HashSet::new();
            for &neighbor_id in &drone.connected_node_ids {
                neighbors.insert(neighbor_id);
            }
            self.network_graph.insert(drone.id, neighbors);
        }

        // Add client connections
        for client in &config.client {
            let mut neighbors = HashSet::new();
            for &drone_id in &client.connected_drone_ids {
                neighbors.insert(drone_id);

                // Add bidirectional connection
                self.network_graph.entry(drone_id)
                    .or_insert_with(HashSet::new)
                    .insert(client.id);
            }
            self.network_graph.insert(client.id, neighbors);
        }

        // Add server connections
        for server in &config.server {
            let mut neighbors = HashSet::new();
            for &drone_id in &server.connected_drone_ids {
                neighbors.insert(drone_id);

                // Add bidirectional connection
                self.network_graph.entry(drone_id)
                    .or_insert_with(HashSet::new)
                    .insert(server.id);
            }
            self.network_graph.insert(server.id, neighbors);
        }
    }

    // Register a command sender for a node
    pub fn register_command_sender(&mut self, node_id: NodeId, sender: Sender<DroneCommand>) {
        self.command_senders.insert(node_id, sender);
    }

    // Main loop to process events
    pub fn run(&mut self) {
        while let Ok(event) = self.event_receiver.recv() {
            self.process_event(event);
        }
    }

    // Process a drone event
    fn process_event(&mut self, event: DroneEvent) {
        match event {
            DroneEvent::PacketSent(packet) => {
                // Log packet sent event
                println!("Packet sent from {} to {}", packet.session_id, packet.routing_header.hops[packet.routing_header.hops.len() - 1]);
            },
            DroneEvent::PacketDropped(packet) => {
                // Log packet dropped event
                println!("Packet dropped from {} to {}", packet.session_id, packet.routing_header.hops[packet.routing_header.hops.len() - 1]);
            },
            DroneEvent::ControllerShortcut(packet) => {
                // Handle direct routing for critical packets
                if let Some(sender) = self.command_senders.get(&packet.routing_header.hops[packet.routing_header.hops.len() - 1]) {
                    // Direct delivery to destination
                    // This is a simplified example - in reality, you would

                    // likely need to transform the packet in some way
                    println!("Controller shortcut: Directly routing packet to {}", packet.routing_header.hops[packet.routing_header.hops.len() - 1]);
                }
            }
        }
    }

    // Command to make a drone crash
    pub fn crash_drone(&mut self, drone_id: NodeId) -> Result<(), Box<dyn Error>> {
        // Validate that removing this drone won't disconnect the network
        if !self.validate_drone_crash(drone_id)? {
            return Err("Crashing this drone would violate network constraints".into());
        }

        // Get the neighbors of the drone
        if let Some(neighbors) = self.network_graph.get(&drone_id) {
            // Clone to avoid borrowing issues
            let neighbors_clone: Vec<NodeId> = neighbors.iter().cloned().collect();

            // First, remove the drone from its neighbors' sender lists
            for neighbor_id in neighbors_clone.clone() {
                if let Some(sender) = self.command_senders.get(&neighbor_id) {
                    sender.send(DroneCommand::RemoveSender(drone_id))
                        .map_err(|_| "Failed to send RemoveSender command")?;
                }
            }

            // Then, send the crash command to the drone
            if let Some(sender) = self.command_senders.get(&drone_id) {
                sender.send(DroneCommand::Crash)
                    .map_err(|_| "Failed to send Crash command")?;
            }

            // Update our internal representation of the network
            for neighbor_id in neighbors_clone {
                if let Some(neighbors) = self.network_graph.get_mut(&neighbor_id) {
                    neighbors.remove(&drone_id);
                }
            }

            // Remove the drone from our graph
            self.network_graph.remove(&drone_id);
            self.command_senders.remove(&drone_id);
        }

        Ok(())
    }

    // Validate that a drone crash won't violate network constraints
    fn validate_drone_crash(&self, drone_id: NodeId) -> Result<bool, Box<dyn Error>> {
        let config = self.network_config.lock().unwrap();

        // Check if the drone exists
        if !self.network_graph.contains_key(&drone_id) {
            return Err("Drone does not exist".into());
        }

        // Create a copy of the graph without the drone to check connectivity
        let mut test_graph = self.network_graph.clone();

        // Remove the drone and its connections
        if let Some(neighbors) = test_graph.remove(&drone_id) {
            for &neighbor_id in &neighbors {
                if let Some(neighbor_neighbors) = test_graph.get_mut(&neighbor_id) {
                    neighbor_neighbors.remove(&drone_id);
                }
            }
        }

        // Check if the network remains connected
        if !self.is_connected(&test_graph) {
            return Ok(false);
        }

        // Check client constraints
        for client in &config.client {
            let connected_drones: Vec<NodeId> = client.connected_drone_ids
                .iter()
                .filter(|&&id| id != drone_id)
                .cloned()
                .collect();

            if connected_drones.is_empty() || connected_drones.len() > 2 {
                return Ok(false);
            }
        }

        // Check server constraints
        for server in &config.server {
            let connected_drones: Vec<NodeId> = server.connected_drone_ids
                .iter()
                .filter(|&&id| id != drone_id)
                .cloned()
                .collect();

            if connected_drones.len() < 2 {
                return Ok(false);
            }
        }

        Ok(true)
    }

    // Check if a graph is connected using BFS
    fn is_connected(&self, graph: &HashMap<NodeId, HashSet<NodeId>>) -> bool {
        if graph.is_empty() {
            return true;
        }

        let start_node = *graph.keys().next().unwrap();
        let mut visited = HashSet::new();
        let mut queue = vec![start_node];

        while let Some(node) = queue.pop() {
            if visited.insert(node) {
                if let Some(neighbors) = graph.get(&node) {
                    for &neighbor in neighbors {
                        if !visited.contains(&neighbor) {
                            queue.push(neighbor);
                        }
                    }
                }
            }
        }

        visited.len() == graph.len()
    }

    // Set the packet drop rate for a drone
    pub fn set_packet_drop_rate(&mut self, drone_id: NodeId, rate: f32) -> Result<(), Box<dyn Error>> {
        if let Some(sender) = self.command_senders.get(&drone_id) {
            sender.send(DroneCommand::SetPacketDropRate(rate))
                .map_err(|_| "Failed to send SetPacketDropRate command".into())
        } else {
            Err("Drone not found".into())
        }
    }

    // Spawn a new drone in the network
    pub fn spawn_drone(&mut self, id: NodeId, pdr: f32, connections: Vec<NodeId>) -> Result<(), Box<dyn Error>> {
        // Validate the new drone's connections
        if !self.validate_new_drone(id, &connections)? {
            return Err("New drone configuration violates network constraints".into());
        }

        // Create a new drone in the configuration
        let mut config = self.network_config.lock().unwrap();

        // Add the drone to the configuration
        // This is a simplified example - you would need to create the actual drone object
        // and update the configuration properly

        // Update the network graph
        let mut neighbors = HashSet::new();
        for &conn_id in &connections {
            neighbors.insert(conn_id);

            // Update the connected node's neighbors
            if let Some(conn_neighbors) = self.network_graph.get_mut(&conn_id) {
                conn_neighbors.insert(id);
            }
        }
        self.network_graph.insert(id, neighbors);

        // In a real implementation, you would also need to create channels for
        // communication with the new drone and set up the actual drone object

        Ok(())
    }

    // Validate that adding a new drone won't violate network constraints
    fn validate_new_drone(&self, id: NodeId, connections: &[NodeId]) -> Result<bool, Box<dyn Error>> {
        // Check for duplicate ID
        if self.network_graph.contains_key(&id) {
            return Err("Drone ID already exists".into());
        }

        // Check that all connections exist in the network
        for &conn_id in connections {
            if !self.network_graph.contains_key(&conn_id) {
                return Err(format!("Connection node {} does not exist", conn_id).into());
            }
        }

        // Check that the network will remain valid
        // (This is a simplified check - you might need more validation)
        Ok(true)
    }
}