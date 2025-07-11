use crate::TOML_parser::Client;
use crate::TOML_parser::Server;
use crate::TOML_parser::Config;

use crate::nodes::server;
use crate::nodes::client1;
use crate::nodes::client2;


use crate::Drone as OrigDrone;
use toml;
use crossbeam_channel::{unbounded, select, Receiver, Sender};
use serde::Deserialize;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::packet::Packet;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::Debug;
use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use crossbeam::channel;

use wg_2024::network::NodeId;
use crate::simulation_controller::SC_backend::SimulationController;
use crate::simulation_controller::gui_input_queue::SharedGuiInput;

use log::warn;


#[cfg(feature = "serialize")]
pub type DroneImpl = Box<dyn DroneImplementation>;
pub(crate) type GroupImplFactory = Box<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>,
    Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32)
    -> Box<dyn DroneImplementation> + Send + 'static >;

#[derive(Deserialize, Debug,Clone)]
pub struct ParsedConfig {
    pub drone: Vec<DroneConfig>,
    pub client: Vec<Client>,
    pub server: Vec<Server>,
}

#[derive(Deserialize,Debug,Clone)]
pub struct DroneConfig {
    pub id: NodeId,
    pub pdr: f32,
    pub connected_node_ids: Vec<NodeId>,
}


impl ParsedConfig {
    pub fn add_drone(&mut self, id: NodeId) {
        let new_drone = DroneConfig {
            id,
            pdr: 0.0,  // just a default pdr, it will be overwritten then
            connected_node_ids: Vec::new(),
        };

        self.drone.push(new_drone);
    }

    pub fn set_drone_connections(&mut self, drone_id: NodeId, connections: Vec<NodeId>) {
        if let Some(drone) = self.drone.iter_mut().find(|d| d.id == drone_id) {
            drone.connected_node_ids = connections;
        }
    }

    pub fn set_drone_pdr(&mut self, drone_id: NodeId, pdr: f32) {
        if let Some(drone) = self.drone.iter_mut().find(|d| d.id == drone_id) {
            drone.pdr = pdr;
        }
    }

    pub fn remove_drone_connections(&mut self, drone_id: NodeId) {
        for drone in &mut self.drone {
            drone.connected_node_ids.retain(|&id| id != drone_id);
        }

        for client in &mut self.client {
            client.connected_drone_ids.retain(|&id| id != drone_id);
        }

        for server in &mut self.server {
            server.connected_drone_ids.retain(|&id| id != drone_id);
        }
    }

    // Remove all connections to/from a specific client
    pub fn remove_client_connections(&mut self, client_id: NodeId) {
        for drone in &mut self.drone {
            drone.connected_node_ids.retain(|&id| id != client_id);
        }

        if let Some(client) = self.client.iter_mut().find(|c| c.id == client_id) {
            client.connected_drone_ids.clear();
        }
    }

    // Remove all connections to/from a specific server
    pub fn remove_server_connections(&mut self, server_id: NodeId) {
        for drone in &mut self.drone {
            drone.connected_node_ids.retain(|&id| id != server_id);
        }
        if let Some(server) = self.server.iter_mut().find(|s| s.id == server_id) {
            server.connected_drone_ids.clear();
        }
    }


    pub fn detect_topology(&self) -> Option<String> {
        use std::collections::{HashMap, HashSet};

        let drone_count = self.drone.len();
        if drone_count != 10 {
            return None;
        }

        let drone_vec: Vec<NodeId> = {
            let mut v = self.drone.iter().map(|d| d.id).collect::<Vec<_>>();
            v.sort();
            v
        };

        let drone_set: HashSet<NodeId> = drone_vec.iter().copied().collect();

        let connections: HashMap<_, HashSet<_>> = self.drone.iter()
            .map(|d| {
                let filtered = d.connected_node_ids
                    .iter()
                    .cloned()
                    .filter(|id| drone_set.contains(id))
                    .collect::<HashSet<_>>();
                (d.id, filtered)
            })
            .collect();

        // === STAR ===
        let is_star = {
            let mut ok = true;
            for (i, &id) in drone_vec.iter().enumerate() {
                let expected1 = drone_vec[(i + 3) % 10];
                let expected2 = drone_vec[(i + 7) % 10];

                if let Some(neigh) = connections.get(&id) {
                    if neigh.len() != 2 || !(neigh.contains(&expected1) && neigh.contains(&expected2)) {
                        ok = false;
                        break;
                    }
                } else {
                    ok = false;
                    break;
                }
            }
            ok
        };

        // === DOUBLE CHAIN ===
        let is_double_chain = {
            let mut deg2 = 0;
            let mut deg3 = 0;
            let mut symmetric_links = 0;

            for (&a, neighbors) in &connections {
                match neighbors.len() {
                    2 => deg2 += 1,
                    3 => deg3 += 1,
                    _ => {},
                }

                for &b in neighbors {
                    if let Some(n_b) = connections.get(&b) {
                        if n_b.contains(&a) {
                            symmetric_links += 1;
                        }
                    }
                }
            }
            let mutual_pairs = symmetric_links / 2;

            deg2 == 4 && deg3 == 6 && mutual_pairs >= 13
        };

        // === BUTTERFLY ===
        let is_butterfly = {
            let mut degrees = Vec::new();
            let mut id_to_neighbors = HashMap::new();

            for d in &self.drone {
                let filtered: Vec<NodeId> = d.connected_node_ids
                    .iter()
                    .filter(|&&n| drone_set.contains(&n))
                    .cloned()
                    .collect();

                id_to_neighbors.insert(d.id, filtered.clone());
                degrees.push(filtered.len());
            }

            let deg2 = degrees.iter().filter(|&&d| d == 2).count();
            let deg3 = degrees.iter().filter(|&&d| d == 3).count();

            // Butterfly core check
            let has_cross_core = id_to_neighbors.get(&9).map_or(false, |n| n.contains(&5) && n.contains(&7) && n.contains(&10)) &&
                id_to_neighbors.get(&10).map_or(false, |n| n.contains(&6) && n.contains(&8) && n.contains(&9));

            deg2 == 4 && deg3 == 6 && has_cross_core
        };

        // === TREE ===
        let is_tree = {
            let mut deg2 = 0;
            let mut deg3 = 0;
            let mut deg4 = 0;
            let mut deg6 = 0;

            for (_id, neighbors) in &connections {
                match neighbors.len() {
                    2 => deg2 += 1,
                    3 => deg3 += 1,
                    4 => deg4 += 1,
                    6 => deg6 += 1,
                    _ => {}
                }
            }
            deg2 == 1 && deg4 == 2 && deg6 == 3 && deg3 == 4
        };

        //subnet
        let is_subnet = {
            let high_deg = connections.values().filter(|n| n.len() >= 4).count();
            high_deg >= 3
        };

        if is_tree {
            return Some("Tree".to_string());
        }

        if is_subnet {
            return Some("Sub-Net".to_string());
        }
        if is_star {
            return Some("Star".to_string());
        }

        if is_butterfly{
            return Some("Butterfly".to_string());
        }

        if is_double_chain {
            return Some("Double Chain".to_string());
        }

        // Fallback
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


#[derive(Debug)]
pub struct MyDrone {
    pub id: NodeId,
    controller_send: Sender<DroneEvent>,
    controller_recv: Receiver<DroneCommand>,
    packet_recv: Receiver<Packet>,
    packet_send: HashMap<NodeId, Sender<Packet>>,
    pdr: f32,
}

use wg_2024::drone::Drone as DroneTrait;

impl DroneImplementation for MyDrone {
    fn run(&mut self) {
        <Self as DroneTrait>::run(self);
    }
}


// Trait for drone implementations
pub trait DroneImplementation: Send + 'static {
    fn run(&mut self);
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

    }
}

macro_rules! impl_drone_adapter {
    ($name:ty) => {
        impl DroneImplementation for $name {
            fn run(&mut self) {
                    <Self as wg_2024::drone::Drone>::run(self);

                }


        }
    };
}

/*
Group Krusty_club buys:
-Ledron James
-CppEnjoyers
-Dr ones
-Skylink
-Rustastics
-Bagel bomber
-Fungi
-Rust
-Rustafarian
-Rolling Drone
 */

use LeDron_James::Drone as LeDron_JamesDrone;
use ap2024_unitn_cppenjoyers_drone::CppEnjoyersDrone;
use dr_ones::Drone as DrOnesDrone;
use skylink::SkyLinkDrone;
use rustastic_drone::RustasticDrone;
use bagel_bomber::BagelBomber;
use fungi_drone::FungiDrone;

use wg_2024_rust::drone::RustDrone;
use rustafarian_drone::RustafarianDrone;
use rolling_drone::RollingDrone;

// === Macro Calls for All 10 Drones ===
impl_drone_adapter!(BagelBomber);
impl_drone_adapter!(FungiDrone);
impl_drone_adapter!(RustDrone);
impl_drone_adapter!(RustafarianDrone);
impl_drone_adapter!(RollingDrone);
impl_drone_adapter!(LeDron_JamesDrone);
impl_drone_adapter!(DrOnesDrone);
impl_drone_adapter!(SkyLinkDrone);
impl_drone_adapter!(RustasticDrone);
impl_drone_adapter!(CppEnjoyersDrone);

pub struct DroneWithId {
    pub id: NodeId,
    pub instance: Box<dyn DroneImplementation>,
    pub group_name: Option<String>,

}


pub struct NetworkInitializer {
    config: Config,
    pub(crate) drone_impls: Vec<DroneWithId>,

    pub(crate) packet_senders: Arc<Mutex<HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>>>,
    pub(crate) packet_receivers: Arc<Mutex<HashMap<NodeId, Receiver<Packet>>>>,

    pub(crate) command_senders: Arc<Mutex<HashMap<NodeId, Sender<DroneCommand>>>>,
    pub(crate) command_receivers: HashMap<NodeId, Receiver<DroneCommand>>,

    pub(crate) controller_event_receiver: Option<Receiver<DroneEvent>>,
    pub(crate) event_sender: Option<Sender<DroneEvent>>,

    controller_tx: Sender<DroneEvent>,
    controller_rx: Receiver<DroneCommand>,
    simulation_controller: Option<Arc<Mutex<SimulationController>>>,
    simulation_log: Arc<Mutex<Vec<String>>>,

    pub(crate) shared_senders: Option<Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>>,
}





impl NetworkInitializer {
    pub fn new(config_path: &str, drone_impls: Vec<DroneWithId>, simulation_log: Arc<Mutex<Vec<String>>>, shared_senders: Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>) -> Result<Self, Box<dyn Error>> {
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

            packet_senders: Arc::new(Mutex::new(HashMap::new())),
            packet_receivers: Arc::new(Mutex::new(HashMap::new())),

            command_senders: Arc::new(Mutex::new(HashMap::new())),
            command_receivers: HashMap::new(),

            controller_event_receiver: None,
            event_sender: None,

            controller_tx,
            controller_rx,
            simulation_controller: None,
            simulation_log,
            shared_senders: Some(shared_senders),
        })

    }

    pub fn set_controller(&mut self, ctrl: Arc<Mutex<SimulationController>>) {
        self.simulation_controller = Some(ctrl);
    }

    pub fn initialize(&mut self, gui_input_queue: SharedGuiInput, host_receivers: HashMap<NodeId, Receiver<Packet>>,
    ) -> Result<(), Box<dyn Error>> {
       // Validate the network configuration
        self.validate_config()?;

        // Distribute drone implementations and spawn drone threads
        self.initialize_drones();

        // Spawn client threads
        self.initialize_clients(gui_input_queue.clone(),self.simulation_log.clone(),&host_receivers);

        // Spawn server threads
        self.initialize_servers(gui_input_queue.clone(),self.simulation_log.clone(),&host_receivers);

        Ok(())

    }

    fn validate_config(&self) -> Result<(), Box<dyn Error>> {
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

            if client.connected_drone_ids.len() > 2 {
                return Err("Client cannot connect to more than 2 drones".into());
            }

            if client.connected_drone_ids.is_empty() {
                return Err("Client must connect to at least 1 drone".into());
            }

            let mut client_connections = HashSet::new();
            for &drone_id in &client.connected_drone_ids {
                if !client_connections.insert(drone_id) {
                    return Err("Duplicate connection in client.connected_drone_ids".into());
                }
            }

            if client.connected_drone_ids.contains(&client.id) {
                return Err("Client cannot connect to itself".into());
            }
        }

        for server in &self.config.server {
            if !all_ids.insert(server.id) {
                return Err("Duplicate node ID found".into());
            }

            if server.connected_drone_ids.len() < 2 {
                return Err("Server must connect to at least 2 drones".into());
            }

            let mut server_connections = HashSet::new();
            for &drone_id in &server.connected_drone_ids {
                if !server_connections.insert(drone_id) {
                    return Err("Duplicate connection in server.connected_drone_ids".into());
                }
            }

            if server.connected_drone_ids.contains(&server.id) {
                return Err("Server cannot connect to itself".into());
            }
        }

        self.check_bidirectional_connections()?;
        self.check_connected_graph()?;
        self.check_edges_property()?;

        Ok(())
    }

    fn check_bidirectional_connections(&self) -> Result<(), Box<dyn Error>> {
        let mut node_connections: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();

        for drone in &self.config.drone {
            let entry = node_connections.entry(drone.id).or_insert_with(HashSet::new);
            for &connected_id in &drone.connected_node_ids {
                entry.insert(connected_id);
            }
        }
        for client in &self.config.client {
            let entry = node_connections.entry(client.id).or_insert_with(HashSet::new);
            for &drone_id in &client.connected_drone_ids {
                entry.insert(drone_id);
            }
            for &drone_id in &client.connected_drone_ids {
                if let Some(drone_connections) = node_connections.get(&drone_id) {
                    if !drone_connections.contains(&client.id) {
                        return Err(format!("Connection between client {} and drone {} is not bidirectional", client.id, drone_id).into());
                    }
                } else {
                    return Err(format!("Client {} connects to non-existent drone {}", client.id, drone_id).into());
                }
            }
        }

        for server in &self.config.server {
            let entry = node_connections.entry(server.id).or_insert_with(HashSet::new);
            for &drone_id in &server.connected_drone_ids {
                entry.insert(drone_id);
            }
            for &drone_id in &server.connected_drone_ids {
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
        let mut adj_list: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        for drone in &self.config.drone {
            adj_list.entry(drone.id).or_default().extend(&drone.connected_node_ids);
            for &neighbor in &drone.connected_node_ids {
                adj_list.entry(neighbor).or_default().push(drone.id);
            }
        }

        for client in &self.config.client {
            adj_list.entry(client.id).or_default().extend(&client.connected_drone_ids);
            for &neighbor in &client.connected_drone_ids {
                adj_list.entry(neighbor).or_default().push(client.id);
            }
        }

        for server in &self.config.server {
            adj_list.entry(server.id).or_default().extend(&server.connected_drone_ids);
            for &neighbor in &server.connected_drone_ids {
                adj_list.entry(neighbor).or_default().push(server.id);
            }
        }

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
        let mut drone_adj_list: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        for drone in &self.config.drone {
            let drone_connections: Vec<NodeId> = drone
                .connected_node_ids
                .iter()
                .filter(|&&id| {
                    self.config.drone.iter().any(|d| d.id == id)
                })
                .cloned()
                .collect();

            drone_adj_list.insert(drone.id, drone_connections);
        }

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

        if visited.len() != drone_adj_list.len() {
            return Err("Drone-only graph is not connected (clients and servers must be at edges)".into());
        }

        Ok(())
    }

    pub fn setup_channels(
        &mut self,
        inbox_senders: Arc<Mutex<HashMap<NodeId, Sender<Packet>>>>
    ) -> (HashMap<NodeId, Receiver<Packet>>, HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>, Receiver<DroneEvent>) {

        let all_node_ids: Vec<NodeId> = self
            .config
            .drone
            .iter()
            .map(|d| d.id)
            .chain(self.config.client.iter().map(|c| c.id))
            .chain(self.config.server.iter().map(|s| s.id))
            .collect();

        let mut receivers = HashMap::new();
        let mut senders = HashMap::new();

        for &id in &all_node_ids {
            let (tx, rx) = unbounded::<Packet>();
            receivers.insert(id, rx);
            senders.insert(id, tx);
        }

        {
            let mut inbox_map = inbox_senders.lock().unwrap();
            for (&id, tx) in &senders {
                inbox_map.insert(id, tx.clone());
            }
        }



        let mut packet_senders_map = HashMap::new();
        let mut shared_senders_map = HashMap::new();

        for &id in &all_node_ids {
            let mut neighbor_senders = HashMap::new();

            let neighbors: Vec<NodeId> = if let Some(drone) = self.config.drone.iter().find(|d| d.id == id) {
                drone.connected_node_ids.clone()
            } else if let Some(client) = self.config.client.iter().find(|c| c.id == id) {
                client.connected_drone_ids.clone()
            } else if let Some(server) = self.config.server.iter().find(|s| s.id == id) {
                server.connected_drone_ids.clone()
            } else {
                vec![]
            };

            for &neighbor in &neighbors {
                if let Some(sender_to_neighbor) = senders.get(&neighbor) {
                    neighbor_senders.insert(neighbor, sender_to_neighbor.clone());

                    shared_senders_map.insert((id, neighbor), sender_to_neighbor.clone());
                }
            }

            packet_senders_map.insert(id, neighbor_senders);
        }

        // 🚀 Setup controller <-> drone command/event channels
        let mut command_senders_map = HashMap::new();
        let mut command_receivers_map = HashMap::new();
        let (event_tx, event_rx) = unbounded::<DroneEvent>(); // Shared receiver for controller

        for drone in &self.config.drone {
            let (cmd_tx, cmd_rx) = unbounded::<DroneCommand>();
            command_senders_map.insert(drone.id, cmd_tx);
            command_receivers_map.insert(drone.id, cmd_rx);
        }

        self.controller_event_receiver = Some(event_rx.clone());
        self.command_receivers = command_receivers_map;
        *self.command_senders.lock().unwrap() = command_senders_map;
        self.event_sender = Some(event_tx);

        *self.packet_senders.lock().unwrap() = packet_senders_map.clone();

        *self.packet_receivers.lock().unwrap() = receivers.clone();


        if let Some(shared) = &self.shared_senders {
            let mut flat = shared.lock().unwrap();
            flat.clear();
            for ((src, dst), tx) in shared_senders_map {
                flat.insert((src, dst), tx);
            }
        }

        (receivers, packet_senders_map , event_rx)
    }


    pub fn initialize_drones(&mut self) {
        for DroneWithId { id, group_name, mut instance } in self.drone_impls.drain(..) {
            if let Some(group) = group_name {
                println!("🛸 Drone {} uses {}", id, group);
            } else {
                println!("🛸 Drone {} uses fallback MyDrone", id);
            }

            thread::spawn(move || {
                instance.run();
            });
        }
    }



    fn initialize_clients(&mut self, gui_input: SharedGuiInput, log: Arc<Mutex<Vec<String>>>, host_receivers: &HashMap<NodeId, Receiver<Packet>>) {
        for (i, client) in self.config.client.iter().enumerate() {
            let log_clone=log.clone();

            let client_id = client.id;

            // Get the full sender map for this client (set up in setup_channels)
            let senders = self
                .packet_senders
                .lock().unwrap().get(&client_id)
                .expect("packet_senders must have client entries")
                .clone();

            let client_rx = self
                .packet_receivers
                .lock().unwrap().get(&client_id)
                .expect("setup_channels must have created this")
                .clone();

            let gui_clone = gui_input.clone();
            let log_clone = self.simulation_log.clone();
            let shared_senders = Arc::clone(self.shared_senders.as_ref().unwrap());
            let shortcut_rx = host_receivers.get(&client_id).cloned().unwrap();

            if self.config.client.len() == 2 {
                if client_id % 2 == 0 {
                    // client2
                    thread::spawn(move || {
                        println!("client2 spawned");
                        let mut cl2 = client2::MyClient::new(client_id, client_rx, senders, None,Some(shortcut_rx));
                        cl2.shared_senders= Some(shared_senders.clone());
                        cl2.attach_log(log_clone);
                        cl2.run(gui_clone);

                    });
                } else {
                    // client1
                    thread::spawn(move || {
                        println!("client1 spawned");
                        let mut cl1 = client1::MyClient::new(client_id, client_rx, senders, HashMap::new(), None, HashSet::new(), None,Some(shortcut_rx));
                        cl1.shared_senders= Some(shared_senders.clone());
                        cl1.attach_log(log_clone);
                        cl1.run(gui_clone);
                    });
                }
            } else {
                if i % 2 == 0 {
                    thread::spawn(move || {
                        println!("client2 spawned");
                        let mut cl2 = client2::MyClient::new(client_id, client_rx, senders, None,Some(shortcut_rx));
                        cl2.shared_senders= Some(shared_senders.clone());
                        cl2.attach_log(log_clone);
                        cl2.run(gui_clone);
                    });
                } else {
                    thread::spawn(move || {
                        println!("client1 spawned");
                        let mut cl1 = client1::MyClient::new(client_id, client_rx, senders, HashMap::new(), None, HashSet::new(), None,Some(shortcut_rx));
                        cl1.shared_senders= Some(shared_senders.clone());
                        cl1.attach_log(log_clone);
                        cl1.run(gui_clone);
                    });
                }
            }
        }
    }

    fn initialize_servers(&mut self, gui_input: SharedGuiInput, log: Arc<Mutex<Vec<String>>>, host_receivers: &HashMap<NodeId, Receiver<Packet>>) {
        for server in &self.config.server {
            let log_clone=log.clone();
            let server_id = server.id;

            let senders = self
                .packet_senders
                .lock().unwrap().get(&server_id)
                .expect("packet_senders must have server entries")
                .clone();

            let server_rx = self
                .packet_receivers
                .lock().unwrap().get(&server_id)
                .expect("setup_channels must have created this")
                .clone();

            let gui_clone = gui_input.clone();
            let shared_senders = Arc::clone(self.shared_senders.as_ref().unwrap());
            let shortcut_rx = host_receivers.get(&server_id).cloned().unwrap();


            thread::spawn(move || {
                let mut srv = server::server::new(server_id as u8, senders, server_rx, None,Some(shortcut_rx));
                srv.attach_log(log_clone);
                srv.shared_senders= Some(shared_senders.clone());
                srv.run(gui_clone);
            });
        }
    }



    pub fn create_drone_implementations(&self) -> Vec<DroneWithId> {
        let mut implementations: Vec<DroneWithId> = Vec::new();

        let group_implementations = Self::load_group_implementations();
        let num_impls = group_implementations.len();

        let drone_configs = self.config.drone.clone();
        let num_drones = drone_configs.len();

        let mut impl_counts = vec![num_drones / num_impls; num_impls];
        for i in 0..(num_drones % num_impls) {
            impl_counts[i] += 1;
        }

        let group_keys: Vec<String> = group_implementations.keys().cloned().collect();

        let mut impl_index = 0;
        let mut count = 0;

        for drone_config in drone_configs {
            if num_impls > 0 && count >= impl_counts[impl_index] {
                impl_index += 1;
                count = 0;
            }

            let id = drone_config.id;
            let pdr = drone_config.pdr;

            let packet_recv = self.packet_receivers.lock().unwrap().get(&id).unwrap().clone();
            let packet_send = self.packet_senders.lock().unwrap().get(&id).unwrap().clone();

            let command_recv = self.command_receivers.get(&id).expect("Missing command_receiver").clone();
            let event_send = self.event_sender.as_ref().expect("Missing event_sender").clone();

            let (drone_impl, group_name) = if num_impls == 0 {
                (
                    Box::new(MyDrone::new(id, event_send, command_recv, packet_recv, packet_send, pdr)) as Box<dyn DroneImplementation>,
                    None,
                )
            } else {
                let impl_key = &group_keys[impl_index];
                if let Some(create_fn) = group_implementations.get(impl_key) {
                    (
                        create_fn(id, event_send, command_recv, packet_recv, packet_send, pdr),
                        Some(impl_key.clone()),
                    )
                } else {
                    warn!("⚠️ Unknown group key, falling back to MyDrone");
                    (
                        Box::new(MyDrone::new(id, event_send, command_recv, packet_recv, packet_send, pdr)) as Box<dyn DroneImplementation>,
                        None,
                    )
                }
            };

            implementations.push(DroneWithId {
                id,
                instance: drone_impl,
                group_name,
            });

            count += 1;
        }

        implementations
    }


    fn load_group_implementations() -> HashMap<String, GroupImplFactory> {
        let mut group_implementations = HashMap::new();

        group_implementations.insert(
            "group_1".to_string(),
            Box::new(|id: NodeId, sim_contr_send: Sender<DroneEvent>, sim_contr_recv: Receiver<DroneCommand>,
                      packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, pdr: f32|
                      -> Box<dyn DroneImplementation> {
                Box::new(LeDron_JamesDrone::new(
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
            "group_2".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(RustafarianDrone::new(
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
            "group_3".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(CppEnjoyersDrone::new(
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
            "group_4".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(BagelBomber::new(
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
            "group_5".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(DrOnesDrone::new(
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
            "group_6".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(RustasticDrone::new(
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
            "group_7".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(SkyLinkDrone::new(
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
            "group_8".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(FungiDrone::new(
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
            "group_9".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(RustDrone::new(
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
            "group_10".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr|
                      -> Box<dyn DroneImplementation> {
                Box::new(RollingDrone::new(
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

    pub fn get_shared_packet_senders(&self) -> Arc<Mutex<HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>>> {
        Arc::clone(&self.packet_senders)
    }

    pub fn get_shared_packet_receivers(&self) -> Arc<Mutex<HashMap<NodeId, Receiver<Packet>>>> {
        Arc::clone(&self.packet_receivers)
    }



}



