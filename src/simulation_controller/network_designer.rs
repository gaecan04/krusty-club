use eframe::egui;
use egui::{Color32, Painter, Pos2, Response, TextEdit};
use crossbeam_channel::Sender;
use wg_2024::controller::{DroneEvent, DroneCommand};
use wg_2024::network::NodeId;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use crate::simulation_controller::SC_backend::SimulationController;
use crate::network::initializer::ParsedConfig;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NodeType {
    Server,
    Client,
    Drone,
}

pub struct Node {
    pub id: usize,
    pub node_type: NodeType,
    pub pdr: f32,
    pub active: bool,
    pub position: (f32, f32),
}

impl Node {
    pub fn drone(id: usize, x: f32, y: f32,pdr:f32) -> Self {
        Self {
            id,
            node_type: NodeType::Drone,
            pdr,
            active: true,
            position: (x, y),
        }
    }

    pub fn server(id: usize, x: f32, y: f32) -> Self {
        Self {
            id,
            node_type: NodeType::Server,
            pdr: 1.0,
            active: true,
            position: (x, y),
        }
    }

    pub fn client(id: usize, x: f32, y: f32) -> Self {
        Self {
            id,
            node_type: NodeType::Client,
            pdr: 1.0,
            active: true,
            position: (x, y),
        }
    }
}

enum Topology {
    Star,
    DoubleLine,
    Butterfly,
    Custom,
}

impl Topology {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "star" => Some(Topology::Star),
            "double_line" => Some(Topology::DoubleLine),
            "butterfly" => Some(Topology::Butterfly),
            "custom" => Some(Topology::Custom),
            _ => None,
        }
    }
}

pub(crate) struct NetworkRenderer {
    pub(crate) nodes: Vec<Node>,
    edges: Vec<(usize, usize)>,
    selected_node: Option<usize>,
    pub scale: f32,
    controller_sender: Option<Sender<DroneEvent>>,
    simulation_controller: Option<Arc<Mutex<SimulationController>>>,
    pub(crate) node_id_to_index: HashMap<NodeId, usize>, // Mapping from node IDs to their index in the nodes vector
    config: Option<Arc<Mutex<ParsedConfig>>>, // Store reference to network config
    next_position_x: f32, // X coordinate for placing new nodes
    next_position_y: f32, // Y coordinate for placing new nodes

    // Connection dialog state
    add_connection_open: bool,
    target_node_id: String,
    pdr_value: f32,
    last_opened: Option<usize>,
}

impl NetworkRenderer {
    pub(crate) fn new(topology: &str, x_offset: f32, y_offset: f32) -> Self {
        let mut network = NetworkRenderer {
            nodes: Vec::new(),
            edges: Vec::new(),
            selected_node: None,
            scale: 1.0,
            controller_sender: None,
            simulation_controller: None,
            node_id_to_index: HashMap::new(),
            config: None,
            next_position_x: 100.0,
            next_position_y: 100.0,
            add_connection_open: false,
            target_node_id: String::new(),
            pdr_value: 0.0,
            last_opened: None,
        };

        match Topology::from_str(topology) {
            Some(Topology::Star) => network.setup_star(x_offset, y_offset),
            Some(Topology::DoubleLine) => network.setup_double_line(x_offset, y_offset),
            Some(Topology::Butterfly) => network.setup_butterfly(x_offset, y_offset),
            Some(Topology::Custom) => (), // Custom topology will be set up from config
            None => println!("Invalid topology"),
        }

        // Initialize the node_id_to_index map
        for (index, node) in network.nodes.iter().enumerate() {
            network.node_id_to_index.insert(node.id as NodeId, index);
        }

        network
    }

    // Create a network renderer directly from config
    pub(crate) fn new_from_config(topology: &str, x_offset: f32, y_offset: f32, config: Arc<Mutex<ParsedConfig>>) -> Self {
        let mut network = Self::new(topology, x_offset, y_offset);

        // Store the config
        network.config = Some(config.clone());

        // If this is a custom topology or we want to ensure consistency with config
        if topology == "custom" || topology == "star" || topology == "double_line" || topology == "butterfly" {
            network.build_from_config(config);
        }

        network
    }

    // Set simulation controller
    pub fn set_simulation_controller(&mut self, controller: Arc<Mutex<SimulationController>>) {
        self.simulation_controller = Some(controller);
    }

    // Set controller sender
    pub fn set_controller_sender(&mut self, sender: Sender<DroneEvent>) {
        self.controller_sender = Some(sender);
    }

    // Update method implementation
    pub fn update(&mut self) {
        // Update the network visualization state
        if let Some(config) = &self.config {
            // Refresh connections from config if needed
            self.sync_connections_with_config();
        }
    }

    // Sync connections with config to ensure they're consistent
    pub(crate) fn sync_connections_with_config(&mut self) {
        if let Some(config) = &self.config {
            let config = config.lock().unwrap();

            // Clear existing edges and rebuild them based on config
            self.edges.clear();

            // Add drone connections
            for drone in &config.drone {
                if let Some(&source_index) = self.node_id_to_index.get(&drone.id) {
                    for &target_id in &drone.connected_node_ids {
                        if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                            self.edges.push((source_index, target_index));
                        }
                    }
                }
            }

            // Add client connections
            for client in &config.client {
                if let Some(&source_index) = self.node_id_to_index.get(&client.id) {
                    for &target_id in &client.connected_drone_ids {
                        if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                            self.edges.push((source_index, target_index));
                        }
                    }
                }
            }

            // Add server connections
            for server in &config.server {
                if let Some(&source_index) = self.node_id_to_index.get(&server.id) {
                    for &target_id in &server.connected_drone_ids {
                        if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                            self.edges.push((source_index, target_index));
                        }
                    }
                }
            }
        }
    }

    pub fn sync_with_simulation_controller(&mut self) {
        // 1) collect which NodeIds have gone inactive
        let mut crashed_ids = Vec::new();
        if let Some(ctrl) = &self.simulation_controller {
            let ctrl = ctrl.lock().unwrap();
            for node in &mut self.nodes {
                if let Some(state) = ctrl.get_node_state(node.id as NodeId) {
                    node.active = state.active;
                    node.position = (state.position.0, state.position.1);
                    if !state.active {
                        crashed_ids.push(node.id as NodeId);
                    }
                }
            }
        }

        // 2) For each crashed NodeId, look up its index and call the old remover
        for node_id in crashed_ids {
            if let Some(&idx) = self.node_id_to_index.get(&node_id) {
                self.remove_edges_of_crashed_node(idx);
            }
        }

        // 3) rebuild any remaining edges from the up‐to‐date config, if you like
        self.sync_connections_with_config();
    }
    pub fn prune_inactive_edges(&mut self) {
        if let Some(ctrl_arc) = &self.simulation_controller {
            let ctrl = ctrl_arc.lock().unwrap();
            self.edges.retain(|&(a, b)| {
                let id_a = self.nodes[a].id as NodeId;
                let id_b = self.nodes[b].id as NodeId;
                let alive_a = ctrl.get_node_state(id_a).map(|s| s.active).unwrap_or(true);
                let alive_b = ctrl.get_node_state(id_b).map(|s| s.active).unwrap_or(true);
                alive_a && alive_b
            });
        }
    }



    // Update network method implementation
    pub fn update_network(&mut self) {
        // Update the simulation controller if needed
        if let Some(controller) = &self.simulation_controller {
            let mut controller = controller.lock().unwrap();
            // Update controller state based on current network configuration
            // (This would depend on your SimulationController implementation)
        }

        // Update local state
        self.update();
    }

    // Build network from config
    pub(crate) fn build_from_config(&mut self, config: Arc<Mutex<ParsedConfig>>) {
        let mut previous_states = HashMap::new();
        for node in &self.nodes {
            previous_states.insert(node.id, node.active);
        }
        // Clear existing nodes and edges
        self.nodes.clear();
        self.edges.clear();
        self.node_id_to_index.clear();

        let config = config.lock().unwrap();

        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        // Calculate layout parameters
        let num_drones = config.drone.len();
        let num_clients = config.client.len();
        let num_servers = config.server.len();

        // Calculate grid positions for nodes
        let total_nodes = num_drones + num_clients + num_servers;
        let grid_size = (total_nodes as f32).sqrt().ceil() as usize;
        let cell_width = WINDOW_WIDTH / (grid_size as f32 + 1.0);
        let cell_height = WINDOW_HEIGHT / (grid_size as f32 + 1.0);

        // Add drones
        let mut node_index = 0;
        for drone in &config.drone {
            let x = (node_index % grid_size) as f32 * cell_width + cell_width;
            let y = (node_index / grid_size) as f32 * cell_height + cell_height;

            // 👇 Recover the active state if it was set before
            let active = previous_states.get(&(drone.id as usize)).copied().unwrap_or(true);

            self.nodes.push(Node {
                id: drone.id as usize,
                node_type: NodeType::Drone,
                pdr: drone.pdr,
                active,
                position: (x, y),
            });

            self.node_id_to_index.insert(drone.id, self.nodes.len() - 1);
            node_index += 1;
        }


        // Add clients
        for client in &config.client {
            let x = (node_index % grid_size) as f32 * cell_width + cell_width;
            let y = (node_index / grid_size) as f32 * cell_height + cell_height;

            self.nodes.push(Node::client(client.id as usize, x, y));
            self.node_id_to_index.insert(client.id, self.nodes.len() - 1);
            node_index += 1;
        }

        // Add servers
        for server in &config.server {
            let x = (node_index % grid_size) as f32 * cell_width + cell_width;
            let y = (node_index / grid_size) as f32 * cell_height + cell_height;

            self.nodes.push(Node::server(server.id as usize, x, y));
            self.node_id_to_index.insert(server.id, self.nodes.len() - 1);
            node_index += 1;
        }

        // Add connections between nodes
        // Drone-to-drone connections
        for drone in &config.drone {
            if let Some(&source_index) = self.node_id_to_index.get(&drone.id) {
                for &target_id in &drone.connected_node_ids {
                    if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                        self.edges.push((source_index, target_index));
                    }
                }
            }
        }

        // Client-to-drone connections
        for client in &config.client {
            if let Some(&source_index) = self.node_id_to_index.get(&client.id) {
                for &target_id in &client.connected_drone_ids {
                    if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                        self.edges.push((source_index, target_index));
                    }
                }
            }
        }

        // Server-to-drone connections
        for server in &config.server {
            if let Some(&source_index) = self.node_id_to_index.get(&server.id) {
                for &target_id in &server.connected_drone_ids {
                    if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                        self.edges.push((source_index, target_index));
                    }
                }
            }
        }

        // Update next position coordinates for new nodes
        self.next_position_x = cell_width;
        self.next_position_y = (node_index / grid_size + 1) as f32 * cell_height;
    }

    // Add a new node to the network
    fn add_new_node(&mut self, node_type: NodeType) -> usize {
        // Find the next available ID
        let max_id = self.nodes.iter().map(|n| n.id).max().unwrap_or(0);
        let new_id = max_id + 1;

        // Create the new node at the next available position
        let new_node = match node_type {
            NodeType::Drone => Node::drone(new_id, self.next_position_x, self.next_position_y,0.5),
            NodeType::Server => Node::server(new_id, self.next_position_x, self.next_position_y),
            NodeType::Client => Node::client(new_id, self.next_position_x, self.next_position_y),

        };

        // Add the node to our list
        self.nodes.push(new_node);

        // Update the lookup map
        self.node_id_to_index.insert(new_id as NodeId, self.nodes.len() - 1);

        // Update the next position for subsequent nodes
        self.next_position_x += 50.0;
        if self.next_position_x > 550.0 {
            self.next_position_x = 50.0;
            self.next_position_y += 50.0;
        }

        // Update the config if available
        if let Some(config) = &self.config {
            let mut config = config.lock().unwrap();

            match node_type {
                NodeType::Drone => {
                    // Create a new drone in the config
                    config.add_drone(new_id as NodeId);
                },

                _ => {}
            }
        }

        new_id
    }



   /* fn add_drone_node(&mut self, id: NodeId, pdr: f32, connections: Vec<NodeId>) {
        let new_node = Node::drone(id as usize, self.next_position_x, self.next_position_y);
        self.nodes.push(new_node);
        self.node_id_to_index.insert(id, self.nodes.len() - 1);

        // Update layout
        self.next_position_x += 50.0;
        if self.next_position_x > 550.0 {
            self.next_position_x = 50.0;
            self.next_position_y += 50.0;
        }

        // Update config
        if let Some(config) = &self.config {
            let mut config = config.lock().unwrap();
            config.add_drone(id);
        }

        // Maybe also record PDR + connections if needed in your data structures
    }*/


    // Add a connection between two nodes
    fn add_connection(&mut self, source_id: usize, target_id: usize) -> bool {
        // Convert node IDs to indices
        if let (Some(&source_idx), Some(&target_idx)) = (
            self.node_id_to_index.get(&(source_id as NodeId)),
            self.node_id_to_index.get(&(target_id as NodeId))
        ) {
            // Check if the connection already exists
            if !self.edges.contains(&(source_idx, target_idx)) && !self.edges.contains(&(target_idx, source_idx)) {
                // Add the connection
                self.edges.push((source_idx, target_idx));

                // Update the config if available
                if let Some(config) = &self.config {
                    let mut cfg = config.lock().unwrap();

                    // Update connections in the config based on node types
                    let source_type = self.nodes[source_idx].node_type;

                    let a_type = self.nodes[source_idx].node_type;
                    let b_type = self.nodes[target_idx].node_type;

                    match (a_type, b_type) {
                        (NodeType::Drone, NodeType::Drone) => {
                            cfg.append_drone_connection(source_id as NodeId, target_id as NodeId);
                            cfg.append_drone_connection(target_id as NodeId, source_id as NodeId);
                        }
                        (NodeType::Drone, NodeType::Server) => {
                            cfg.append_drone_connection(source_id as NodeId, target_id as NodeId);
                            cfg.append_server_connection(target_id as NodeId, source_id as NodeId);
                        }
                        (NodeType::Server, NodeType::Drone) => {
                            cfg.append_drone_connection(target_id as NodeId, source_id as NodeId);
                            cfg.append_server_connection(source_id as NodeId, target_id as NodeId);
                        }
                        (NodeType::Drone, NodeType::Client) => {
                            // Drone → Client
                            let client = cfg.client.iter().find(|c| c.id == target_id as NodeId);
                            if let Some(c) = client {
                                if c.connected_drone_ids.len() >= 2 {
                                    eprintln!("Client {} already has 2 drone connections!", target_id);
                                    return false;
                                }
                            }

                            cfg.append_client_connection(target_id as NodeId, source_id as NodeId);
                            cfg.append_drone_connection(source_id as NodeId, target_id as NodeId);
                        }
                        (NodeType::Client, NodeType::Drone) => {
                            // Client → Drone
                            let client = cfg.client.iter().find(|c| c.id == source_id as NodeId);
                            if let Some(c) = client {
                                if c.connected_drone_ids.len() >= 2 {
                                    eprintln!("Client {} already has 2 drone connections!", source_id);
                                    return false;
                                }
                            }

                            cfg.append_client_connection(source_id as NodeId, target_id as NodeId);
                            cfg.append_drone_connection(target_id as NodeId, source_id as NodeId);
                        }
                        _ => {
                            eprintln!("Unsupported connection: {:?} ↔ {:?}", a_type, b_type);
                        }
                    }
                }
                if let Some(ctrl_arc) = &self.simulation_controller {
                    let mut ctrl = ctrl_arc.lock().unwrap();
                    ctrl.add_connection(source_id as NodeId, target_id as NodeId);
                }


                return true;
                }
            }

        false
    }


    fn setup_star(&mut self, _x_offset: f32, _y_offset: f32) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let num_drones = 10;
        let center_x = WINDOW_WIDTH / 2.0;
        let center_y = WINDOW_HEIGHT / 2.0;
        let radius = 100.0; // Reduced radius to fit within window

        // Create drones in a circular pattern
        for i in 0..num_drones {
            let angle = i as f32 * (std::f32::consts::PI * 2.0 / num_drones as f32);
            let x = center_x + radius * angle.cos();
            let y = center_y + radius * angle.sin();
            self.nodes.push(Node::drone(i, x, y,1.0));
        }

        // Connect drones with some edges
        for i in 0..num_drones {
            let target = (i + 3) % num_drones;
            self.edges.push((i, target));
        }

        // Place server outside the drone circle
        let server_id = num_drones;
        let server_x = center_x + radius + 50.0;
        let server_y = center_y;
        self.nodes.push(Node::server(server_id, server_x, server_y));

        // Connect server to specific drones
        self.edges.push((server_id, 0)); // Connect to first drone
        self.edges.push((server_id, 5)); // Connect to mid-range drone

        // Place clients outside the drone circle
        let client_1_id = num_drones + 1;
        let client_2_id = num_drones + 2;

        let client_1_x = center_x - radius - 50.0;
        let client_1_y = center_y - 50.0;

        let client_2_x = center_x - radius - 50.0;
        let client_2_y = center_y + 50.0;

        self.nodes.push(Node::client(client_1_id, client_1_x, client_1_y));
        self.nodes.push(Node::client(client_2_id, client_2_x, client_2_y));

        // Connect clients to their respective drones
        self.edges.push((client_1_id, 2));
        self.edges.push((client_2_id, 7));
    }

    fn setup_double_line(&mut self, _x_offset: f32, _y_offset: f32) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let line_spacing = (WINDOW_WIDTH - 100.0) / 4.0; // Distribute across window width
        let mut node_count = 0;

        // Create two lines of drones
        for i in 0..5 {
            let top_drone_x = 50.0 + i as f32 * line_spacing;
            let bottom_drone_x = 50.0 + i as f32 * line_spacing;

            let top_drone_y = WINDOW_HEIGHT / 3.0;
            let bottom_drone_y = 2.0 * WINDOW_HEIGHT / 3.0;

            self.nodes.push(Node::drone(node_count, top_drone_x, top_drone_y,1.0));
            node_count += 1;
            self.nodes.push(Node::drone(node_count, bottom_drone_x, bottom_drone_y,1.0));
            node_count += 1;

            // Connect vertical drones
            if i > 0 {
                self.edges.push((node_count - 2, node_count - 1));
            }

            // Connect horizontal drones
            if i > 0 {
                self.edges.push((node_count - 2, node_count - 4));
                self.edges.push((node_count - 1, node_count - 3));
            }
        }

        // Add server outside the drone lines
        let server_id = node_count;
        let server_x = WINDOW_WIDTH - 50.0;
        let server_y = WINDOW_HEIGHT / 6.0;
        self.nodes.push(Node::server(server_id, server_x, server_y));
        node_count += 1;

        // Connect server to nearby drones
        self.edges.push((server_id, 0)); // Connect to first top drone
        self.edges.push((server_id, 1)); // Connect to first bottom drone

        // Add clients outside the drone lines
        let client_1_id = node_count;
        let client_2_id = node_count + 1;

        // Client 1 below bottom line
        let client_1_x = 50.0;
        let client_1_y = WINDOW_HEIGHT - 50.0;

        // Client 2 above top line
        let client_2_x = WINDOW_WIDTH - 50.0;
        let client_2_y = 50.0;

        self.nodes.push(Node::client(client_1_id, client_1_x, client_1_y));
        self.nodes.push(Node::client(client_2_id, client_2_x, client_2_y));

        // Connect clients to their respective drones
        self.edges.push((client_1_id, 1));  // Connect to bottom line
        self.edges.push((client_2_id, 0));  // Connect to top line
    }

    fn setup_butterfly(&mut self, _x_offset: f32, _y_offset: f32) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let line_spacing = (WINDOW_WIDTH - 100.0) / 4.0; // Distribute across window width
        let mut node_count = 0;

        // Create two lines of drones with a 'wing' effect
        for i in 0..5 {
            let left_drone_x = 50.0 + i as f32 * line_spacing;
            let right_drone_x = WINDOW_WIDTH - 50.0 - i as f32 * line_spacing;

            let left_drone_y = WINDOW_HEIGHT / 3.0;
            let right_drone_y = 2.0 * WINDOW_HEIGHT / 3.0;

            self.nodes.push(Node::drone(node_count, left_drone_x, left_drone_y,1.0));
            node_count += 1;
            self.nodes.push(Node::drone(node_count, right_drone_x, right_drone_y,1.0));
            node_count += 1;

            // Connect vertical drones
            if i > 0 {
                self.edges.push((node_count - 2, node_count - 1));
            }

            // Connect horizontal drones
            if i > 0 {
                self.edges.push((node_count - 2, node_count - 4));
                self.edges.push((node_count - 1, node_count - 3));
            }
        }

        // Add server outside the drone lines
        let server_id = node_count;
        let server_x = WINDOW_WIDTH / 2.0;
        let server_y = 50.0;
        self.nodes.push(Node::server(server_id, server_x, server_y));
        node_count += 1;

        // Connect server to nearby drones
        self.edges.push((server_id, 0)); // Connect to first top drone
        self.edges.push((server_id, 1)); // Connect to first bottom drone

        // Add clients outside the drone lines
        let client_1_id = node_count;
        let client_2_id = node_count + 1;

        // Client 1 below bottom line
        let client_1_x = 50.0;
        let client_1_y = WINDOW_HEIGHT - 50.0;

        // Client 2 above top line
        let client_2_x = WINDOW_WIDTH - 50.0;
        let client_2_y = 50.0;

        self.nodes.push(Node::client(client_1_id, client_1_x, client_1_y));
        self.nodes.push(Node::client(client_2_id, client_2_x, client_2_y));

        // Connect clients to their respective drones
        self.edges.push((client_1_id, 1));  // Connect to bottom line
        self.edges.push((client_2_id, 0));  // Connect to top line
    }

    /// node_id is the actual NodeId (u8) — we look up its internal index before removing.
    pub(crate) fn remove_edges_of_crashed_node(&mut self, idx: usize) {
        self.edges.retain(|&(a, b)| a != idx && b != idx);

        // Update the config if available
        if let Some(cfg_arc) = &self.config {
                    let mut cfg = cfg_arc.lock().unwrap();

            // Find the node in the node_id_to_index map
                   let node_type = self.nodes[idx].node_type;

                // Update connections in the config based on node types
                match node_type {
                    NodeType::Drone => {
                        cfg.remove_drone_connections(self.nodes[idx].id as NodeId);
                    },
                    NodeType::Client => {
                        cfg.remove_client_connections(self.nodes[idx].id as NodeId);
                    },
                    NodeType::Server => {
                        cfg.remove_server_connections(self.nodes[idx].id as NodeId);

                },
                }

        }
    }


        /// Remove every edge incident on `node_id`, and clean up the config.
    /*pub fn remove_edges_of_crashed_node(&mut self, node_id: usize) {
        // 1) Find the index in `self.nodes` for that NodeId.
        if let Some(&idx) = self.node_id_to_index.get(&node_id) {
            // 2) Drop all edges touching that index.
            self.edges.retain(|&(a, b)| a != idx && b != idx);

            // 3) Update the toml-backed config so future rebuilds skip this node.
            if let Some(cfg_arc) = &self.config {
                let mut cfg = cfg_arc.lock().unwrap();
                cfg.remove_drone_connections(node_id);
                cfg.remove_client_connections(node_id);
                cfg.remove_server_connections(node_id);
            }
        }
    }*/




    pub fn render(&mut self, ui: &mut egui::Ui) {
        // Draw network controls
        ui.horizontal(|ui| {
            if ui.button("Add Drone").clicked() {
                let new_id = self.add_new_node(NodeType::Drone);
                // Select the new node
                self.selected_node = Some(new_id);
            }

            if ui.button("Add Client").clicked() {
                let new_id = self.add_new_node(NodeType::Client);
                // Select the new node
                self.selected_node = Some(new_id);
            }

            if ui.button("Add Server").clicked() {
                let new_id = self.add_new_node(NodeType::Server);
                // Select the new node
                self.selected_node = Some(new_id);
            }

            if ui.button("Refresh Network").clicked() {
                if let Some(config) = &self.config {
                    self.build_from_config(config.clone());
                }
            }
        });

        // Draw edges first
        {
            let painter = ui.painter();
            for &(a, b) in &self.edges {
                if a < self.nodes.len() && b < self.nodes.len() {
                    let pos_a = Pos2::new(self.nodes[a].position.0, self.nodes[a].position.1);
                    let pos_b = Pos2::new(self.nodes[b].position.0, self.nodes[b].position.1);

                    let node_a_type = self.nodes[a].node_type;
                    let node_b_type = self.nodes[b].node_type;

                    // Determine edge color based on what it's connected to
                    let color = if node_a_type == NodeType::Server || node_b_type == NodeType::Server {
                        Color32::BLUE
                    } else if node_a_type == NodeType::Client || node_b_type == NodeType::Client {
                        Color32::YELLOW
                    } else {
                        Color32::GRAY
                    };

                    painter.line_segment([pos_a, pos_b], (2.0, color));
                }
            }

        }

        // Handle node interaction
        for (idx, node) in self.nodes.iter().enumerate() {
            let pos = Pos2::new(node.position.0, node.position.1);
            let color = match node.node_type {
                NodeType::Server => Color32::BLUE,
                NodeType::Client => Color32::YELLOW,
                NodeType::Drone => if node.active { Color32::GREEN } else { Color32::RED },
            };

            // Handle interaction (allocate_rect) first, before using painter
            let response = ui.allocate_rect(
                egui::Rect::from_center_size(pos, egui::vec2(20.0, 20.0)),
                egui::Sense::click()
            );

            // Now we can safely draw the node circle using the painter
            {
                let painter = ui.painter();
                painter.circle(pos, 10.0, color, (1.0, Color32::BLACK));

                // Draw node ID next to the node
                painter.text(
                    Pos2::new(pos.x + 15.0, pos.y - 10.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{}", node.id),
                    egui::FontId::proportional(14.0),
                    Color32::BLACK,
                );
            }

            // Handle click interaction
            if response.clicked() {
                self.selected_node = Some(idx);
                if self.last_opened != Some(idx) {
                    self.pdr_value = self.nodes[idx].pdr;
                    self.last_opened = Some(idx);
                }

            }
        }
    }

    pub fn render_node_details(&mut self, ctx: &egui::Context) {
        if let Some(idx) = self.selected_node {
            // latch initial PDR the first time we open this node
            if self.last_opened != Some(idx) {
                self.pdr_value   = self.nodes[idx].pdr;
                self.last_opened = Some(idx);
            }

            let mut should_close = false;
            let mut should_crash = false;

            let node_id   = self.nodes[idx].id as NodeId;
            let node_type = self.nodes[idx].node_type;

            egui::Window::new("Node Information")
                .collapsible(false)
                .open(&mut self.selected_node.is_some())
                .show(ctx, |ui| {
                    ui.label(format!("Node ID: {}", node_id));
                    ui.label(match node_type {
                        NodeType::Server => "Type: Server".to_string(),
                        NodeType::Client => "Type: Client".to_string(),
                        NodeType::Drone  => "Type: Drone".to_string(),
                    });
                    // Show neighbors from the config
                    if let Some(cfg) = &self.config {
                        let cfg = cfg.lock().unwrap();

                        let neighbors = match node_type {
                            NodeType::Drone => cfg.drone.iter()
                                .find(|d| d.id == node_id)
                                .map(|d| d.connected_node_ids.clone())
                                .unwrap_or_default(),

                            NodeType::Client => cfg.client.iter()
                                .find(|c| c.id == node_id)
                                .map(|c| c.connected_drone_ids.clone())
                                .unwrap_or_default(),

                            NodeType::Server => cfg.server.iter()
                                .find(|s| s.id == node_id)
                                .map(|s| s.connected_drone_ids.clone())
                                .unwrap_or_default(),
                        };

                        let list = if neighbors.is_empty() {
                            "(none)".to_string()
                        } else {
                            neighbors.iter().map(|id| format!("{}", id)).collect::<Vec<_>>().join(", ")
                        };

                        ui.label(format!("Connected to: [{}]", list));
                    }


                    if let NodeType::Drone = node_type {
                        if self.nodes[idx].active {
                            // ✅ Draw editable controls only if the drone is active

                            let mut new_pdr = self.pdr_value;
                            ui.add(egui::Slider::new(&mut new_pdr, 0.0..=1.0).text("PDR"));

                            if (new_pdr - self.pdr_value).abs() > f32::EPSILON {
                                self.pdr_value = new_pdr;
                                self.nodes[idx].pdr = new_pdr;

                                if let Some(cfg) = &self.config {
                                    cfg.lock().unwrap().set_drone_pdr(node_id, new_pdr);
                                }

                                if let Some(ctrl) = &self.simulation_controller {
                                    let _ = ctrl.lock().unwrap().set_packet_drop_rate(node_id, new_pdr);
                                }
                            }

                            ui.horizontal(|ui| {
                                if ui.button("Crash Node").clicked() {
                                    should_crash = true;
                                }

                                let mut pending_connection: Option<NodeId> = None;

                                egui::ComboBox::from_label("Connect to:")
                                    .selected_text("select peer")
                                    .show_ui(ui, |ui| {
                                        for peer in self.nodes.iter() {
                                            if peer.active && peer.id as NodeId != node_id {
                                                let label = match peer.node_type {
                                                    NodeType::Drone => format!("Drone {}", peer.id),
                                                    NodeType::Client => format!("Client {}", peer.id),
                                                    NodeType::Server => format!("Server {}", peer.id),
                                                };
                                                if ui.selectable_label(false, label).clicked() {
                                                    pending_connection = Some(peer.id as NodeId);
                                                    ui.close_menu();
                                                }
                                            }
                                        }
                                    });

                                if let Some(peer_id) = pending_connection {
                                    self.add_connection(node_id as usize, peer_id as usize);
                                }
                            });
                        } else {
                            // 🚫 Read-only mode for inactive drone
                            ui.label("This drone is inactive (crashed). PDR and connections cannot be changed.");
                        }
                    }

                    if ui.button("Close").clicked() {
                        should_close = true;
                    }
                });

            // Crash logic
            if should_crash {
                let crash_allowed = if let Some(ctrl_arc) = &self.simulation_controller {
                    let mut ctrl = ctrl_arc.lock().unwrap();
                    match ctrl.crash_drone(node_id as NodeId) {
                        Ok(_) => true,
                        Err(e) => {
                            eprintln!("SC refused to crash {}: {}", node_id, e);
                            false
                        }
                    }
                } else {
                    false
                };

                if crash_allowed {
                    // ✅ Now it's safe to mutably borrow self
                    self.nodes[idx].active = false;

                    if let Some(cfg_arc) = &self.config {
                        let mut cfg = cfg_arc.lock().unwrap();
                        cfg.remove_drone_connections(node_id as NodeId);
                    }

                    if let Some(cfg_arc) = &self.config {
                        self.build_from_config(cfg_arc.clone());
                    }
                }
            }



            // Close and reset
            if should_close {
                self.selected_node = None;
                self.last_opened   = None;
            }
        }
    }
    pub fn get_drone_ids(&self) -> Vec<NodeId> {
        self.nodes.iter()
            .filter(|node| matches!(node.node_type, NodeType::Drone) && node.active)
            .map(|node| node.id as NodeId)
            .collect()
    }

    pub fn get_client_ids(&self) -> Vec<NodeId> {
        self.nodes.iter()
            .filter(|node| matches!(node.node_type, NodeType::Client))
            .map(|node| node.id as NodeId)
            .collect()
    }

    pub fn get_server_ids(&self) -> Vec<NodeId> {
        self.nodes.iter()
            .filter(|node| matches!(node.node_type, NodeType::Server))
            .map(|node| node.id as NodeId)
            .collect()
    }


    pub fn add_drone(&mut self, id: NodeId, pdr: f32, connections: Vec<NodeId>) {
        // Calculate position for the new drone
        let x = self.next_position_x;
        let y = self.next_position_y;

        // Create new drone node
        let new_node = Node::drone(id as usize, x, y,pdr);

        // Add the node to our list
        let new_node_index = self.nodes.len();
        self.nodes.push(new_node);

        // Update the lookup map
        self.node_id_to_index.insert(id, new_node_index);

        // Add connections to the specified nodes
        for &target_id in &connections {
            if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                self.edges.push((new_node_index, target_index));
            }
        }

        // Update the next position for subsequent nodes
        self.next_position_x += 50.0;
        if self.next_position_x > 550.0 {
            self.next_position_x = 50.0;
            self.next_position_y += 50.0;
        }

        // Update the config if available
        if let Some(config) = &self.config {
            let mut config = config.lock().unwrap();

            // Add the drone to the config
            config.add_drone(id);

            // Add connections in the config
            for &target_id in &connections {
                config.set_drone_connections(id, vec![target_id]);
            }
        }
    }

}
