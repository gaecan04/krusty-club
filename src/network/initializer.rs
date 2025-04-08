use toml;
use crossbeam_channel::{unbounded, select_biased, select, Receiver, Sender};
use serde::Deserialize;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::packet::Packet;
//use wg_2024::drone::Drone;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::thread;
use crossbeam::channel;

// Assuming these are defined elsewhere or imported
use wg_2024::network::NodeId;
#[cfg(feature = "serialize")]
use serde::Deserialize;

// These struct definitions were provided in your paste
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Drone {
    pub id: NodeId,
    pub connected_node_ids: Vec<NodeId>,
    pub pdr: f32,
}

#[derive(Debug, Clone,Deserialize)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Client {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,
}

#[derive(Debug, Clone,Deserialize)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Server {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Config {
    pub drone: Vec<Drone>,
    pub client: Vec<Client>,
    pub server: Vec<Server>,
}

// Type aliases for clarity
pub type DroneImpl = Box<dyn DroneImplementation>;
type NodeChannels = HashMap<NodeId, channel::Sender<Message>>;

// Message type for inter-node communication
#[derive(Debug, Clone)]
enum Message {
    // Define your message types here
    Data(Vec<u8>),
    Control(ControlMessage),
}

#[derive(Debug, Clone)]
enum ControlMessage {
    // Define control messages here
    Start,
    Stop,
    Status,
}

#[derive(Deserialize, Debug)]
pub struct ParsedConfig {
    pub drone: Vec<DroneConfig>, // Assume this contains information about each drone
    pub client: Vec<Client>,
    pub server: Vec<Server>,
}

#[derive(Deserialize,Debug)]
pub struct DroneConfig {
    pub id: NodeId,  // Example ID
    pub pdr: f32, // Example PDR
    pub connected_node_ids: Vec<NodeId>,
}


// Trait for drone implementations
trait DroneImplementation: Send + 'static {
    fn process_message(&mut self, msg: Message) -> Vec<Message>;
    fn get_id(&self) -> NodeId;
}

pub(crate) struct NetworkInitializer {
    config: Config,
    drone_impls: Vec<DroneImpl>,
    channels: NodeChannels,
    controller_tx: channel::Sender<Message>,
}

impl NetworkInitializer {
    pub fn new(config_path: &str, drone_impls: Vec<DroneImpl>) -> Result<Self, Box<dyn Error>> {
        // Read config file
        let config_str = fs::read_to_string(config_path)?;

        // Parse the TOML config
        #[cfg(feature = "serialize")]
        let config: Config = toml::from_str(&config_str)?;

        #[cfg(not(feature = "serialize"))]
        let config = panic!("The 'serialize' feature must be enabled to parse TOML");

        // Create controller channel (unbounded)
        let (controller_tx, _) = channel::unbounded();

        Ok(NetworkInitializer {
            config,
            drone_impls,
            channels: HashMap::new(),
            controller_tx,
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
        // Build adjacency list
        let mut adj_list: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        // Add all nodes
        for drone in &self.config.drone {
            adj_list.insert(drone.id, drone.connected_node_ids.clone());
        }

        for client in &self.config.client {
            adj_list.insert(client.id, client.connected_drone_ids.clone());
        }

        for server in &self.config.server {
            adj_list.insert(server.id, server.connected_drone_ids.clone());
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

        // Check if all nodes were visited
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
        // Create unbounded channels for all nodes
        for drone in &self.config.drone {
            let (tx, _) = channel::unbounded();
            self.channels.insert(drone.id, tx);
        }

        for client in &self.config.client {
            let (tx, _) = channel::unbounded();
            self.channels.insert(client.id, tx);
        }

        for server in &self.config.server {
            let (tx, _) = channel::unbounded();
            self.channels.insert(server.id, tx);
        }
    }

    fn initialize_drones(&mut self) {
        let num_drones = self.config.drone.len();
        let num_impls = self.drone_impls.len();

        // Distribute implementations evenly
        let mut impl_counts = vec![0; num_impls];
        let min_count = num_drones / num_impls;
        let remainder = num_drones % num_impls;

        // Each implementation should be used at least min_count times
        for i in 0..num_impls {
            impl_counts[i] = min_count;
            if i < remainder {
                impl_counts[i] += 1;
            }
        }

        // Assign implementations to drones and spawn threads
        let mut impl_index = 0;
        let mut count = 0;

        for drone in &self.config.drone {
            if count >= impl_counts[impl_index] {
                impl_index = (impl_index + 1) % num_impls;
                count = 0;
            }

            // Clone the channels that this drone needs
            let mut channels_for_drone = HashMap::new();
            for &connected_id in &drone.connected_node_ids {
                if let Some(tx) = self.channels.get(&connected_id) {
                    channels_for_drone.insert(connected_id, tx.clone());
                }
            }

            // Add controller channel
            let controller_tx = self.controller_tx.clone();

            // Get a drone implementation
            let drone_impl = &self.drone_impls[impl_index];
            let drone_id = drone.id;
            let drone_pdr = drone.pdr;

            // Spawn drone thread
            thread::spawn(move || {
                // Drone logic here
                // You would use drone_impl, channels_for_drone, controller_tx, etc.
                println!("Drone {} started with PDR {}", drone_id, drone_pdr);
            });

            count += 1;
        }
    }

    fn initialize_clients(&self) {
        for client in &self.config.client {
            // Clone the channels this client needs
            let mut channels_for_client = HashMap::new();
            for &drone_id in &client.connected_drone_ids {
                if let Some(tx) = self.channels.get(&drone_id) {
                    channels_for_client.insert(drone_id, tx.clone());
                }
            }

            // Add controller channel
            let controller_tx = self.controller_tx.clone();
            let client_id = client.id;

            // Spawn client thread
            thread::spawn(move || {
                // Client logic here
                println!("Client {} started", client_id);
            });
        }
    }

    fn initialize_servers(&self) {
        for server in &self.config.server {
            // Clone the channels this server needs
            let mut channels_for_server = HashMap::new();
            for &drone_id in &server.connected_drone_ids {
                if let Some(tx) = self.channels.get(&drone_id) {
                    channels_for_server.insert(drone_id, tx.clone());
                }
            }

            // Add controller channel
            let controller_tx = self.controller_tx.clone();
            let server_id = server.id;

            // Spawn server thread
            thread::spawn(move || {
                // Server logic here
                println!("Server {} started", server_id);
            });
        }
    }

    fn spawn_controller(&self) {
        // Clone necessary data for the controller
        let nodes = self.channels.keys().cloned().collect::<Vec<_>>();
        let controller_tx = self.controller_tx.clone();

        thread::spawn(move || {
            // Controller logic here
            println!("Controller started, managing {} nodes", nodes.len());
        });
    }
}


// Usage example
fn main() -> Result<(), Box<dyn Error>> {
    // Sample drone implementations
    let drone_impls: Vec<DroneImpl> = vec![
        // Your actual drone implementations would go here
    ];

    let mut initializer = NetworkInitializer::new("network_config.toml", drone_impls)?;
    initializer.initialize()?;

    println!("Network initialized successfully!");

    // Keep the main thread alive
    loop {
        thread::sleep(std::time::Duration::from_secs(1));
    }
}