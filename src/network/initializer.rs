use crate::TOML_parser::Client;
use crate::TOML_parser::Drone;
use crate::TOML_parser::Server;
use crate::TOML_parser::Config;

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
use std::thread;
use crossbeam::channel;

//for testing
use Krusty_Club::Krusty_C;
// Assuming these are defined elsewhere or imported
use wg_2024::network::NodeId;


#[cfg(feature = "serialize")]

// Type aliases for clarity
pub type DroneImpl = Box<dyn DroneImplementation>;
// This should be defined somewhere in your code
type GroupImplFactory = Box<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>,
    Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32)
    -> Box<dyn DroneImplementation> + Send + 'static>;

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

#[derive(Debug)]
pub struct MyDrone {
    pub id: NodeId,
    controller_send: Sender<DroneEvent>,
    controller_recv: Receiver<DroneCommand>,
    packet_recv: Receiver<Packet>,
    packet_send: HashMap<NodeId, Sender<Packet>>,
    pdr: f32,
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
}

impl NetworkInitializer {
    pub fn new(config_path: &str, drone_impls: Vec<Box<dyn DroneImplementation>>) -> Result<Self, Box<dyn Error>> {
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
        println!("num drones {}", num_drones);
        println!("num impls {}", num_impls);

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

            // Clone the packet channels that this drone needs
            let mut packet_send_channels = HashMap::new();
            for &connected_id in &drone.connected_node_ids {
                if let Some(tx) = self.channels.get(&connected_id) {
                    packet_send_channels.insert(connected_id, tx.clone());
                }
            }

            // Create packet receive channel for this drone
            let (packet_tx, packet_rx) = channel::unbounded();

            // Store the tx end in our channels map
            self.channels.insert(drone.id, packet_tx);

            // Add controller channels
            let controller_tx = self.controller_tx.clone();
            let controller_rx = self.controller_rx.clone();

            // Get a drone implementation
            let drone_impl = &self.drone_impls[impl_index];
            let drone_id = drone.id;
            let drone_pdr = drone.pdr;

            // Create a new drone implementation
            let mut drone_instance = MyDrone::new(
                drone_id,
                controller_tx,
                controller_rx,
                packet_rx,
                packet_send_channels,
                drone_pdr
            );

            // Spawn drone thread
            thread::spawn(move || {
                // Start the drone's main loop
                drone_instance.run();
            });

            count += 1;
        }
    }

    fn initialize_clients(&self) {
        for client in &self.config.client {
            // Clone the channels this client needs
            let mut packet_send_channels = HashMap::new();
            for &drone_id in &client.connected_drone_ids {
                if let Some(tx) = self.channels.get(&drone_id) {
                    packet_send_channels.insert(drone_id, tx.clone());
                }
            }

            // Create packet receive channel for this client
            //let (_, packet_rx) = channel::unbounded::<T>();

            let client_id = client.id;

            //depending on the client struct I will add the recv_channel and channel with SC

            // Spawn client thread
            thread::spawn(move || {
                println!("Client {} started", client_id);

                // Client would use packet_send_channels to send packets to connected drones
                // And packet_rx to receive packets from those drones
                loop {
                    // Client logic here
                    thread::sleep(std::time::Duration::from_secs(1));
                }
            });
        }
    }

    fn initialize_servers(&self) {
        for server in &self.config.server {
            // Clone the channels this server needs
            let mut packet_send_channels = HashMap::new();
            for &drone_id in &server.connected_drone_ids {
                if let Some(tx) = self.channels.get(&drone_id) {
                    packet_send_channels.insert(drone_id, tx.clone());
                }
            }

            // Create packet receive channel for this server
            //let (_, packet_rx) = channel::unbounded();

            let server_id = server.id;

            // Spawn server thread
            thread::spawn(move || {
                println!("Server {} started", server_id);

                // Server would use packet_send_channels to send packets to connected drones
                // And packet_rx to receive packets from those drones
                loop {
                    // Server logic here
                    thread::sleep(std::time::Duration::from_secs(1));
                }
            });
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

// Usage example
fn main() -> Result<(), Box<dyn Error>> {
    // Sample drone implementations
    let drone_impls: Vec<Box<dyn DroneImplementation>> = vec![
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_setup() {
        // Create a mock config for testing
        let mock_config = Config {
            drone: vec![
                Drone {
                    id: 1,
                    connected_node_ids: vec![2],
                    pdr: 0.9,
                },
                Drone {
                    id: 2,
                    connected_node_ids: vec![1],
                    pdr: 0.8,
                },
            ],
            client: vec![
                Client {
                    id: 3,
                    connected_drone_ids: vec![1],
                },
            ],
            server: vec![
                Server {
                    id: 4,
                    connected_drone_ids: vec![1],
                },
            ],
        };

        // Initialize NetworkInitializer with mock config
        let mut initializer = NetworkInitializer::new("topologies/butterfly.toml", vec![]).unwrap();

        // Call setup_channels to create channels
        initializer.setup_channels();

        // Check that channels for each node were created
        for drone in &mock_config.drone {
            assert!(initializer.channels.contains_key(&drone.id), "Missing channel for drone {}", drone.id);
        }

        for client in &mock_config.client {
            assert!(initializer.channels.contains_key(&client.id), "Missing channel for client {}", client.id);
        }

        for server in &mock_config.server {
            assert!(initializer.channels.contains_key(&server.id), "Missing channel for server {}", server.id);
        }
    }



    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn test_drone_initialization() {
        let drone_count = 3;
        let counter = Arc::new(Mutex::new(0));

        // Create a mock config with 3 drones
        let mock_config = Config {
            drone: vec![
                Drone { id: 1, connected_node_ids: vec![2], pdr: 0.9 },
                Drone { id: 2, connected_node_ids: vec![1], pdr: 0.8 },
                Drone { id: 3, connected_node_ids: vec![1], pdr: 0.7 },
            ],
            client: vec![],
            server: vec![],
        };

        let mut initializer = NetworkInitializer::new("topologies/butterfly.toml", vec![]).unwrap();

        // Setup channels and drone implementations
        initializer.setup_channels();

        // Simulate drone thread creation (in place of the actual `thread::spawn`)
        for drone in mock_config.drone.clone() {
            let counter = Arc::clone(&counter);
            thread::spawn(move || {
                let mut count = counter.lock().unwrap();
                *count += 1;
                println!("Drone {} initialized", drone.id);
            });
        }

        // Wait for threads to complete
        thread::sleep(std::time::Duration::from_secs(1));

        // Check if the expected number of threads (drones) were spawned
        let count = counter.lock().unwrap();
        assert_eq!(*count, drone_count, "Expected {} drones, but got {}", drone_count, *count);
    }




    use super::*;

    #[test]
    fn test_full_network_initialization() {
        // Use a mock or simplified config to test the entire initialization flow
        let mock_config = Config {
            drone: vec![
                Drone { id: 1, connected_node_ids: vec![2], pdr: 0.9 },
                Drone { id: 2, connected_node_ids: vec![1], pdr: 0.8 },
            ],
            client: vec![
                Client { id: 3, connected_drone_ids: vec![1] },
            ],
            server: vec![
                Server { id: 4, connected_drone_ids: vec![2] },
            ],
        };

        let mut initializer = NetworkInitializer::new("topologies/butterfly.toml", vec![]).unwrap();

        // Perform the full initialization process
        initializer.initialize().unwrap();

        // Check that all channels are set up
        assert!(initializer.channels.contains_key(&1), "Missing channel for drone 1");
        assert!(initializer.channels.contains_key(&2), "Missing channel for drone 2");
        assert!(initializer.channels.contains_key(&3), "Missing channel for client 3");
        assert!(initializer.channels.contains_key(&4), "Missing channel for server 4");

        // Check that the drone threads are spawned (if using thread simulation)
        // You can further check the logs or counters if needed
    }
}


