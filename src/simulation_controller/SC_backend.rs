use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::{Arc, Mutex};
use crossbeam_channel::{Receiver, Sender};
use wg_2024::packet::Packet;
use wg_2024::controller::{DroneCommand,DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use crate::network::initializer::{ParsedConfig};
use crate::simulation_controller::network_designer::{Node, NodeType};
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
    event_sender: Sender<DroneEvent>,
    command_senders: HashMap<NodeId, Sender<DroneCommand>>, // Map of NodeId -> CommandSender
    network_graph: HashMap<NodeId, HashSet<NodeId>>, // Adjacency list of the network
    packet_senders: HashMap<NodeId, Sender<Packet>>,
    drone_factory: Arc<dyn Fn(
        NodeId,
        Sender<DroneEvent>,
        Receiver<DroneCommand>,
        Receiver<Packet>,
        HashMap<NodeId, Sender<Packet>>,
        f32,
    ) -> Box<dyn Drone> + Send + Sync>,
}

impl SimulationController {
    pub fn new(
        network_config: Arc<Mutex<ParsedConfig>>,
        event_sender: Sender<DroneEvent>,
        event_receiver: Receiver<DroneEvent>,
        drone_factory: Arc<dyn Fn(NodeId,Sender<DroneEvent>,Receiver<DroneCommand>,Receiver<Packet>,HashMap<NodeId, Sender<Packet>>,f32, ) -> Box<dyn Drone> + Send + Sync>,

    ) -> Self {
        let mut controller = SimulationController {
            network_config,
            event_sender,
            event_receiver,
            command_senders: HashMap::new(),
            network_graph: HashMap::new(),
            packet_senders: HashMap::new(),
            drone_factory,
        };

        // Initialize the network graph
        controller.initialize_network_graph();

        controller
    }

    pub fn get_node_state(&self, node_id: NodeId) -> Option<Node> {
        let config = self.network_config.lock().unwrap();

        // We use a dummy/default position since position is not available
        let default_position = (0.0, 0.0);

        for drone in &config.drone {
            if drone.id == node_id {
                return Some(Node {
                    id: node_id as usize,
                    node_type: NodeType::Drone,
                    pdr: drone.pdr,
                    active: self.network_graph.contains_key(&node_id),
                    position: default_position, // fallback
                });
            }
        }

        for client in &config.client {
            if client.id == node_id {
                return Some(Node {
                    id: node_id as usize,
                    node_type: NodeType::Client,
                    pdr: 1.0,
                    active: self.network_graph.contains_key(&node_id),
                    position: default_position,
                });
            }
        }

        for server in &config.server {
            if server.id == node_id {
                return Some(Node {
                    id: node_id as usize,
                    node_type: NodeType::Server,
                    pdr: 1.0,
                    active: self.network_graph.contains_key(&node_id),
                    position: default_position,
                });
            }
        }

        None
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

    pub fn register_packet_sender(&mut self, node_id: NodeId, sender: Sender<Packet>) {
        self.packet_senders.insert(node_id, sender);
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
                if let Some(dest_id) = packet.routing_header.destination() {
                    if let Some(sender) = self.packet_senders.get(&dest_id) {
                        if let Err(e) = sender.send(packet.clone()) {
                            eprintln!("Failed to forward ControllerShortcut to node {}: {}", dest_id, e);
                        } else {
                            println!("ControllerShortcut forwarded to node {}", dest_id);
                        }
                    } else {
                        eprintln!("No packet sender found for destination node {}", dest_id);
                    }
                } else {
                    eprintln!("ControllerShortcut has no destination");
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

        // Update internal config
        {
            let mut config = self.network_config.lock().unwrap();
            config.add_drone(id);
            config.set_drone_connections(id, connections.clone());
        }

        // Update network graph
        let mut neighbors = HashSet::new();
        for &conn_id in &connections {
            neighbors.insert(conn_id);
            self.network_graph.entry(conn_id).or_default().insert(id);
        }
        self.network_graph.insert(id, neighbors);

        // Create command channel (controller → drone)
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        self.command_senders.insert(id, command_tx.clone());

        // Create packet channel (to drone)
        let (packet_tx, packet_rx) = crossbeam_channel::unbounded();
        self.packet_senders.insert(id, packet_tx.clone());

        // Create packet senders map for this drone (to neighbors)
        let mut packet_send_map = HashMap::new();
        for &conn_id in &connections {
            if let Some(sender) = self.packet_senders.get(&conn_id) {
                packet_send_map.insert(conn_id, sender.clone());
            }
        }

        // Clone for drone thread
        let controller_send = self.event_sender.clone();
        let factory = Arc::clone(&self.drone_factory);

        // Spawn drone in its own thread
        std::thread::spawn(move || {
            let mut drone = factory(id, controller_send, command_rx, packet_rx, packet_send_map, pdr);
            drone.run();
        });

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

