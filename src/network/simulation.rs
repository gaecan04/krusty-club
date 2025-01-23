/*


use std::collections::HashMap;
use std::process::Command;
use crossbeam_channel::{unbounded, Receiver, Sender};
use wg_2024::network::NodeId;
use wg_2024::packet::NodeType;
use wg_2024::packet::Packet;
use wg_2024::controller::{DroneCommand, DroneEvent};  // Assuming command.rs has the necessary enums

/// Trait that defines the interface for controlling the simulation.
pub trait SimulationController {
    fn crash(&mut self, crashed: NodeId);
    fn add_sender(&mut self, dst_id: NodeId, sender: Sender<Packet>); //da fare
    fn remove_sender(&mut self, nghb_id:NodeId); //da fare
    fn set_packet_drop_rate(pdr:f32); //da fare
    fn spawn_node(&mut self, node_id: NodeId, node_type: NodeType, neighbors: Vec<(NodeId, NodeType)>); //da completare
    //fn message_sent(&self, source: NodeId, target: NodeId); //inside drone
    fn handle_node_event(&mut self, event: DroneEvent);
    fn message_sent(&self, source: NodeId, target: NodeId);
}

/// Enum to define node types.
///
/*
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeType {
    pub Server,
    pub Client,
    pub Drone,
}
*/

/// The implementation of the SimulationController trait.
pub struct SimulationControllerImpl {
    //nodes : Vec<(NodeId, NodeType)>,
    pub nodes_and_neighbors: HashMap<NodeId ,Vec<(NodeId, NodeType)>>,
    // You could add more fields to store the network state or other data
    pub drone_channels_command: HashMap<NodeId,Sender<DroneCommand>>, // Map of NodeId to sender channels
    pub drone_channels_packet: HashMap<NodeId, Sender<Packet>>,
    pub drone_receiver_event:  Receiver<DroneEvent>,

}

impl SimulationControllerImpl {
    /// Create a new instance of the SimulationControllerImpl
    pub fn new() -> Self {
        let (_event_sender, event_receiver) = unbounded();
        Self {
            //nodes: Vec::new(),
            nodes_and_neighbors: HashMap::new(),
            drone_channels_command: HashMap::new(),
            drone_channels_packet: HashMap::new(),
            drone_receiver_event: event_receiver ,
        }
    }
    pub fn add_channel(&mut self, node_id: NodeId, channel: Sender<Packet>) {
        //self.drone_channels_packet.insert(node_id, channel);

    }
}

impl SimulationController for SimulationControllerImpl {
    /// Handle a node crash event.
    fn crash(&mut self, crashed: NodeId) {
        let neighbours = self.nodes_and_neighbors.get(&crashed).unwrap();
        for neighbour in neighbours {
            if neighbour.1 != NodeType::Drone{
                let neighbours1 = self.nodes_and_neighbors.get(&neighbour.0).unwrap();
                if neighbours1.len() == 1 {
                    println!("node has 1 neighbor only: client and server must be connected to at least one drone");
                    return
                }
            }
            let sender_remove= self.drone_channels_command.get(&neighbour.0).unwrap();
            sender_remove.send(DroneCommand::RemoveSender(crashed)).expect("Could not send remove sender");
            println!("Sender from {:?} to crashed node {:?} removed", neighbour.0, crashed);

        }
        //send the command "crash" to "crashed";
        let sender_channel= self.drone_channels_command.get(&crashed).unwrap();
        sender_channel.send(DroneCommand::Crash).expect("Command not sent");


        // Remove the crashed node
        self.nodes_and_neighbors.remove(&crashed);
        self.drone_channels_command.remove(&crashed);
        self.drone_channels_packet.remove(&crashed);

        /*
        1. ricevo NodeId
        2. vado in nodes_and_neighbors e trovo il vettore dei vicini di NodeId
        3. Per ogni nodo nel vettore dei vicini, controllo il NodeType.
            - In caso di server e client: controllo la lunghezza del vettore dei vicini:
                  - se .len() == 1 -> non processo il crash
                  - se .len() > 1 -> processo tranquillamente
            - In caso di drone procedo normalmente
         */
    }

    fn add_sender(&mut self, dst_id: NodeId, sender: Sender<Packet>){}

    fn remove_sender(&mut self, nghb_id:NodeId){

    }

    fn set_packet_drop_rate(pdr:f32){}


    /// Spawn a new node of a specific type.
    fn spawn_node(&mut self, node_id: NodeId, node_type: NodeType, neighbors: Vec<(NodeId, NodeType)>) {
        println!("Spawning node {} of type {:?}", node_id, node_type);
        self.nodes_and_neighbors.insert(node_id, neighbors);
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
                if let Some(&source) = packet.routing_header.hops.last() {
                    // Attempt to find the sender channel for the source
                    if let Some(sender) = self.drone_channels_packet.get(&source) {
                       println!("Trying to send to source {:?}", source);
                        if let Err(err) = sender.send(packet) {
                            eprintln!("Failed to forward packet to source {}: {}", source, err);
                        } else {
                            println!("Drone forwarded to source {} correctly: ", source);
                            //eprintln!("Failed to forward packet to source {}:", source, );
                            //send packet to "source" (last element of routingheader.hops)

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

 */
