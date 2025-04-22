use crate::TOML_parser::Client;
use crate::TOML_parser::Drone;
use crate::TOML_parser::Server;
use crate::TOML_parser::Config;

use crate::nodes::server;
use crate::nodes::client1;
use crate::nodes::client2;



use crate::Drone as OrigDrone;
use toml;
use crossbeam_channel::{unbounded, select_biased, select, Receiver, Sender};
use serde::Deserialize;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::packet::Packet;
//use wg_2024::drone::Drone;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use crossbeam::channel;

//for testing
use Krusty_Club::Krusty_C;
// Assuming these are defined elsewhere or imported
use wg_2024::network::NodeId;
use crate::simulation_controller::SC_backend::SimulationController;

#[cfg(feature = "serialize")]

// Type aliases for clarity
pub type DroneImpl = Box<dyn DroneImplementation>;
// This should be defined somewhere in your code
type GroupImplFactory = Box<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>,
    Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32)
    -> Box<dyn DroneImplementation> + Send + 'static>;

#[derive(Deserialize, Debug,Clone)]
pub struct ParsedConfig {
    pub drone: Vec<DroneConfig>, // Assume this contains information about each drone
    pub client: Vec<Client>,
    pub server: Vec<Server>,
}

#[derive(Deserialize,Debug,Clone)]
pub struct DroneConfig {
    pub id: NodeId,  // Example ID
    pub pdr: f32, // Example PDR
    pub connected_node_ids: Vec<NodeId>,
}

#[derive(Debug)]
pub struct MyDrone {
    pub id: NodeId,
    controller_send: Sender<DroneEvent>,
    controller_recv: Receiver<DroneCommand>,
    packet_recv: Receiver<Packet>,
    packet_send: HashMap<NodeId, Sender<Packet>>,
    pdr: f32,
}

impl ParsedConfig {
    pub fn add_drone(&mut self, id: NodeId) {
        // Create a new drone configuration with default values
        let new_drone = DroneConfig {
            id,
            pdr: 1.0,  // Default packet delivery rate
            connected_node_ids: Vec::new(),  // No connections initially
        };

        // Add the new drone to the configuration
        self.drone.push(new_drone);
    }

    // You might also want to add a method to set the connections for a drone
    pub fn set_drone_connections(&mut self, drone_id: NodeId, connections: Vec<NodeId>) {
        if let Some(drone) = self.drone.iter_mut().find(|d| d.id == drone_id) {
            drone.connected_node_ids = connections;
        }
    }

    // And a method to set the PDR for a drone
    pub fn set_drone_pdr(&mut self, drone_id: NodeId, pdr: f32) {
        if let Some(drone) = self.drone.iter_mut().find(|d| d.id == drone_id) {
            drone.pdr = pdr;
        }
    }

    pub fn remove_drone_connections(&mut self, drone_id: NodeId) {
        // First, remove this drone from all other drones' connection lists
        for drone in &mut self.drone {
            drone.connected_node_ids.retain(|&id| id != drone_id);
        }

        // Also remove this drone from all clients' connection lists
        for client in &mut self.client {
            client.connected_drone_ids.retain(|&id| id != drone_id);
        }

        // And remove this drone from all servers' connection lists
        for server in &mut self.server {
            server.connected_drone_ids.retain(|&id| id != drone_id);
        }
    }

    // Remove all connections to/from a specific client
    pub fn remove_client_connections(&mut self, client_id: NodeId) {
        // Remove this client from all drones' connection lists
        for drone in &mut self.drone {
            drone.connected_node_ids.retain(|&id| id != client_id);
        }

        // Also find the client and clear its connections
        if let Some(client) = self.client.iter_mut().find(|c| c.id == client_id) {
            client.connected_drone_ids.clear();
        }
    }

    // Remove all connections to/from a specific server
    pub fn remove_server_connections(&mut self, server_id: NodeId) {
        // Remove this server from all drones' connection lists
        for drone in &mut self.drone {
            drone.connected_node_ids.retain(|&id| id != server_id);
        }

        // Also find the server and clear its connections
        if let Some(server) = self.server.iter_mut().find(|s| s.id == server_id) {
            server.connected_drone_ids.clear();
        }
    }


        pub fn detect_topology(&self) -> Option<String> {
            let drone_count = self.drone.len();
            let mut drone_degrees = self.drone.iter().map(|d| d.connected_node_ids.len()).collect::<Vec<_>>();
            drone_degrees.sort();

            if drone_count == 10 && drone_degrees.iter().filter(|&&deg| deg == 2).count() == 10 {
                return Some("Double Chain".to_string());
            }

            if drone_count == 10 && drone_degrees.iter().filter(|&&deg| deg == 2 || deg == 3).count() >= 8 {
                return Some("Star".to_string());
            }

            if drone_count == 10 && drone_degrees.iter().all(|&d| d == 2 || d == 3) {
                return Some("Butterfly".to_string());
            }

            if drone_count == 10 && drone_degrees.contains(&6) && drone_degrees.contains(&2) {
                return Some("Tree".to_string());
            }

            if drone_count == 10 && drone_degrees.iter().any(|&d| d == 4 || d == 5) {
                return Some("Sub-Net".to_string());
            }

            None
        }
    pub fn append_drone_connection(&mut self, drone_id: NodeId, peer: NodeId) {
        if let Some(drone) = self.drone.iter_mut().find(|d| d.id == drone_id) {
            if !drone.connected_node_ids.contains(&peer) {
                drone.connected_node_ids.push(peer);
            }
        }
    }

    pub fn append_server_connection(&mut self, server_id: NodeId, drone: NodeId) {
        if let Some(server) = self.server.iter_mut().find(|s| s.id == server_id) {
            if !server.connected_drone_ids.contains(&drone) {
                server.connected_drone_ids.push(drone);
            }
        }
    }

    pub fn append_client_connection(&mut self, client_id: NodeId, drone: NodeId) {
        if let Some(client) = self.client.iter_mut().find(|c| c.id == client_id) {
            if !client.connected_drone_ids.contains(&drone) {
                client.connected_drone_ids.push(drone);
            }
        }
    }


}




impl DroneImplementation for MyDrone {
    fn process_packet(&mut self, packet: Packet) -> Vec<Packet> {
        // Process the packet (This is just a placeholder, modify as needed)
        println!("Processing packet for drone {}: {:?}", self.id, packet);
        vec![packet] // Returning the packet for now, adjust this to your needs
    }

    fn get_id(&self) -> NodeId {
        self.id
    }
}

impl DroneImplementation for Krusty_C {
    fn process_packet(&mut self, packet: Packet) -> Vec<Packet> {
        // Real implementation here
        println!("Krusty_C processing packet: {:?}", packet);
        vec![packet]
    }

    fn get_id(&self) -> NodeId {
        self.id // or wherever the ID is stored
    }
}

impl wg_2024::drone::Drone for MyDrone {
    fn new(
        id: NodeId,
        controller_send: Sender<DroneEvent>,
        controller_recv: Receiver<DroneCommand>,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
        pdr: f32,
    ) -> Self {
        MyDrone {
            id,
            controller_send,
            controller_recv,
            packet_recv,
            packet_send,
            pdr,
        }
    }

    fn run(&mut self) {
        println!("Running drone {} with PDR {}", self.id, self.pdr);

        // Real implementation would handle packets and controller commands
        loop {
            select! {
                recv(self.packet_recv) -> packet => {
                    if let Ok(packet) = packet {
                        println!("Drone {} received packet", self.id);
                        // Process packet logic here
                    }
                }
                recv(self.controller_recv) -> command => {
                    if let Ok(command) = command {
                        println!("Drone {} received command", self.id);
                        // Process command logic here
                    }
                }
            }
        }
    }
}

// Trait for drone implementations
pub trait DroneImplementation: Send + 'static {
    fn process_packet(&mut self, packet: Packet) -> Vec<Packet>;
    fn get_id(&self) -> NodeId;
}

pub struct NetworkInitializer {
    config: Config,
    drone_impls: Vec<Box<dyn DroneImplementation>>,
    channels: HashMap<NodeId, Sender<Packet>>,
    controller_tx: Sender<DroneEvent>,
    controller_rx: Receiver<DroneCommand>,
    simulation_controller: Arc<Mutex<SimulationController>>,

}

impl NetworkInitializer {
    pub fn new(config_path: &str, drone_impls: Vec<Box<dyn DroneImplementation>>,    simulation_controller: Arc<Mutex<SimulationController>>,
    ) -> Result<Self, Box<dyn Error>> {
        // Read config file
        let config_str = fs::read_to_string(config_path)?;

        // Parse the TOML config
        #[cfg(feature = "serialize")]
        let config: Config = toml::from_str(&config_str)?;

        #[cfg(not(feature = "serialize"))]
        let config = panic!("The 'serialize' feature must be enabled to parse TOML");

        // Create controller channels
        let (controller_tx, _) = channel::unbounded();
        let (_, controller_rx) = channel::unbounded();

        Ok(NetworkInitializer {
            config,
            drone_impls,
            channels: HashMap::new(),
            controller_tx,
            controller_rx,
            simulation_controller,
        })
    }

    pub fn initialize(&mut self) -> Result<(), Box<dyn Error>> {
        // Validate the network configuration
        self.validate_config()?;

        // Create channels for all nodes
        self.setup_channels();

        // Distribute drone implementations and spawn drone threads
        self.initialize_drones();

        // Spawn client threads
        self.initialize_clients();

        // Spawn server threads
        self.initialize_servers();

        // Spawn simulation controller thread
        self.spawn_controller();

        Ok(())
    }

    fn validate_config(&self) -> Result<(), Box<dyn Error>> {
        // Check for duplicate node IDs
        let mut all_ids = HashSet::new();

        for drone in &self.config.drone {
            if !all_ids.insert(drone.id) {
                return Err("Duplicate node ID found".into());
            }
        }

        for client in &self.config.client {
            if !all_ids.insert(client.id) {
                return Err("Duplicate node ID found".into());
            }

            // Check client constraints
            if client.connected_drone_ids.len() > 2 {
                return Err("Client cannot connect to more than 2 drones".into());
            }

            if client.connected_drone_ids.is_empty() {
                return Err("Client must connect to at least 1 drone".into());
            }

            // Check for repetitions in connected_drone_ids
            let mut client_connections = HashSet::new();
            for &drone_id in &client.connected_drone_ids {
                if !client_connections.insert(drone_id) {
                    return Err("Duplicate connection in client.connected_drone_ids".into());
                }
            }

            // Check that client is not connecting to itself
            if client.connected_drone_ids.contains(&client.id) {
                return Err("Client cannot connect to itself".into());
            }
        }

        for server in &self.config.server {
            if !all_ids.insert(server.id) {
                return Err("Duplicate node ID found".into());
            }

            // Check server constraints
            if server.connected_drone_ids.len() < 2 {
                return Err("Server must connect to at least 2 drones".into());
            }

            // Check for repetitions in connected_drone_ids
            let mut server_connections = HashSet::new();
            for &drone_id in &server.connected_drone_ids {
                if !server_connections.insert(drone_id) {
                    return Err("Duplicate connection in server.connected_drone_ids".into());
                }
            }

            // Check that server is not connecting to itself
            if server.connected_drone_ids.contains(&server.id) {
                return Err("Server cannot connect to itself".into());
            }
        }

        // Check bidirectional graph property
        self.check_bidirectional_connections()?;

        // Check connected graph property
        self.check_connected_graph()?;

        // Check that clients and servers are at the edges
        self.check_edges_property()?;

        Ok(())
    }

    fn check_bidirectional_connections(&self) -> Result<(), Box<dyn Error>> {
        // Create a map of all nodes and their connections
        let mut node_connections: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();

        // Add drone connections
        for drone in &self.config.drone {
            let entry = node_connections.entry(drone.id).or_insert_with(HashSet::new);
            for &connected_id in &drone.connected_node_ids {
                entry.insert(connected_id);
            }
        }

        // Add client connections
        for client in &self.config.client {
            let entry = node_connections.entry(client.id).or_insert_with(HashSet::new);
            for &drone_id in &client.connected_drone_ids {
                entry.insert(drone_id);
            }
            for &drone_id in &client.connected_drone_ids {
                // Check bidirectional connection
                if let Some(drone_connections) = node_connections.get(&drone_id) {
                    if !drone_connections.contains(&client.id) {
                        return Err(format!("Connection between client {} and drone {} is not bidirectional", client.id, drone_id).into());
                    }
                } else {
                    return Err(format!("Client {} connects to non-existent drone {}", client.id, drone_id).into());
                }
            }
        }

        // Add server connections
        for server in &self.config.server {
            let entry = node_connections.entry(server.id).or_insert_with(HashSet::new);
            for &drone_id in &server.connected_drone_ids {
                entry.insert(drone_id);
            }
            //2 loops to avoid the mut/immutable borrow simultaneous
            for &drone_id in &server.connected_drone_ids {
                // Check bidirectional connection
                if let Some(drone_connections) = node_connections.get(&drone_id) {
                    if !drone_connections.contains(&server.id) {
                        return Err(format!("Connection between server {} and drone {} is not bidirectional", server.id, drone_id).into());
                    }
                } else {
                    return Err(format!("Server {} connects to non-existent drone {}", server.id, drone_id).into());
                }
            }
        }

        Ok(())
    }

    fn check_connected_graph(&self) -> Result<(), Box<dyn Error>> {
        // Build undirected adjacency list
        let mut adj_list: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        // Add drone connections
        for drone in &self.config.drone {
            adj_list.entry(drone.id).or_default().extend(&drone.connected_node_ids);
            for &neighbor in &drone.connected_node_ids {
                adj_list.entry(neighbor).or_default().push(drone.id);
            }
        }

        // Add client connections
        for client in &self.config.client {
            adj_list.entry(client.id).or_default().extend(&client.connected_drone_ids);
            for &neighbor in &client.connected_drone_ids {
                adj_list.entry(neighbor).or_default().push(client.id);
            }
        }

        // Add server connections
        for server in &self.config.server {
            adj_list.entry(server.id).or_default().extend(&server.connected_drone_ids);
            for &neighbor in &server.connected_drone_ids {
                adj_list.entry(neighbor).or_default().push(server.id);
            }
        }

        // BFS to check connectivity
        if adj_list.is_empty() {
            return Ok(());
        }

        let start_node = *adj_list.keys().next().unwrap();
        let mut visited = HashSet::new();
        let mut queue = vec![start_node];

        while let Some(node) = queue.pop() {
            if visited.insert(node) {
                if let Some(neighbors) = adj_list.get(&node) {
                    for &neighbor in neighbors {
                        if !visited.contains(&neighbor) {
                            queue.push(neighbor);
                        }
                    }
                }
            }
        }

        if visited.len() != adj_list.len() {
            return Err("Graph is not connected".into());
        }

        Ok(())
    }

    fn check_edges_property(&self) -> Result<(), Box<dyn Error>> {
        // Build a graph without clients and servers
        let mut drone_adj_list: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        // Extract drone-to-drone connections
        for drone in &self.config.drone {
            let drone_connections: Vec<NodeId> = drone
                .connected_node_ids
                .iter()
                .filter(|&&id| {
                    // Check if id belongs to a drone (not a client or server)
                    self.config.drone.iter().any(|d| d.id == id)
                })
                .cloned()
                .collect();

            drone_adj_list.insert(drone.id, drone_connections);
        }

        // Check if the drone-only graph is connected using BFS
        if drone_adj_list.is_empty() {
            return Ok(());
        }

        let start_node = *drone_adj_list.keys().next().unwrap();
        let mut visited = HashSet::new();
        let mut queue = vec![start_node];

        while let Some(node) = queue.pop() {
            if visited.insert(node) {
                if let Some(neighbors) = drone_adj_list.get(&node) {
                    for &neighbor in neighbors {
                        if !visited.contains(&neighbor) {
                            queue.push(neighbor);
                        }
                    }
                }
            }
        }

        // Check if all drones were visited
        if visited.len() != drone_adj_list.len() {
            return Err("Drone-only graph is not connected (clients and servers must be at edges)".into());
        }

        Ok(())
    }

    fn setup_channels(&mut self) {
        let all_node_ids: Vec<NodeId> = self
            .config
            .drone
            .iter()
            .map(|d| d.id)
            .chain(self.config.client.iter().map(|c| c.id))
            .chain(self.config.server.iter().map(|s| s.id))
            .collect();

        for node_id in all_node_ids {
            // Create packet channel for each node
            let (tx, _rx) = unbounded::<Packet>();

            // Save to local runtime channels
            self.channels.insert(node_id, tx.clone());

            // Register with the Simulation Controller for ControllerShortcut
            if let Ok(mut controller) = self.simulation_controller.lock() {
                controller.register_packet_sender(node_id, tx);
            }
        }
    }

    fn initialize_drones(&mut self) {
        let num_drones = self.config.drone.len();
        let num_impls = self.drone_impls.len();
        println!("num drones {}", num_drones);
        println!("num impls {}", num_impls);

        // Distribute implementations evenly
        let mut impl_counts = vec![0; num_impls];
        let min_count = num_drones / num_impls;
        let remainder = num_drones % num_impls;

        for i in 0..num_impls {
            impl_counts[i] = min_count;
            if i < remainder {
                impl_counts[i] += 1;
            }
        }

        let mut impl_index = 0;
        let mut count = 0;

        for drone in &self.config.drone {
            if count >= impl_counts[impl_index] {
                impl_index = (impl_index + 1) % num_impls;
                count = 0;
            }

            let drone_id = drone.id;
            let drone_pdr = drone.pdr;

            // Create packet receive channel
            let (packet_tx, packet_rx) = channel::unbounded::<Packet>();
            self.channels.insert(drone_id, packet_tx);

            // Create command channel: Controller → Drone
            let (command_tx, command_rx) = channel::unbounded::<DroneCommand>();

            // Register the Sender in the Simulation Controller
            if let Ok(mut controller) = self.simulation_controller.lock() {
                controller.register_command_sender(drone_id, command_tx.clone());
            }

            // Build packet senders to neighbors
            let mut packet_send_channels = HashMap::new();
            for &connected_id in &drone.connected_node_ids {
                if let Some(tx) = self.channels.get(&connected_id) {
                    packet_send_channels.insert(connected_id, tx.clone());
                }
            }

            // Clone event channel (Drone → Controller)
            let controller_tx = self.controller_tx.clone();

            // Create the drone
            let mut drone_instance = MyDrone::new(
                drone_id,
                controller_tx,
                command_rx,
                packet_rx,
                packet_send_channels,
                drone_pdr,
            );

            // Spawn drone thread
            thread::spawn(move || {
                drone_instance.run();
            });

            count += 1;
        }
    }

    fn initialize_clients(&mut self) {
        for client in &self.config.client {
            let client_id = client.id;

            let mut senders = HashMap::new();
            for &drone_id in &client.connected_drone_ids {
                if let Some(tx) = self.channels.get(&drone_id) {
                    senders.insert(drone_id, tx.clone());
                }
            }

            let (client_tx, client_rx) = crossbeam_channel::unbounded();
            self.channels.insert(client_id, client_tx.clone());

            client1::start_client(client_id, client_rx, senders);
        }
    }


    fn initialize_servers(&mut self) {
        for server in &self.config.server {
            let server_id = server.id;

            // Gather packet senders to connected drones
            let mut senders = HashMap::new();
            for &drone_id in &server.connected_drone_ids {
                if let Some(tx) = self.channels.get(&drone_id) {
                    senders.insert(drone_id, tx.clone());
                }
            }

            // Create and store receiver
            let (server_tx, server_rx) = crossbeam_channel::unbounded();
            self.channels.insert(server_id, server_tx.clone());

            server::start_server(server_id, server_rx, senders);
        }
    }


    fn spawn_controller(&self) {
        // Get all node IDs for the controller to manage
        let nodes = self.channels.keys().cloned().collect::<Vec<_>>();

        // Create controller send/receive channels for commands and events
        let controller_tx = self.controller_tx.clone();
        let controller_rx = self.controller_rx.clone();

        thread::spawn(move || {
            println!("Controller started, managing {} nodes", nodes.len());

            // Controller main loop
            loop {
                // Process incoming drone events
                select! {
                    recv(controller_rx) -> event => {
                        if let Ok(event) = event {
                            println!("Controller received event: {:?}", event);
                            // Process the event
                        }
                    }
                    default => {
                        // No events received, can do periodic controller tasks here
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        });
    }

    pub fn create_drone_implementations(
        config: &ParsedConfig,
        controller_send: Sender<DroneEvent>,
        controller_recv: Receiver<DroneCommand>,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
    ) -> Vec<Box<dyn DroneImplementation>> {
        let mut implementations: Vec<Box<dyn DroneImplementation>> = Vec::new();

        // Load group implementations
        let group_implementations = Self::load_group_implementations();
        let num_impls = group_implementations.len();

        if num_impls == 0 {
            println!("Warning: No group implementations found. Using default implementation.");
            // Use default implementation for all drones
            for drone_config in config.drone.iter() {
                let id = drone_config.id;
                let pdr = drone_config.pdr;

                let drone_impl = Box::new(MyDrone::new(
                    id,
                    controller_send.clone(),
                    controller_recv.clone(),
                    packet_recv.clone(),
                    packet_send.clone(),
                    pdr,
                )) as Box<dyn DroneImplementation>;

                implementations.push(drone_impl);
            }

            return implementations;
        }

        // Calculate distribution of implementations
        let num_drones = config.drone.len();
        let mut impl_counts = vec![0; num_impls];
        let min_count = num_drones / num_impls;
        let remainder = num_drones % num_impls;

        for i in 0..num_impls {
            impl_counts[i] = min_count;
            if i < remainder {
                impl_counts[i] += 1;
            }
        }

        // Get the ordered list of implementations
        let group_keys: Vec<String> = group_implementations.keys().cloned().collect();

        // Distribute implementations to drones
        let mut impl_index = 0;
        let mut count = 0;

        for drone_config in &config.drone {
            if count >= impl_counts[impl_index] {
                impl_index = (impl_index + 1) % num_impls;
                count = 0;
            }

            let id = drone_config.id;
            let pdr = drone_config.pdr;

            // Get the implementation creator function
            let impl_key = &group_keys[impl_index];
            if let Some(create_impl) = group_implementations.get(impl_key) {
                // Create the group's implementation
                let drone_impl = create_impl(
                    id,
                    controller_send.clone(),
                    controller_recv.clone(),
                    packet_recv.clone(),
                    packet_send.clone(),
                    pdr,
                );

                implementations.push(drone_impl);
            } else {
                println!("ATTENTION :default drone impl");
                // Fallback to default implementation
                let drone_impl = Box::new(MyDrone::new(
                    id,
                    controller_send.clone(),
                    controller_recv.clone(),
                    packet_recv.clone(),
                    packet_send.clone(),
                    pdr,
                )) as Box<dyn DroneImplementation>;

                implementations.push(drone_impl);
            }

            count += 1;
        }

        implementations
    }

    // Method to load group implementations
    fn load_group_implementations() -> HashMap<String, GroupImplFactory> {
        let mut group_implementations = HashMap::new();

        // Group A implementation using Krusty_Club
        group_implementations.insert(
            "group_a1".to_string(),
            Box::new(|id: NodeId, sim_contr_send: Sender<DroneEvent>, sim_contr_recv: Receiver<DroneCommand>,
                      packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, pdr: f32|
                      -> Box<dyn DroneImplementation> {
                Box::new(Krusty_Club::Krusty_C::new(
                    id,
                    sim_contr_send,
                    sim_contr_recv,
                    packet_recv,
                    packet_send,
                    pdr
                ))
            }) as GroupImplFactory
        );

        // Same pattern for other implementations
        group_implementations.insert(
            "group_a2".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(Krusty_Club::Krusty_C::new(
                    id,
                    sim_contr_send,
                    sim_contr_recv,
                    packet_recv,
                    packet_send,
                    pdr
                ))
            }) as GroupImplFactory
        );

        group_implementations.insert(
            "group_a3".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(Krusty_Club::Krusty_C::new(
                    id,
                    sim_contr_send,
                    sim_contr_recv,
                    packet_recv,
                    packet_send,
                    pdr
                ))
            }) as GroupImplFactory
        );

        group_implementations.insert(
            "group_b".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(Krusty_Club::Krusty_C::new(
                    id,
                    sim_contr_send,
                    sim_contr_recv,
                    packet_recv,
                    packet_send,
                    pdr
                ))
            }) as GroupImplFactory
        );

        group_implementations
    }

    fn configure_drone_connections(&mut self) -> Result<(), Box<dyn Error>> {
        // Set up connections between drones
        for drone_config in &self.config.drone {
            let drone_id = drone_config.id;

            // Find the drone implementation
            if let Some(drone_impl) = self.drone_impls.iter_mut().find(|d| d.get_id() == drone_id) {
                // Configure connections to other drones
                for &connected_id in &drone_config.connected_node_ids {
                    println!("Drone {} connected to node {}", drone_id, connected_id);
                }
            }
        }

        // Set up connections to clients
        for client in &self.config.client {
            for &drone_id in &client.connected_drone_ids {
                if let Some(drone_impl) = self.drone_impls.iter_mut().find(|d| d.get_id() == drone_id) {
                    println!("Client {} connected to drone {}", client.id, drone_id);
                }
            }
        }

        // Set up connections to servers
        for server in &self.config.server {
            for &drone_id in &server.connected_drone_ids {
                if let Some(drone_impl) = self.drone_impls.iter_mut().find(|d| d.get_id() == drone_id) {
                    println!("Server {} connected to drone {}", server.id, drone_id);
                }
            }
        }

        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use crate::simulation_controller::SC_backend::SimulationController;

    fn mock_controller(config: ParsedConfig) -> Arc<Mutex<SimulationController>> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded::<DroneEvent>();
        Arc::new(Mutex::new(SimulationController::new(Arc::new(Mutex::new(config)), event_receiver)))
    }

    #[test]
    fn test_channel_setup() {
        // Create a mock config for testing
        let config = Config {
            drone: vec![
                Drone { id: 1, connected_node_ids: vec![2], pdr: 0.9 },
                Drone { id: 2, connected_node_ids: vec![1], pdr: 0.8 },
            ],
            client: vec![
                Client { id: 3, connected_drone_ids: vec![1] },
            ],
            server: vec![
                Server { id: 4, connected_drone_ids: vec![1] },
            ],
        };

        let parsed_config = ParsedConfig {
            drone: config.drone.iter().map(|d| DroneConfig {
                id: d.id,
                pdr: d.pdr,
                connected_node_ids: d.connected_node_ids.clone(),
            }).collect(),
            client: config.client.clone(),
            server: config.server.clone(),
        };

        let controller = mock_controller(parsed_config.clone());

        let drone_impls = vec![]; // no implementations for channel setup test

        let mut initializer = NetworkInitializer {
            config,
            drone_impls,
            channels: HashMap::new(),
            controller_tx: crossbeam_channel::unbounded().0,
            controller_rx: crossbeam_channel::unbounded().1,
            simulation_controller: controller,
        };

        initializer.setup_channels();

        for drone in &parsed_config.drone {
            assert!(initializer.channels.contains_key(&drone.id), "Missing channel for drone {}", drone.id);
        }
        for client in &parsed_config.client {
            assert!(initializer.channels.contains_key(&client.id), "Missing channel for client {}", client.id);
        }
        for server in &parsed_config.server {
            assert!(initializer.channels.contains_key(&server.id), "Missing channel for server {}", server.id);
        }
    }

    #[test]
    fn test_drone_initialization() {
        let drone_count = 3;
        let drones = vec![
            Drone { id: 1, connected_node_ids: vec![2], pdr: 0.9 },
            Drone { id: 2, connected_node_ids: vec![1], pdr: 0.8 },
            Drone { id: 3, connected_node_ids: vec![1], pdr: 0.7 },
        ];

        let parsed_config = ParsedConfig {
            drone: drones.iter().map(|d| DroneConfig {
                id: d.id,
                pdr: d.pdr,
                connected_node_ids: d.connected_node_ids.clone(),
            }).collect(),
            client: vec![],
            server: vec![],
        };

        let controller = mock_controller(parsed_config.clone());

        let mut initializer = NetworkInitializer {
            config: Config { drone: drones, client: vec![], server: vec![] },
            drone_impls: vec![Box::new(MyDrone::new(
                0,
                crossbeam_channel::unbounded().0,
                crossbeam_channel::unbounded().1,
                crossbeam_channel::unbounded().1,
                HashMap::new(),
                0.5,
            ))],
            channels: HashMap::new(),
            controller_tx: crossbeam_channel::unbounded().0,
            controller_rx: crossbeam_channel::unbounded().1,
            simulation_controller: controller,
        };

        initializer.setup_channels();
        initializer.initialize_drones(); // Calls your new logic

        // There's no deterministic way to test threads started here,
        // so we rely on "it didn't panic" and print logs
        assert_eq!(true, true);
    }

    #[test]
    fn test_full_network_initialization() {
        let drones = vec![
            Drone { id: 1, connected_node_ids: vec![2], pdr: 0.9 },
            Drone { id: 2, connected_node_ids: vec![1], pdr: 0.8 },
        ];
        let clients = vec![Client { id: 3, connected_drone_ids: vec![1] }];
        let servers = vec![Server { id: 4, connected_drone_ids: vec![2] }];

        let config = Config {
            drone: drones.clone(),
            client: clients.clone(),
            server: servers.clone(),
        };

        let parsed_config = ParsedConfig {
            drone: drones.iter().map(|d| DroneConfig {
                id: d.id,
                pdr: d.pdr,
                connected_node_ids: d.connected_node_ids.clone(),
            }).collect(),
            client: clients.clone(),
            server: servers.clone(),
        };

        let controller = mock_controller(parsed_config.clone());

        let mut initializer = NetworkInitializer {
            config,
            drone_impls: vec![Box::new(MyDrone::new(
                0,
                crossbeam_channel::unbounded().0,
                crossbeam_channel::unbounded().1,
                crossbeam_channel::unbounded().1,
                HashMap::new(),
                0.5,
            ))],
            channels: HashMap::new(),
            controller_tx: crossbeam_channel::unbounded().0,
            controller_rx: crossbeam_channel::unbounded().1,
            simulation_controller: controller,
        };

        initializer.setup_channels();
        initializer.initialize_drones();
        initializer.initialize_clients();
        initializer.initialize_servers();

        for node_id in [1, 2, 3, 4] {
            assert!(initializer.channels.contains_key(&node_id), "Missing channel for node {}", node_id);
        }
    }
}
