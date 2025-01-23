/*

use std::collections::{HashMap, HashSet};
use std::fs;
use toml;
use std::thread;
use crossbeam_channel::{unbounded, select_biased, select, Receiver, Sender};
use serde::Deserialize;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::packet::Packet;
use wg_2024::drone::Drone;
use crate::droneK::drone::MyDrone;

#[cfg(feature = "serialize")]
use serde::Deserialize;
use wg_2024::network::NodeId;

#[derive(Debug, Clone,Deserialize)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct DroneNetIn {
    pub id: NodeId,
    pub connected_node_ids: Vec<NodeId>,
    pub pdr: f32,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Deserialize, Debug)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Topology {
    pub drone: Vec<dyn Drone>,
    pub client: Vec<Client>,
    pub server: Vec<Server>,
}

// Helper function to read and parse the TOML file
fn parse_topology(file_path: &str) -> Topology {
    let file_content = fs::read_to_string(file_path)
        .expect("Failed to read topology file");
    let topology: Topology= toml::from_str(&file_content)
        .expect("Failed to parse topology file");
    topology
}

// Validate the network initialization file
fn validate_topology(topology: &Topology) {
    // Check for unique node IDs
    let mut all_ids: HashSet<u8> = HashSet::new();
    for drone in &topology.drone {
        if !all_ids.insert(drone.id) {
            panic!("Duplicate ID found: {}", drone.id);
        }
        for conn_id in &drone.connected_node_ids {
            if *conn_id == drone.id {
                panic!("Drone {} cannot connect to itself", drone.id);
            }
        }
    }

    for client in &topology.client {
        if !all_ids.insert(client.id) {
            panic!("Duplicate ID found: {}", client.id);
        }
        if client.connected_drone_ids.len() > 2 {
            panic!("Client {} cannot be connected to more than 2 drones", client.id);
        }
    }

    for server in &topology.server {
        if !all_ids.insert(server.id) {
            panic!("Duplicate ID found: {}", server.id);
        }
        if server.connected_drone_ids.len() < 2 {
            panic!("Server {} must be connected to at least 2 drones", server.id);
        }
    }

    // Additional validations can be added here (e.g., bidirectional graph check)
}

fn initialize_network(topology: Topology) {
    // Maps to store channels and node configurations
    let mut node_event_channels: HashMap<u8, Sender<DroneEvent>> = HashMap::new(); //each drone sender of event
    let mut node_command_channels: HashMap<u8, Sender<DroneCommand>> = HashMap::new(); //here there will be the various senders from the simulation controller
    let mut node_neighbors: HashMap<u8, HashMap<u8, Sender<Packet>>> = HashMap::new(); //hashmap of neighbors of each drone with every sender to neighbors
    let mut packet_channels: HashMap<u8, (Sender<Packet>, Receiver<Packet>)> = HashMap::new();// ????

    // Create channels for drones
    for drone in &topology.drone {
        let (packet_sender, packet_receiver) = unbounded::<Packet>(); //drone <--> drone/ client / server
        let (event_send, _) = unbounded::<DroneEvent>(); //drone --> Sim Contr
        let (command_send, command_recv) = unbounded::<DroneCommand>(); //Sim Contr <--> drone

        packet_channels.insert(drone.id, (packet_sender.clone(), packet_receiver));
        node_event_channels.insert(drone.id, event_send.clone());
        node_command_channels.insert(drone.id, command_send.clone());

        let neighbors = drone
            .connected_node_ids
            .iter()
            .map(|&neighbor_id| {
                let (send, recv) = unbounded();
                (neighbor_id, send.clone())
            })
            .collect::<HashMap<u8, Sender<Packet>>>();
        node_neighbors.insert(drone.id, neighbors);
    }

    // Create channels for clients and servers
    for client in &topology.client {
        let (client_send, client_recv) = unbounded::<Packet>();
        //we can add here the channel with SC
        packet_channels.insert(client.id, (client_send.clone(), client_recv));
    }
    for server in &topology.server {
        let (server_send, server_recv) = unbounded::<Packet>();
        //we can add here the channel with SC
        packet_channels.insert(server.id, (server_send.clone(), server_recv));
    }

    // Spawn drone threads
    for drone in topology.drone {
        let id = drone.id;
        let pdr = drone.pdr;
        let packet_send = node_neighbors.get(&id).unwrap().clone();
        let sim_contr_send = node_event_channels.get(&id).unwrap().clone();
        let sim_contr_recv = node_command_channels.get(&id).unwrap().clone();
        let packet_recv = packet_channels.get(&id).unwrap().1.clone();

        thread::spawn(move || {
            let mut drone_node = Drone::new(
                id,
                sim_contr_send,
                sim_contr_recv,
                packet_recv,
                packet_send,
                pdr,
            );
            drone_node.run();
        });
    }

    /*
    // Spawn client threads
    for client in topology.client {
        let id = client.id;
        let connected_drones = client.connected_drone_ids.clone();
        let packet_channel = packet_channels.get(&id).unwrap().1.clone();

        thread::spawn(move || {
            let mut client_node = ClientImpl::new(id, connected_drones, packet_channel);
            client_node.run();
        });
    }

    // Spawn server threads
    for server in topology.server {
        let id = server.id;
        let connected_drones = server.connected_drone_ids.clone();
        let packet_channel = packet_channels.get(&id).unwrap().1.clone();

        thread::spawn(move || {
            let mut server_node = ServerImpl::new(id, connected_drones, packet_channel);
            server_node.run();
        });
    }

    // Create and run the simulation controller
    let simulation_controller = SimulationControllerImpl {
        nodes_and_neighbors: node_neighbors,
        drone_channels_command: node_command_channels,
        drone_channels_packet: packet_channels,
        drone_receiver_event: unbounded().1,
    };

    thread::spawn(move || {
        simulation_controller.run();
    });

     */
}


// Function to run a drone node
/*
fn run_drone(id: u8, pdr: f32, connected_nodes: Vec<u8>, tx: Sender<_>, rx: Receiver<_>) {
    println!("Drone {} running with PDR {}", id, pdr);
    // Implement drone behavior (message processing, PDR handling, etc.)
}

// Function to run a client node

fn run_client(id: u8, connected_drones: Vec<u8>) {
    println!("Client {} running, connected to drones {:?}", id, connected_drones);
    // Implement client behavior
}

// Function to run a server node
fn run_server(id: u8, connected_drones: Vec<u8>) {
    println!("Server {} running, connected to drones {:?}", id, connected_drones);
    // Implement server behavior
}*/


pub fn run() {
    let topology = parse_topology("topologies/butterfly.toml");
    println!("{:?}", topology);
    //validate_topology(&topology);
    //initialize_network(topology);
}





 */