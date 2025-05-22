use eframe::egui;
use egui::{Color32, Context, Painter, Pos2, Response, TextEdit, Vec2};
use crossbeam_channel::Sender;
use wg_2024::controller::{DroneEvent, DroneCommand};
use wg_2024::network::NodeId;
use std::sync::{Arc, Mutex};
use std::collections::{HashMap, HashSet};
use crate::simulation_controller::SC_backend::SimulationController;
use crate::network::initializer::ParsedConfig;
use crate::simulation_controller::gui_input_queue::broadcast_topology_change;

use egui::epaint::PathShape;


#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NodeType {
    Server,
    Client,
    Drone,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: usize,
    pub node_type: NodeType,
    pub pdr: f32,
    pub active: bool,
    pub position: (f32, f32),
    pub manual_position: bool,  // ⭐ NEW
}

impl Node {
    pub fn drone(id: usize, x: f32, y: f32,pdr:f32) -> Self {
        Self {
            id,
            node_type: NodeType::Drone,
            pdr,
            active: true,
            position: (x, y),
            manual_position: false,  // default
        }
    }

    pub fn server(id: usize, x: f32, y: f32) -> Self {
        Self {
            id,
            node_type: NodeType::Server,
            pdr: 1.0,
            active: true,
            position: (x, y),
            manual_position: false,
        }
    }

    pub fn client(id: usize, x: f32, y: f32) -> Self {
        Self {
            id,
            node_type: NodeType::Client,
            pdr: 1.0,
            active: true,
            position: (x, y),
            manual_position: false,
        }
    }
}

enum Topology {
    Star,
    DoubleChain,
    Butterfly,
    Tree,
    SubNet,
    Custom,
}

impl Topology {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "star" => Some(Topology::Star),
            "double_chain" => Some(Topology::DoubleChain),
            "butterfly" => Some(Topology::Butterfly),
            "tree" => Some(Topology::Tree),
            "sub-net" => Some(Topology::SubNet),
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

    //for icons=photos
    drone_texture: Option<egui::TextureHandle>,
    client_texture: Option<egui::TextureHandle>,
    server_texture: Option<egui::TextureHandle>,
    fire: Option<egui::TextureHandle>,
    // for crash/spawn go-back to grid prob
    current_topology: Option<String>,
    //for the overwrite spawn prob
    manual_positions: HashMap<NodeId, (f32, f32)>,
    last_spawned_position: Option<(f32, f32)>,



}

impl NetworkRenderer {
    pub(crate) fn new(topology: &str, x_offset: f32, y_offset: f32,ctx: &egui::Context) -> Self {
        let drone_texture = Some(load_texture(ctx, "assets/drone.png"));
        let client_texture = Some(load_texture(ctx, "assets/client.png"));
        let server_texture = Some(load_texture(ctx, "assets/server.png"));
        let fire = Some(load_texture(ctx, "assets/fire.png"));



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
            drone_texture,
            client_texture,
            server_texture,
            fire,
            current_topology: None,
            manual_positions: HashMap::new(),
            last_spawned_position: None,
        };

        match Topology::from_str(topology) {
            Some(Topology::Star) => network.setup_star(x_offset, y_offset),
            Some(Topology::DoubleChain) => network.setup_double_chain(x_offset, y_offset),
            Some(Topology::Butterfly) => network.setup_butterfly(x_offset, y_offset),
            Some(Topology::Tree) => network.setup_tree(x_offset, y_offset),
            Some(Topology::SubNet) => network.setup_subnet(x_offset, y_offset),

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
    pub(crate) fn new_from_config(topology: &str, x_offset: f32, y_offset: f32, config: Arc<Mutex<ParsedConfig>>,ctx: &egui::Context) -> Self {
        let mut network = Self::new(topology, x_offset, y_offset,ctx); // Force base to be empty

        // Store the config
        network.config = Some(config.clone());

        // Always build strictly from config
        network.build_from_config(config);

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
        //self.sync_connections_with_config();
        self.prune_inactive_edges();


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
        let previous_nodes = self.nodes.clone();


        self.nodes.clear();
        self.edges.clear();
        self.node_id_to_index.clear();

        let config = config.lock().unwrap();

        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let detected_topo = config.detect_topology();
        if let Some(topo_name) = detected_topo.clone() {
            self.current_topology = Some(topo_name.clone());
            println!("✅ Detected topology: {}", topo_name);
            self.build_topology_layout(&topo_name, &config, &previous_states, &previous_nodes);
        }else if let Some(prev_topo) = self.current_topology.clone() {
            println!("⚠️ Detection failed after crash. Falling back to last known topology: {}", prev_topo);
            self.build_topology_layout(&prev_topo, &config, &previous_states, &previous_nodes);
        } else {
            println!("❌ No known topology. Falling back to grid layout.");
            self.build_grid(&config, &previous_states);
        }


        self.next_position_x = 50.0;
        self.next_position_y = WINDOW_HEIGHT - 50.0;
    }

    pub fn rebuild_preserving_topology(&mut self, config: Arc<Mutex<ParsedConfig>>) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let mut previous_states = HashMap::new();
        for node in &self.nodes {
            previous_states.insert(node.id, node.active);
        }
        let previous_nodes = self.nodes.clone();
        let mut newly_spawned_nodes = Vec::new();

        // Keep track of existing node IDs before clearing
        let existing_node_ids: HashSet<NodeId> = self.nodes.iter()
            .map(|node| node.id as NodeId)
            .collect();

        self.nodes.clear();
        self.edges.clear();
        self.node_id_to_index.clear();

        let config = config.lock().unwrap();

        // Process nodes differently based on whether they are new or existing
        for drone in &config.drone {
            let is_new_node = !existing_node_ids.contains(&drone.id);

            // Handle new nodes with special positioning
            if is_new_node && !drone.connected_node_ids.is_empty() {

                // This is a new drone - position it intelligently
                let (x, y) = self.position_new_drone(drone.id, &drone.connected_node_ids);

                self.nodes.push(Node {
                    id: drone.id as usize,
                    node_type: NodeType::Drone,
                    pdr: drone.pdr,
                    active: true, // New nodes are active by default
                    position: (x, y),
                    manual_position: false,
                });

                self.node_id_to_index.insert(drone.id, self.nodes.len() - 1);

                // Save this position as manual so it won't be repositioned
                self.manual_positions.insert(drone.id, (x, y));
                newly_spawned_nodes.push(drone.id); // 👈 Track added

                // Skip this drone in the regular layout routine
                continue;
            }
        }

        // Now process the rest of the nodes with normal layout
        if let Some(ref topo) = self.current_topology.clone() {
            println!("🔄 Rebuilding using saved topology: {}", topo);


            self.build_topology_layout(&topo, &config, &previous_states, &previous_nodes);
        } else {
            println!("⚠️ No saved topology, falling back to grid layout");
            self.build_grid(&config, &previous_states);
        }

        self.next_position_x = 50.0;
        self.next_position_y = WINDOW_HEIGHT - 50.0;
    }

    pub fn position_new_drone(&mut self, new_drone_id: NodeId, connections: &[NodeId]) -> (f32, f32) {
        if let Some((last_x, last_y)) = self.last_spawned_position {
            // If a drone was already spawned, offset from last
            let new_x = last_x + 50.0; // move to the right by 50
            let new_y = last_y + 20.0; // move slightly down
            self.last_spawned_position = Some((new_x, new_y));
            return (new_x, new_y);
        }

        // Otherwise (first spawned drone), position next to neighbors
        if connections.is_empty() {
            let fallback = (self.next_position_x, self.next_position_y);
            self.last_spawned_position = Some(fallback);
            return fallback;
        }

        let mut neighbor_positions = Vec::new();
        for &conn_id in connections {
            if let Some(&idx) = self.node_id_to_index.get(&conn_id) {
                neighbor_positions.push(self.nodes[idx].position);
            }
        }

        if neighbor_positions.is_empty() {
            let fallback = (self.next_position_x, self.next_position_y);
            self.last_spawned_position = Some(fallback);
            return fallback;
        }

        let avg_x = neighbor_positions.iter().map(|p| p.0).sum::<f32>() / neighbor_positions.len() as f32;
        let avg_y = neighbor_positions.iter().map(|p| p.1).sum::<f32>() / neighbor_positions.len() as f32;

        self.last_spawned_position = Some((avg_x, avg_y));
        (avg_x, avg_y)
    }

    fn build_topology_layout(&mut self, topo: &str, config: &ParsedConfig, previous_states: &HashMap<usize, bool>, previous_nodes: &Vec<Node>){
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;
        match topo {
            "Star" => {
                let center_x = WINDOW_WIDTH / 2.0;
                let center_y = WINDOW_HEIGHT / 2.0;
                let radius = 120.0;

                for (i, drone) in config.drone.iter().enumerate() {
                    let angle = i as f32 * (std::f32::consts::TAU / 10.0);
                    let x = center_x + radius * angle.cos();
                    let y = center_y + radius * angle.sin();
                    let active = previous_states.get(&(drone.id as usize)).copied().unwrap_or(true);

                    // 🛠️ Check if this drone ID was manually positioned before
                    if let Some(&(mx, my)) = self.manual_positions.get(&drone.id) {
                        self.nodes.push(Node {
                            id: drone.id as usize,
                            node_type: NodeType::Drone,
                            pdr: drone.pdr,
                            active,
                            position: (mx, my),  // ✅ manually saved position
                            manual_position: true,
                        });
                    } else {
                        self.nodes.push(Node {
                            id: drone.id as usize,
                            node_type: NodeType::Drone,
                            pdr: drone.pdr,
                            active,
                            position: (x, y),  // normal computed layout
                            manual_position: false,
                        });
                    }


                    self.node_id_to_index.insert(drone.id, self.nodes.len() - 1);
                }
            }

            "Double Chain" => {
                let top_y = WINDOW_HEIGHT / 2.0 - 60.0;
                let bottom_y = WINDOW_HEIGHT / 2.0 + 60.0;
                let spacing = (WINDOW_WIDTH - 100.0) / 4.0;

                for (i, drone) in config.drone.iter().enumerate() {
                    let x = 50.0 + (i % 5) as f32 * spacing;
                    let y = if i < 5 { top_y } else { bottom_y };
                    let active = previous_states.get(&(drone.id as usize)).copied().unwrap_or(true);

                    // 🛠️ Check if this drone ID was manually positioned before
                    if let Some(&(mx, my)) = self.manual_positions.get(&drone.id) {
                        self.nodes.push(Node {
                            id: drone.id as usize,
                            node_type: NodeType::Drone,
                            pdr: drone.pdr,
                            active,
                            position: (mx, my),  // ✅ manually saved position
                            manual_position: true,
                        });
                    } else {
                        self.nodes.push(Node {
                            id: drone.id as usize,
                            node_type: NodeType::Drone,
                            pdr: drone.pdr,
                            active,
                            position: (x, y),  // normal computed layout
                            manual_position: false,
                        });
                    }


                    self.node_id_to_index.insert(drone.id, self.nodes.len() - 1);
                }
            }

            "Butterfly" => {
                let x1 = 100.0;
                let x2 = 200.0;
                let x3 = 300.0;
                let x4 = 400.0;

                let top_y = 100.0;
                let mid_y = 200.0;
                let bottom_y = 300.0;

                let positions = vec![
                    (x1, top_y),    // 1
                    (x2, top_y),    // 2
                    (x3, top_y),    // 3
                    (x4, top_y),    // 4
                    (x1, mid_y), // 5
                    (x2, mid_y), // 6
                    (x3, mid_y), // 7
                    (x4, mid_y), // 8
                    (x2, bottom_y),    // 9
                    (x3, bottom_y),    // 10
                ];

                for (i, drone) in config.drone.iter().enumerate() {
                    if i >= positions.len() {
                        break;
                    }
                    let (x, y) = positions[i];
                    let active = previous_states.get(&(drone.id as usize)).copied().unwrap_or(true);

                    // 🛠️ Check if this drone ID was manually positioned before
                    if let Some(&(mx, my)) = self.manual_positions.get(&drone.id) {
                        self.nodes.push(Node {
                            id: drone.id as usize,
                            node_type: NodeType::Drone,
                            pdr: drone.pdr,
                            active,
                            position: (mx, my),  // ✅ manually saved position
                            manual_position: true,
                        });
                    } else {
                        self.nodes.push(Node {
                            id: drone.id as usize,
                            node_type: NodeType::Drone,
                            pdr: drone.pdr,
                            active,
                            position: (x, y),  // normal computed layout
                            manual_position: false,
                        });
                    }


                    self.node_id_to_index.insert(drone.id, self.nodes.len() - 1);
                }
            }
            "Tree" => {
                let spacing_x = WINDOW_WIDTH / 5.0;
                let spacing_y = WINDOW_HEIGHT / 5.0;
                let mut y = spacing_y;
                let levels = [1, 2, 3, 4];
                let mut drone_index = 0;

                for &n_nodes in &levels {
                    let x_start = (WINDOW_WIDTH - (n_nodes as f32 - 1.0) * spacing_x) / 2.0;
                    for j in 0..n_nodes {
                        if drone_index >= config.drone.len() {
                            break;
                        }

                        let drone = &config.drone[drone_index];
                        let x = x_start + j as f32 * spacing_x;
                        let active = previous_states.get(&(drone.id as usize)).copied().unwrap_or(true);

                        if let Some(&(mx, my)) = self.manual_positions.get(&drone.id) {
                            self.nodes.push(Node {
                                id: drone.id as usize,
                                node_type: NodeType::Drone,
                                pdr: drone.pdr,
                                active,
                                position: (mx, my),  // ✅ manually saved position
                                manual_position: true,
                            });
                        } else {
                            self.nodes.push(Node {
                                id: drone.id as usize,
                                node_type: NodeType::Drone,
                                pdr: drone.pdr,
                                active,
                                position: (x, y),  // normal computed layout
                                manual_position: false,
                            });
                        }


                        self.node_id_to_index.insert(drone.id, self.nodes.len() - 1);
                        drone_index += 1;
                    }
                    y += spacing_y;
                }
            }

            "Sub-Net" => {
                let top_y = 100.0;
                let bottom_y = 200.0;

                // Predefine exact X coordinates
                let x_coords = vec![
                    60.0,   // 1
                    120.0,  // 2
                    180.0,  // 3
                    240.0,  // 4
                    300.0,  // 5
                    360.0,  // 6
                    90.0,   // 7 (center between 1 and 2)
                    180.0,  // 8 (under 3)
                    240.0,  // 9 (under 4)
                    330.0   // 10 (center between 5 and 6)
                ];

                for (i, drone) in config.drone.iter().enumerate() {
                    if i >= x_coords.len() {
                        break;
                    }

                    let x = x_coords[i];
                    let y = if i < 6 { top_y } else { bottom_y };
                    let active = previous_states.get(&(drone.id as usize)).copied().unwrap_or(true);

                    // 🛠️ Check if this drone ID was manually positioned before
                    if let Some(&(mx, my)) = self.manual_positions.get(&drone.id) {
                        self.nodes.push(Node {
                            id: drone.id as usize,
                            node_type: NodeType::Drone,
                            pdr: drone.pdr,
                            active,
                            position: (mx, my),  // ✅ manually saved position
                            manual_position: true,
                        });
                    } else {
                        self.nodes.push(Node {
                            id: drone.id as usize,
                            node_type: NodeType::Drone,
                            pdr: drone.pdr,
                            active,
                            position: (x, y),  // normal computed layout
                            manual_position: false,
                        });
                    }



                    self.node_id_to_index.insert(drone.id, self.nodes.len() - 1);
                }


            }
            _ => {
                println!("⚠️ Unknown topology in fallback, using grid.");
                self.build_grid(config, previous_states);
            }
        }

        // Step 1: Determine bounding box of all drones
        let drone_positions: Vec<(f32, f32)> = self.nodes
            .iter()
            .filter(|n| matches!(n.node_type, NodeType::Drone))
            .map(|n| n.position)
            .collect();

        if drone_positions.is_empty() {
            println!("No drones found, skipping edge placement.");
            return;
        }

        let min_x = drone_positions.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let max_x = drone_positions.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
        let min_y = drone_positions.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let max_y = drone_positions.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);

        let client_y = max_y + 80.0; // Below
        let server_y = min_y - 80.0; // Above
        let spacing = 100.0;

        // Step 2: Place clients along bottom, spaced horizontally
        for (i, client) in config.client.iter().enumerate() {
            let cx = min_x + spacing * i as f32;
            self.nodes.push(Node::client(client.id as usize, cx, client_y));
            self.node_id_to_index.insert(client.id, self.nodes.len() - 1);
        }

        // Step 3: Place servers along top, spaced horizontally
        for (i, server) in config.server.iter().enumerate() {
            let sx = min_x + spacing * i as f32;
            self.nodes.push(Node::server(server.id as usize, sx, server_y));
            self.node_id_to_index.insert(server.id, self.nodes.len() - 1);
        }


        // Fallback to grid if nothing placed
        if self.nodes.is_empty() {
            self.build_grid(config, previous_states);
        }

        // Add edges
        for drone in &config.drone {
            if let Some(&source_index) = self.node_id_to_index.get(&drone.id) {
                for &target_id in &drone.connected_node_ids {
                    if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                        self.edges.push((source_index, target_index));
                    }
                }
            }
        }

        for client in &config.client {
            if let Some(&source_index) = self.node_id_to_index.get(&client.id) {
                for &target_id in &client.connected_drone_ids {
                    if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                        self.edges.push((source_index, target_index));
                    }
                }
            }
        }

        for server in &config.server {
            if let Some(&source_index) = self.node_id_to_index.get(&server.id) {
                for &target_id in &server.connected_drone_ids {
                    if let Some(&target_index) = self.node_id_to_index.get(&target_id) {
                        self.edges.push((source_index, target_index));
                    }
                }
            }
        }

        self.next_position_x = 50.0;
        self.next_position_y = WINDOW_HEIGHT - 50.0;
    }


    // Add a new node to the network
    // "I don't know neighbors, just drop node somewhere."
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

    fn build_grid(&mut self, config: &ParsedConfig, previous_states: &HashMap<usize, bool>) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let total_nodes = config.drone.len() + config.client.len() + config.server.len();
        let grid_size = (total_nodes as f32).sqrt().ceil() as usize;
        let cell_width = WINDOW_WIDTH / (grid_size as f32 + 1.0);
        let cell_height = WINDOW_HEIGHT / (grid_size as f32 + 1.0);

        let mut node_index = 0;
        for drone in &config.drone {
            let x = (node_index % grid_size) as f32 * cell_width + cell_width;
            let y = (node_index / grid_size) as f32 * cell_height + cell_height;
            let active = previous_states.get(&(drone.id as usize)).copied().unwrap_or(true);

            self.nodes.push(Node {
                id: drone.id as usize,
                node_type: NodeType::Drone,
                pdr: drone.pdr,
                active,
                position: (x, y),
                manual_position:false,
            });
            self.node_id_to_index.insert(drone.id, self.nodes.len() - 1);
            node_index += 1;
        }
        for client in &config.client {
            let x = (node_index % grid_size) as f32 * cell_width + cell_width;
            let y = (node_index / grid_size) as f32 * cell_height + cell_height;
            self.nodes.push(Node::client(client.id as usize, x, y));
            self.node_id_to_index.insert(client.id, self.nodes.len() - 1);
            node_index += 1;
        }
        for server in &config.server {
            let x = (node_index % grid_size) as f32 * cell_width + cell_width;
            let y = (node_index / grid_size) as f32 * cell_height + cell_height;
            self.nodes.push(Node::server(server.id as usize, x, y));
            self.node_id_to_index.insert(server.id, self.nodes.len() - 1);
            node_index += 1;
        }
    }






    // Add a connection between two nodes
    fn add_connection(&mut self, source_id: usize, target_id: usize) -> bool {
        if let (Some(&source_idx), Some(&target_idx)) = (
            self.node_id_to_index.get(&(source_id as NodeId)),
            self.node_id_to_index.get(&(target_id as NodeId)),
        ) {
            if self.edges.contains(&(source_idx, target_idx)) || self.edges.contains(&(target_idx, source_idx)) {
                return false;
            }

            if let Some(cfg_arc) = &self.config {
                let mut cfg = cfg_arc.lock().unwrap();

                let a_type = self.nodes[source_idx].node_type;
                let b_type = self.nodes[target_idx].node_type;

                // 🔒 Constraint checks
                let constraint_ok = match (a_type, b_type) {
                    (NodeType::Client, NodeType::Drone) => {
                        let c = cfg.client.iter().find(|c| c.id == source_id as NodeId);
                        if let Some(c) = c {
                            if c.connected_drone_ids.len() >= 2 {
                                eprintln!("Client {} already has 2 drone connections!", source_id);
                                return false;
                            }
                        }
                        true
                    }

                    (NodeType::Drone, NodeType::Client) => {
                        let c = cfg.client.iter().find(|c| c.id == target_id as NodeId);
                        if let Some(c) = c {
                            if c.connected_drone_ids.len() >= 2 {
                                eprintln!("Client {} already has 2 drone connections!", target_id);
                                return false;
                            }
                        }
                        true
                    }

                    _ => true,
                };

                if !constraint_ok {
                    return false; // 🚫 Constraint violated — exit early
                }

                // ✅ Passed checks — apply connection
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
                        cfg.append_drone_connection(source_id as NodeId, target_id as NodeId);
                        cfg.append_client_connection(target_id as NodeId, source_id as NodeId);
                    }
                    (NodeType::Client, NodeType::Drone) => {
                        cfg.append_drone_connection(target_id as NodeId, source_id as NodeId);
                        cfg.append_client_connection(source_id as NodeId, target_id as NodeId);
                    }
                    _ => {
                        eprintln!("Unsupported connection between {:?} and {:?}", a_type, b_type);
                    }
                }

                // ✅ Only now: add edge visually
                self.edges.push((source_idx, target_idx));
            }
            if let Some(ctrl_arc) = &self.simulation_controller {
                let mut ctrl = ctrl_arc.lock().unwrap();
                ctrl.add_connection(source_id as NodeId, target_id as NodeId);
            }

            return true;
        }

        false
    }


    fn setup_star(&mut self, _x_offset: f32, _y_offset: f32) {
        println!("im in setup");
        const WINDOW_WIDTH: f32 = 800.0;
        const WINDOW_HEIGHT: f32 = 600.0;

        let num_drones = 10;
        let center_x = WINDOW_WIDTH / 2.0;
        let center_y = WINDOW_HEIGHT / 2.0;
        let radius = 200.0;

        // Position drones in a circle (IDs: 1 to 10)
        for i in 0..num_drones {
            let angle = i as f32 * (std::f32::consts::TAU / num_drones as f32);
            let x = center_x + radius * angle.cos();
            let y = center_y + radius * angle.sin();
            self.nodes.push(Node::drone(i + 1, x, y, 1.0)); // PDR will be synced from config
        }

        // Connect each drone to its 3rd follower (decagram)
        for i in 0..num_drones {
            let from_id = i + 1;
            let to_id = ((i + 3) % num_drones) + 1;
            self.edges.push((from_id, to_id));
        }

        // Add client 100 near drone 1
        self.nodes.push(Node::client(100, center_x - radius - 50.0, center_y));
        self.edges.push((100, 1));

        // Add client 101 near drone 5
        let angle_5 = 4.0 * std::f32::consts::TAU / 10.0;
        self.nodes.push(Node::client(101,
                                     center_x + (radius + 50.0) * angle_5.cos(),
                                     center_y + (radius + 50.0) * angle_5.sin()));
        self.edges.push((101, 5));

        // Add server 102 near drones 3 and 7
        let pos3 = &self.nodes[2].position;
        let pos7 = &self.nodes[6].position;
        let mid_x = (pos3.0 + pos7.0) / 2.0;
        let mid_y = (pos3.1 + pos7.1) / 2.0;
        self.nodes.push(Node::server(102, mid_x, mid_y - 60.0));
        self.edges.push((102, 3));
        self.edges.push((102, 7));
    }


    fn setup_double_chain(&mut self, _x_offset: f32, _y_offset: f32) {
        const WINDOW_WIDTH: f32 = 1200.0;
        const WINDOW_HEIGHT: f32 = 900.0;

        let drones_per_row = 5;
        let h_spacing = (WINDOW_WIDTH - 2.0 * 100.0) / (drones_per_row as f32 - 1.0);
        let top_y = WINDOW_HEIGHT / 2.0 - 80.0;
        let bottom_y = WINDOW_HEIGHT / 2.0 + 80.0;

        // Add drones 1-5 (top row)
        for i in 0..drones_per_row {
            let x = 100.0 + i as f32 * h_spacing;
            let id = i + 1;
            self.nodes.push(Node::drone(id, x, top_y, 1.0));
        }

        // Add drones 6-10 (bottom row)
        for i in 0..drones_per_row {
            let x = 100.0 + i as f32 * h_spacing;
            let id = i + 6;
            self.nodes.push(Node::drone(id, x, bottom_y, 1.0));
        }

        // Horizontal edges top: (1-2), (2-3), ..., (4-5)
        for i in 1..drones_per_row {
            self.edges.push((i, i + 1));
        }

        // Horizontal edges bottom: (6-7), (7-8), ..., (9-10)
        for i in 6..(6 + drones_per_row - 1) {
            self.edges.push((i, i + 1));
        }

        // Vertical edges: (1-6), (2-7), ..., (5-10)
        for i in 0..drones_per_row {
            self.edges.push((i + 1, i + 6));
        }

        // Server above drone 1
        let server_id = 102;
        self.nodes.push(Node::server(server_id, 100.0, top_y - 80.0));
        self.edges.push((server_id, 1));

        // Clients below drones 6 and 10
        self.nodes.push(Node::client(100, 100.0, bottom_y + 80.0)); // near drone 6
        self.nodes.push(Node::client(101, 100.0 + 4.0 * h_spacing, bottom_y + 80.0)); // near drone 10
        self.edges.push((100, 6));
        self.edges.push((101, 10));
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

    fn setup_tree(&mut self, _x_offset: f32, _y_offset: f32) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let mut id = 0;

        // L1 (1 node)
        let x_l1 = WINDOW_WIDTH / 2.0;
        let y_l1 = 50.0;
        self.nodes.push(Node::drone(id, x_l1, y_l1, 1.0));
        let root_id = id;
        id += 1;

        // L2 (2 nodes)
        let y_l2 = y_l1 + 60.0;
        let l2_ids: Vec<usize> = (0..2).map(|i| {
            let x = 200.0 + i as f32 * 200.0;
            self.nodes.push(Node::drone(id, x, y_l2, 1.0));
            let l2_id = id;
            self.edges.push((root_id, id));
            id += 1;
            l2_id
        }).collect();

        // L3 (3 nodes)
        let y_l3 = y_l2 + 60.0;
        let l3_ids: Vec<usize> = (0..3).map(|i| {
            let x = 150.0 + i as f32 * 150.0;
            self.nodes.push(Node::drone(id, x, y_l3, 1.0));
            let l3_id = id;
            for &l2 in &l2_ids {
                self.edges.push((l2, l3_id));
            }
            id += 1;
            l3_id
        }).collect();

        // L4 (4 nodes)
        let y_l4 = y_l3 + 60.0;
        let l4_ids: Vec<usize> = (0..4).map(|i| {
            let x = 100.0 + i as f32 * 125.0;
            self.nodes.push(Node::drone(id, x, y_l4, 1.0));
            let l4_id = id;
            for &l3 in &l3_ids {
                self.edges.push((l3, l4_id));
            }
            id += 1;
            l4_id
        }).collect();

        // Add server at top
        self.nodes.push(Node::server(id, x_l1, y_l1 - 50.0));
        self.edges.push((id, root_id));
        self.edges.push((id, l2_ids[0]));
        id += 1;

        // Add two clients connected to bottom leaves
        self.nodes.push(Node::client(id, 50.0, y_l4 + 50.0));
        self.edges.push((id, l4_ids[0]));
        id += 1;

        self.nodes.push(Node::client(id, WINDOW_WIDTH - 50.0, y_l4 + 50.0));
        self.edges.push((id, l4_ids[3]));
    }


    fn setup_subnet(&mut self, _x_offset: f32, _y_offset: f32) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let mut id = 0;

        // Group A: left 5 drones in a star shape
        let center_a = (150.0, WINDOW_HEIGHT / 2.0);
        for i in 0..5 {
            let angle = i as f32 * (std::f32::consts::TAU / 5.0);
            let x = center_a.0 + 50.0 * angle.cos();
            let y = center_a.1 + 50.0 * angle.sin();
            self.nodes.push(Node::drone(id, x, y, 1.0));
            id += 1;
        }

        // Connect Group A drones in a star-like pattern
        for i in 0..5 {
            self.edges.push((i, (i + 2) % 5));
        }

        // Group B: right 5 drones in another star
        let center_b = (450.0, WINDOW_HEIGHT / 2.0);
        for i in 0..5 {
            let angle = i as f32 * (std::f32::consts::TAU / 5.0);
            let x = center_b.0 + 50.0 * angle.cos();
            let y = center_b.1 + 50.0 * angle.sin();
            self.nodes.push(Node::drone(id, x, y, 1.0));
            id += 1;
        }

        // Connect Group B drones in a star-like pattern
        for i in 5..10 {
            self.edges.push((i, 5 + (i + 2) % 5));
        }

        // Interconnect 2 drones from each group
        self.edges.push((0, 5)); // bridge
        self.edges.push((2, 7)); // bridge

        // Add a server and two clients
        self.nodes.push(Node::server(id, WINDOW_WIDTH / 2.0, 50.0));
        self.edges.push((id, 0));
        self.edges.push((id, 5));
        id += 1;

        self.nodes.push(Node::client(id, 50.0, WINDOW_HEIGHT - 30.0));
        self.edges.push((id, 3));
        id += 1;

        self.nodes.push(Node::client(id, WINDOW_WIDTH - 50.0, WINDOW_HEIGHT - 30.0));
        self.edges.push((id, 8));
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

    pub fn render(&mut self, ui: &mut egui::Ui,offset: Vec2) {

        // Draw edges first
        {
            let painter = ui.painter();
            for &(a, b) in &self.edges {
                if a < self.nodes.len() && b < self.nodes.len() {
                    let node_a = &self.nodes[a];
                    let node_b = &self.nodes[b];

                    let pos_a = Pos2::new(
                        node_a.position.0 * self.scale + offset.x,
                        node_a.position.1 * self.scale + offset.y,
                    );

                    let pos_b = Pos2::new(
                        node_b.position.0 * self.scale + offset.x,
                        node_b.position.1 * self.scale + offset.y,
                    );

                    // Edge color based on node type (optional)
                    let color = if node_a.node_type == NodeType::Server || node_b.node_type == NodeType::Server {
                        Color32::BLUE
                    } else if node_a.node_type == NodeType::Client || node_b.node_type == NodeType::Client {
                        Color32::YELLOW
                    } else {
                        Color32::GRAY
                    };

                    if node_a.node_type == NodeType::Client || node_b.node_type == NodeType::Client {
                        let control = Pos2::new((pos_a.x + pos_b.x) / 2.0, (pos_a.y + pos_b.y) / 2.0 - 40.0);

                        let mut points = Vec::new();
                        let steps = 20;
                        for i in 0..=steps {
                            let t = i as f32 / steps as f32;
                            let inv_t = 1.0 - t;

                            let x = inv_t * inv_t * pos_a.x + 2.0 * inv_t * t * control.x + t * t * pos_b.x;
                            let y = inv_t * inv_t * pos_a.y + 2.0 * inv_t * t * control.y + t * t * pos_b.y;

                            points.push(Pos2::new(x, y));
                        }

                        let curve = egui::epaint::PathShape::line(points, egui::Stroke::new(2.0, color));
                        painter.add(curve);
                    } else {
                        painter.line_segment([pos_a, pos_b], (2.0, color));
                    }



                }
            }


        }

        // Handle node interaction
        for (idx, node) in self.nodes.iter().enumerate() {
            let pos = Pos2::new(
                node.position.0 * self.scale + offset.x,
                node.position.1 * self.scale + offset.y,
            );

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
                if let Some(texture) = match node.node_type {
                    NodeType::Drone => {
                        if node.active {
                            &self.drone_texture
                        } else {
                            &self.fire
                        }
                    },
                    NodeType::Client => &self.client_texture,
                    NodeType::Server => &self.server_texture,
                } {
                    let size = egui::vec2(32.0, 32.0); // Size for the image
                    let rect = egui::Rect::from_center_size(pos, size);
                    ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)), Color32::WHITE);

                    // node id next to image
                    painter.text(
                        Pos2::new(pos.x + 15.0, pos.y - 10.0),
                        egui::Align2::LEFT_CENTER,
                        format!("{}", node.id),
                        egui::FontId::proportional(14.0),
                        Color32::BLACK,
                    );

                }
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

                                // New: Remove Sender dropdown and button
                                let mut selected_neighbor: Option<NodeId> = None;

                                egui::ComboBox::from_label("Remove sender (neighbor):")
                                    .selected_text("select neighbor")
                                    .show_ui(ui, |ui| {
                                        if let Some(cfg) = &self.config {
                                            let cfg = cfg.lock().unwrap();
                                            let neighbors = cfg.drone.iter()
                                                .find(|d| d.id == node_id)
                                                .map(|d| d.connected_node_ids.clone())
                                                .unwrap_or_default();

                                            for &neighbor in &neighbors {
                                                if ui.selectable_label(false, format!("Node {}", neighbor)).clicked() {
                                                    selected_neighbor = Some(neighbor);
                                                    ui.close_menu();
                                                }
                                            }
                                        }
                                    });

                                if let Some(neighbor_id) = selected_neighbor {
                                    if let Some(ctrl_arc) = &self.simulation_controller {
                                        let mut ctrl = ctrl_arc.lock().unwrap();

                                        // ✅ Run removal constraint check
                                        if ctrl.is_removal_allowed(node_id, neighbor_id) {
                                            if let Some(sender) = ctrl.command_senders.get(&node_id) {
                                                sender
                                                    .send(DroneCommand::RemoveSender(neighbor_id))
                                                    .expect("Failed to send RemoveSender");
                                                println!("RemoveSender sent from {} to {}", node_id, neighbor_id);
                                            }

                                            // 🧠 Update config connections
                                            if let Some(cfg_arc) = &self.config {
                                                let mut cfg = cfg_arc.lock().unwrap();
                                                if let Some(drone) = cfg.drone.iter_mut().find(|d| d.id == node_id) {
                                                    drone.connected_node_ids.retain(|&id| id != neighbor_id);
                                                }
                                                if let Some(drone) = cfg.drone.iter_mut().find(|d| d.id == neighbor_id) {
                                                    drone.connected_node_ids.retain(|&id| id != node_id);
                                                }

                                                drop(ctrl); // Drop before self mutation
                                                drop(cfg);
                                                //broadcast_topology_change(&self.chat_ui.gui_input, &self.network_config, "[FloodRequired]::SpawnDrone");

                                                self.build_from_config(cfg_arc.clone());
                                            }
                                        } else {
                                            eprintln!(
                                                "Refused to remove link {} <-> {}: violates constraints",
                                                node_id, neighbor_id
                                            );
                                        }
                                    }
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
                    // 1) mark the node inactive (so it won’t show controls or draw edges)
                    self.nodes[idx].active = false;

                    // 2) remove all UI‐edges to/from that node index
                    self.edges.retain(|&(u, v)| u != idx && v != idx);

                    // 3) update the config so no new edges reference it
                    if let Some(cfg_arc) = &self.config {
                        cfg_arc.lock().unwrap().remove_drone_connections(node_id);
                    }

                    // 4) (optional) reflow your servers/clients if you need to
                    self.reposition_hosts();
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


    //I know neighbors, place drone near its neighbor smartly."
    pub fn add_drone(&mut self, id: NodeId, pdr: f32, connections: Vec<NodeId>) {
        // ── 0) If we already rendered this drone, just update it and return ───────────────
        if let Some(&idx) = self.node_id_to_index.get(&id) {
            // 0a) Update its PDR
            self.nodes[idx].pdr = pdr;
            // 0b) Remove any old edges touching this node…
            self.edges.retain(|&(u, v)| u != idx && v != idx);
            // …then re-add all the new ones:
            for &peer in &connections {
                if let Some(&peer_idx) = self.node_id_to_index.get(&peer) {
                    self.edges.push((idx, peer_idx));
                }
            }
            // 0c) Sync just its PDR & links in the config
            if let Some(cfg_arc) = &self.config {
                let mut cfg = cfg_arc.lock().unwrap();
                cfg.set_drone_pdr(id, pdr);
                cfg.set_drone_connections(id, connections.clone());
                for &peer in &connections {
                    cfg.append_drone_connection(peer, id);
                }
            }
            return;
        }

        // ── 1) Pick a screen‐position ────────────────────────────────────────────────────
        let mut x = self.next_position_x;
        let mut y = self.next_position_y;

        // If we’ve manually placed it before, reuse that spot
        if let Some(&(mx, my)) = self.manual_positions.get(&id) {
            x = mx; y = my;
        }
        // Otherwise, if it has a neighbor, spawn it off to that quadrant
        else if let Some(&first) = connections.first() {
            if let Some(&n_idx) = self.node_id_to_index.get(&first) {
                let (nx, ny) = self.nodes[n_idx].position;
                let cx = (100.0 + 500.0) * 0.5;
                let cy = (100.0 + 400.0) * 0.5;
                let rnd = || rand_offset();

                if nx < cx && ny < cy {
                    x = 100.0 - 100.0 + rnd();
                    y = 100.0 - 100.0 + rnd();
                } else if nx >= cx && ny < cy {
                    x = 500.0 + 100.0 + rnd();
                    y = 100.0 - 100.0 + rnd();
                } else if nx < cx && ny >= cy {
                    x = 100.0 - 100.0 + rnd();
                    y = 400.0 + 100.0 + rnd();
                } else {
                    x = 500.0 + 100.0 + rnd();
                    y = 400.0 + 100.0 + rnd();
                }
            }
        }

        // Remember this manual position
        self.manual_positions.insert(id, (x, y));

        // ── 2) Push the new Node into our renderer’s lists ─────────────────────────────
        let new_idx = self.nodes.len();
        let mut node = Node::drone(id as usize, x, y, pdr);
        node.manual_position = true;
        self.nodes.push(node);
        self.node_id_to_index.insert(id, new_idx);

        // ── 3) Sync the canonical config ────────────────────────────────────────────────
        if let Some(cfg_arc) = &self.config {
            let mut cfg = cfg_arc.lock().unwrap();
            cfg.add_drone(id);                                  // add the drone
            cfg.set_drone_pdr(id, pdr);                         // record its PDR
            cfg.set_drone_connections(id, connections.clone()); // record its outgoing links
            // make the links bidirectional
            for &peer in &connections {
                cfg.append_drone_connection(peer, id);
            }
        }

        // ── 4) Finally, build the UI edges ─────────────────────────────────────────────
        for &peer in &connections {
            if let Some(&peer_idx) = self.node_id_to_index.get(&peer) {
                self.edges.push((new_idx, peer_idx));
            }
        }

        // ── 5) Advance the “next spawn” cursor ─────────────────────────────────────────
        self.next_position_x += 50.0;
        if self.next_position_x > 550.0 {
            self.next_position_x = 50.0;
            self.next_position_y += 50.0;
        }
        self.reposition_hosts();
    }
    fn reposition_hosts(&mut self) {
        // 1) Collect all drone positions
        let drones: Vec<&Node> = self
            .nodes
            .iter()
            .filter(|n| matches!(n.node_type, NodeType::Drone))
            .collect();
        if drones.is_empty() {
            return;
        }

        // 2) Compute drone bounding‐box
        let min_x = drones.iter().map(|n| n.position.0).fold(f32::INFINITY, f32::min);
        let max_x = drones.iter().map(|n| n.position.0).fold(f32::NEG_INFINITY, f32::max);
        let min_y = drones.iter().map(|n| n.position.1).fold(f32::INFINITY, f32::min);
        let max_y = drones.iter().map(|n| n.position.1).fold(f32::NEG_INFINITY, f32::max);

        // 3) Margin between drones and hosts
        let margin = 40.0;

        // 4) Reposition servers (above = min_y - margin)
        let servers: Vec<usize> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| if matches!(n.node_type, NodeType::Server) { Some(i) } else { None })
            .collect();
        let s_count = servers.len() as f32;
        if s_count > 0.0 {
            let h_step = (max_x - min_x) / (s_count + 1.0);
            let y_ser = min_y - margin;
            for (i, &idx) in servers.iter().enumerate() {
                let x = min_x + h_step * ((i + 1) as f32);
                self.nodes[idx].position.0 = x;
                self.nodes[idx].position.1 = y_ser;
                self.manual_positions.insert(
                    self.nodes[idx].id as NodeId,
                    (x, y_ser),
                );
            }
        }

        // 5) Reposition clients (below = max_y + margin)
        let clients: Vec<usize> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| if matches!(n.node_type, NodeType::Client) { Some(i) } else { None })
            .collect();
        let c_count = clients.len() as f32;
        if c_count > 0.0 {
            let h_step = (max_x - min_x) / (c_count + 1.0);
            let y_cli = max_y + margin;
            for (i, &idx) in clients.iter().enumerate() {
                let x = min_x + h_step * ((i + 1) as f32);
                self.nodes[idx].position.0 = x;
                self.nodes[idx].position.1 = y_cli;
                self.manual_positions.insert(
                    self.nodes[idx].id as NodeId,
                    (x, y_cli),
                );
            }
        }
    }



}

fn rand_offset() -> f32 {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    rng.gen_range(-30.0..30.0)
}

fn load_texture(ctx: &egui::Context, path: &str) -> egui::TextureHandle {
    let image = image::open(path).unwrap_or_else(|e| {
        panic!("❌ Failed to load image at '{}': {}", path, e);
    });    let size = [image.width() as usize, image.height() as usize];
    let image_buffer = image.to_rgba8();
    let pixels = image_buffer.as_flat_samples();
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_slice());
    ctx.load_texture(path, color_image, egui::TextureOptions::default())
}