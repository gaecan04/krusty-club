/*
COSE DA FARE :
- le varie funzioni di interazione con i client: con i vari codici da mettere nei frammenti
- implementare sto grafo dimmerda
- fare simulazioni

*/

use petgraph::algo::dijkstra;
use std::collections::HashMap;
use std::mem::transmute;
use crossbeam_channel::{Receiver, RecvError, Sender};
use eframe::egui::accesskit::Node;
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use log::{info, error, warn, debug};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::VecDeque;


#[derive(Clone, Debug)]
pub struct NetworkGraph {
    graph: DiGraph<NodeId, usize>,
    node_indices: HashMap<NodeId, NodeIndex>,
}

impl NetworkGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            node_indices: HashMap::new(),
        }
    }

    pub fn add_node(&mut self, id: NodeId) -> NodeIndex {
        if let Some(&index) = self.node_indices.get(&id) {
            index
        } else {
            let index = self.graph.add_node(id);
            self.node_indices.insert(id, index);
            index
        }
    }
    pub fn remove_node(&mut self, node_id: NodeId) {
        if let Some(node_index) = self.node_indices.remove(&node_id) {
            self.graph.remove_node(node_index);
        } else {
            warn!("Tried to remove non-existing node {}", node_id);
        }
    }

    pub fn add_link(&mut self, a: NodeId, b: NodeId) {
        let a_idx = self.add_node(a);
        let b_idx = self.add_node(b);
        self.graph.update_edge(a_idx, b_idx, 0);
        self.graph.update_edge(b_idx, a_idx, 0);
    }
    pub fn increment_drop(&mut self, a: NodeId, b: NodeId) {
        if let (Some(&a_idx), Some(&b_idx)) = (self.node_indices.get(&a), self.node_indices.get(&b)) {
            if let Some(edge) = self.graph.find_edge(a_idx, b_idx) {
                if let Some(weight) = self.graph.edge_weight_mut(edge) {
                    *weight += 1;
                }
            }
            //increment also the other way
            if let Some(edge) = self.graph.find_edge(b_idx, a_idx) {
                if let Some(weight) = self.graph.edge_weight_mut(edge) {
                    *weight += 1;
                }
            }
        }
    }
    pub fn best_path(&self, source: NodeId, target: NodeId) -> Option<Vec<NodeId>> {
        let source_idx = *self.node_indices.get(&source)?;
        let target_idx = *self.node_indices.get(&target)?;

        // Dijkstra: restituisco anche da dove vengo (predecessore)
        let mut predecessors: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        let _ = dijkstra(&self.graph, source_idx, Some(target_idx), |e| {
            let from = e.source();
            let to = e.target();
            // Se trovo un nuovo nodo raggiungibile, registro da chi arrivo
            predecessors.entry(to).or_insert(from);
            *e.weight()
        });

        // Se non ho trovato un cammino fino a target, esco
        if !predecessors.contains_key(&target_idx) {
            return None;
        }

        // Ora ricostruisco il path usando i predecessori
        let mut path = vec![self.graph[target_idx]];
        let mut current = target_idx;
        while current != source_idx {
            if let Some(&prev) = predecessors.get(&current) {
                path.push(self.graph[prev]);
                current = prev;
            } else {
                return None;
            }
        }

        path.reverse();
        Some(path)
    }
    pub fn print_graph(&self) {
        println!("Current network graph:");

        for edge in self.graph.edge_references() {
            let source = self.graph[edge.source()];
            let target = self.graph[edge.target()];
            let weight = edge.weight();
            println!("{} <-> {} with {} drops", source, target, weight);
        }
    }
}

const MAX_CHAT_HISTORY: usize = 50; //50 messagges max for the chronology
#[derive(Debug, Clone)]
pub struct server {
    pub id: u8, // Server ID
    pub received_fragments: HashMap<(u64, NodeId), Vec<Option<[u8; 128]>>>, // Maps (session_id, src_id) to fragment data
    pub fragment_lengths: HashMap<(u64, NodeId), u8>, // Maps (session_id, src_id) to length of the last fragment
    pub packet_sender: HashMap<NodeId, Sender<Packet>>, // Hashmap containing each sender channel to the neighbors (channels to send packets to clients)
    pub packet_receiver: Receiver<Packet>, // Channel to receive packets from clients/drones
    pub registered_clients: HashMap<NodeId,Vec<NodeId>>,
    pub network_graph: NetworkGraph,
    pub chat_history: HashMap<(NodeId,NodeId), VecDeque<String>>,
    //recovery_in_progress:  HashMap<(u64, NodeId), bool>, // Tracks if recovery is already in progress for a session
    //drop_counts: HashMap<(u64, NodeId), usize>, // Track number of drops per session
}

impl server {
    pub(crate) fn new(id: u8, packet_sender: HashMap<NodeId,Sender<Packet>>, packet_receiver: Receiver<Packet>) -> Self {
        // Log server creation
        info!("Server {} created.", id);

        Self {
            id,
            received_fragments: HashMap::new(),
            fragment_lengths: HashMap::new(),
            packet_sender: packet_sender,
            packet_receiver,
            registered_clients: HashMap::new(),
            network_graph: NetworkGraph::new(),
            chat_history:HashMap::new(),
            //recovery_in_progress: HashMap::new(),
            //drop_counts: HashMap::new(),
        }
    }

    /// Run the server to process incoming packets and handle fragment assembly
    pub fn run(&mut self) {
        // Initialize logger (logging setup for your program)
        if let Err(e) = env_logger::builder().try_init() {
            eprintln!("Failed to initialize logger: {}", e);
        }

        info!("Server {} started running.", self.id);

        loop {
            match self.packet_receiver.recv() {
                Ok(mut packet) => {
                    match &packet.pack_type {
                        PacketType::MsgFragment(fragment) => {
                            self.send_ack(&packet, &fragment);
                            self.handle_fragment(packet.session_id, fragment, packet.routing_header);
                        }
                        PacketType::Nack(nack) => {
                            self.handle_nack(packet.session_id, nack, packet.routing_header);
                        }
                        PacketType::Ack(ack) => {
                            // Process ACKs, needed for simulation controller to know when to print the message
                            //self.handle_ack(packet.session_id, ack, packet.routing_header);
                        }
                        PacketType::FloodRequest(flood_request) => {
                            // Process flood requests from clients trying to discover the network
                            // Server should send back a flood response
                            self.handle_flood_request(packet.session_id, flood_request, packet.routing_header);
                        }
                        PacketType::FloodResponse(flood_response) => {
                            // Process flood responses containing network information
                            self.handle_flood_response(packet.session_id, flood_response, packet.routing_header);
                        }
                        _ => {
                            warn!("Server {} received unexpected packet type.", self.id);
                        }
                    }
                }
                Err(e) => {
                    warn!("Error receiving packet: {}", e);
                    break;
                }
            }

        }
    }

    /// Handle fragment processing
    fn handle_fragment(&mut self, session_id: u64, fragment: &Fragment, routing_header: SourceRoutingHeader) {
        let key = (session_id, routing_header.hops[0]);

        // Initialize storage for fragments if not already present
        let entry = self.received_fragments.entry(key).or_insert_with(|| vec![None; fragment.total_n_fragments as usize]);

        // Check if the fragment is already received
        if entry[fragment.fragment_index as usize].is_some() {
            warn!("Duplicate fragment {} received for session {:?}", fragment.fragment_index, key);
            return;
        }

        // Store the fragment data
        entry[fragment.fragment_index as usize] = Some(fragment.data);

        // Update the last fragment's length if applicable
        if fragment.fragment_index == fragment.total_n_fragments - 1 {
            self.fragment_lengths.insert(key, fragment.length);
            info!("Last fragment received for session {:?} with length {}.", key, fragment.length);
        }

        // Check if all fragments have been received
        if entry.iter().all(Option::is_some) {
            // All fragments received, handle complete message
            self.handle_complete_message(key, routing_header.clone());
        }
    }

    fn handle_complete_message(&mut self, key: (u64, NodeId), routing_header: SourceRoutingHeader) {
        let fragments = self.received_fragments.remove(&key).unwrap();
        let total_length = fragments.len() * 128 - 128 + self.fragment_lengths.remove(&key).unwrap_or(128) as usize;

        let mut message = Vec::with_capacity(total_length);
        for fragment in fragments {
            message.extend_from_slice(&fragment.unwrap());
        }
        message.truncate(total_length);

        let message_string = String::from_utf8_lossy(&message).to_string();
        let session_id: u64 = key.0;
        let client_id = key.1;

        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();

        match tokens.as_slice() {
            ["[Login]"] => {
                self.registered_clients
                    .entry(session_id as NodeId)
                    .or_default()
                    .push(client_id);
                info!("Client {} registered to chat in session {}", client_id, session_id);
            }
            ["[ClientListRequest]"] => {
                if let Some(sender) = self.packet_sender.get(&client_id) {
                    let clients = self
                        .registered_clients
                        .get(&(session_id as NodeId))
                        .cloned()
                        .unwrap_or_default();
                    let response = format!("[ClientListResponse]::{:?}", clients);
                    self.send_chat_message(session_id, client_id, response, routing_header);
                }
            }
            ["[ChatRequest]", target_id_str] => {
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    let success = self.registered_clients
                        .get(&(session_id as NodeId))
                        .map_or(false, |list| list.contains(&target_id));
                    let response = format!("[ChatStart]::{}", success);
                    self.send_chat_message(session_id, client_id, response, routing_header);
                }
            }
            ["[MessageTo]", target_id_str, msg] => {
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    if self.registered_clients
                        .get(&(session_id as NodeId))
                        .map_or(false, |list| list.contains(&target_id))
                    {
                        let response = format!("[MessageFrom]::{}::{}", client_id, msg);
                        self.send_chat_message(session_id, target_id, response, routing_header);
                        //now we store the messages to have the "CHRONOLOGY" feature:
                        let entry= self.chat_history.entry((client_id.min(target_id), client_id.max(target_id))).or_insert_with(VecDeque::new);
                        let chat_entry = format!("{}: {}", client_id, msg);
                        entry.push_back(chat_entry);

                        if entry.len() > MAX_CHAT_HISTORY {
                            entry.pop_front(); // remove the oldest message
                        }
                    } else {
                        self.send_chat_message(session_id, client_id, "error_wrong_client_id!".to_string(), routing_header);
                    }
                }
            }
            ["[HistoryRequest]", target_id_str] => { //when client wants to see chronology
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    let key = (client_id.min(target_id), client_id.max(target_id));
                    let history = self.chat_history.get(&key);
                    let response = match history {
                        Some(messages) => messages.iter().cloned().collect::<Vec<String>>().join("\n"),
                        None => "No history available".to_string(),
                    };
                    self.send_chat_message(session_id, client_id, format!("[HistoryResponse]::{}", response), routing_header);
                }
            }
            ["[ChatFinish]"] => {
                if let Some(clients) = self.registered_clients.get_mut(&(session_id as NodeId)) {
                    clients.retain(|&id| id != client_id);
                    info!("Client {} finished chat in session {}", client_id, session_id);
                }
            }
            ["[Logout]"] => {
                if let Some(clients) = self.registered_clients.get_mut(&(session_id as NodeId)) {
                    clients.retain(|&id| id != client_id);
                    info!("Client {} logged out from session {}", client_id, session_id);
                }
            }
            _ => {
                warn!("Unrecognized message: {}", message_string);
                info!("Reassembled message for session {:?}: {:?}", key, message);
            }
        }
    }


    //fn process_nack(&mut self, nack: &Nack, packet: &mut Packet)
    fn handle_nack(&mut self, session_id: u64, nack: &Nack, routing_header: SourceRoutingHeader) {

        info!("Recieved NACK for fragment {} with type {:?} in session {}", nack.fragment_index, nack.nack_type, session_id);

        match nack.nack_type {
            NackType::Dropped => { // the only case i recieve nack Dropped is when i am forwarding the fragments to the second client
                // Nacktype: ErrorInRouting ---> floodRequest --> find crashed drone --> remove from graph --> calculate path to dijstra and keep sending
                //put a limit of 5 dropped fragments, at the 6th the server will flood the network sending a floodrequest
                // to the drone to whom it is connected.

                //Modus Operandi client/server:
                // prima operazione generale: floodRequest.
                /* Riceverò delle flood_response e da queste mi costruisco il grafo.
                    Una volta costruito il grafo ad ogni nack::Dropped che ricevo vado a modificare il peso di tutti i
                    collegamenti relativi a quel drone. Gli altri nack implicano che c'è qualche problema del tipo crashed Drone.
                    Come gestisco? Mando floodRequest, riceverò floodResponse che mi indicheranno il drone problematico, carpita
                    questa informazione lo vado a rimuovere dal grafo.
                 */
                //quando ricevo un nack::ErrorInRouting ... faccio un flooding normale (creo una flood_request normale),
                //poi riceverò flood_response e agisco sul grafo come al solito.

                // grafo fatto con pet_graph.
                warn!("Server {}: Received Nack::Dropped", self.id);

                if routing_header.hop_index >= 1 {
                    if let (Some(from), Some(to)) = (
                        routing_header.hops.get(routing_header.hop_index - 1),
                        routing_header.hops.get(routing_header.hop_index),
                    ) {
                        self.network_graph.increment_drop(*from, *to);
                        info!("Increased drop cost between {} and {}", from, to);
                    }
                }
                warn!("Received Nack::Dropped, modifying the costs in the graph")

            },
            NackType::ErrorInRouting(crashed_node_id) => {
                warn!("Server detected a crashed node {} due to ErrorInRouting.", crashed_node_id);

                // REMOVE the crashed node from the network graph
                self.network_graph.remove_node(crashed_node_id);

                info!("Server updated network graph after detecting crash of node {}", crashed_node_id);
                self.network_graph.print_graph(); // optional: print updated graph
            },
            _ => {
                    warn!("Received ErrorInRouting/DestinationIsDrone/UnexpectedRecipient NACK type, sending flood request");
                    let flood_request = FloodRequest {
                        flood_id: session_id,
                        initiator_id: self.id as NodeId,
                        path_trace: vec![(self.id as NodeId, NodeType::Server)], //starting from server ID.,
                    };
                    // Create the flood packet to send to all connected drones
                    let flood_packet = Packet::new_flood_request(
                        // Route to first drone in original path
                        SourceRoutingHeader {
                            hop_index: 1,
                            hops: routing_header.hops.iter().rev().cloned().collect(),
                        },
                        session_id,
                        flood_request
                    );

                    if let Some(next_hop) = flood_packet.routing_header.hops.get(1) {
                        if let Some(sender) = self.packet_sender.get(next_hop) {
                            match sender.try_send(flood_packet.clone()) {
                                Ok(()) => {
                                    info!("FloodRequest sent to node {} for session {}", next_hop, session_id);
                                }
                                Err(e) => {
                                    error!("Error sending flood packet: {:?}", e);
                                }
                            }
                        } else {
                            error!("No sender found for node {}", next_hop);
                        }
                    } else {
                        error!("No next hop available in routing header");
                    }
            }
            //di conseguenza successivamente il server riceverà delle floodResponse. Queste dovranno essere analizzate
            //ed interpretate per poi andare a modificare il grafo.
        }
    }

    // modifica cosi che ogni volta che ricevo un frammento mando un ack, con il proprio index number.
    fn send_ack(&mut self, packet: &Packet, fragment: &Fragment) {
        let ack_packet = Packet {
            pack_type: PacketType::Ack(Ack {
                fragment_index: fragment.fragment_index,
            }),
            routing_header: SourceRoutingHeader {
                hop_index: 1, //---> start at the beginning of the reversed path
                hops: packet.routing_header.hops.iter().rev().copied().collect(),
            },
            session_id: packet.session_id,
        };
        if let Some(next_hop) = ack_packet.routing_header.hops.get(1){
            if let Some(sender) = self.packet_sender.get(&next_hop){
                match sender.try_send(ack_packet.clone()) {
                    Ok(()) => {
                        sender.send(ack_packet.clone()).unwrap();
                    }
                    Err(e)=>{
                        println!("Error sending packet: {:?}", e);
                    }
                }
            } else {
                println!("No sender found for {:?}", next_hop);
            }
        }

    }

    fn handle_flood_request(&mut self, session_id:u64, flood_request: &FloodRequest, source_routing_header: SourceRoutingHeader) {
        info!("Received Flood request for session {} with flood id {}", session_id, flood_request.flood_id);

        let flood_response= PacketType::FloodResponse(
            FloodResponse {
                flood_id: session_id,
                path_trace: vec![(self.id,NodeType::Server)],
            }
        );
        let packet_flood_response= Packet{
            routing_header: SourceRoutingHeader{
                hop_index: 0,
                hops: source_routing_header.hops.iter().rev().copied().collect(),
            },
            session_id,
            pack_type: flood_response,
        };
        self.packet_sender.iter().for_each(|(_, sender)| {
            sender.try_send(packet_flood_response.clone()).unwrap()
        })

    }

    fn handle_flood_response(&mut self, session_id: u64, flood_response: &FloodResponse, routing_header: SourceRoutingHeader) {
        info!("Received FloodResponse for flood_id {} in session {}", flood_response.flood_id, session_id);
        //obiettivo: fare tutta quella roba strana con il grafo.

        /*
        // Extract the path from the response
        let path = flood_response.path_trace
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<NodeId>>();

        info!("New path discovered: {:?}", path);
        //integrare tutti i collegamenti e nodi del path dentro il grado

        // TODO: Use this path for future communications with this client
        // You might want to store this path in a new field in your server struct*/
        let nodes: Vec<NodeId> = flood_response.path_trace.iter().map(|(id, _)| *id).collect();

        for pair in nodes.windows(2) {
            if let [a, b] = pair {
                self.network_graph.add_link(*a, *b);
            }
        }

    }

    fn send_chat_message(&self, session_id:u64, target_id: NodeId, msg: String, original_header:SourceRoutingHeader) {
        let data = msg.as_bytes();
        let mut fragment_data = [0u8; 128];
        let length = data.len().min(128);
        fragment_data[..length].copy_from_slice(&data[..length]);

        let fragment = Fragment {
            fragment_index: 0,
            total_n_fragments: 1,
            length: length as u8,
            data: fragment_data,
        };

        //find the best path from server to receiver client, using network_graph: NetworkGraph
        let source= self.id;
        let destination= target_id;
        let hops= match self.network_graph.best_path(source, target_id){

            Some(path) => {path}
            None => {
                error!("No path found from server {} to client {}", source, destination);
                return;
            }
        };



        let packet = Packet {
            session_id,
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops,
            },
            pack_type: PacketType::MsgFragment(fragment),
        };

        if let Some(sender) = self.packet_sender.get(&target_id) {
            if let Err(e) = sender.send(packet) {
                error!("Failed to send chat response to {}: {:?}", target_id, e);
            }
        } else {
            error!("No sender available for node {}", target_id);
        }
    }
    pub fn compute_best_path(&self, from: NodeId, to: NodeId) -> Option<Vec<NodeId>> {
        self.network_graph.best_path(from, to)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_graph_best_path() {
        println!("Creating new graph for test");
        let mut graph = NetworkGraph::new();

        // Build small network: Server(0) - Drone1(1) - Drone2(2) - Client(3)
        graph.add_link(0, 1);
        graph.add_link(1, 2);
        graph.add_link(2, 3);

        // Add alternative worse path: Server(0) - Drone4(4) - Client(3)
        graph.add_link(0, 4);
        graph.add_link(4, 3);

        // Simulate drop on 0-4 and 4-3 (bad path)
        graph.increment_drop(0, 4);
        graph.increment_drop(0, 4);
        graph.increment_drop(4, 3);
        println!("prova");
        // Now best path should be 0 -> 1 -> 2 -> 3
        let path = graph.best_path(0, 3).unwrap();
        println!("{:?}", path);
        assert_eq!(path, vec![0, 1, 2, 3]);
        graph.print_graph();
    }
    #[test]
    fn test_network_graph_best_path_2() {
        let mut graph = NetworkGraph::new();

        // Crea la rete:
        // 0 -> 1 -> 2 -> 3
        // 0 -> 4 -> 3
        // 0 -> 5 -> 6 -> 3
        graph.add_link(0, 1);
        graph.add_link(1, 2);
        graph.add_link(2, 3);

        graph.add_link(0, 4);
        graph.add_link(4, 3);

        graph.add_link(0, 5);
        graph.add_link(5, 6);
        graph.add_link(6, 3);

        // Simuliamo drop:
        // peggioriamo alcuni link
        graph.increment_drop(0, 1);
        graph.increment_drop(1, 2);
        graph.increment_drop(2, 3);

        graph.increment_drop(0, 4);
        graph.increment_drop(4, 3);
        graph.increment_drop(4, 3); // penalizza ancora di più 4->3

        graph.print_graph();

        // Non tocchiamo 5-6-3 ➔ Percorso alternativo migliore
        // Percorsi:
        // - 0-1-2-3: 3 drop
        // - 0-4-3: 3 drop
        // - 0-5-6-3: 0 drop (BEST PATH)

        let path = graph.best_path(0, 3).unwrap();
        println!("Path found: {:?}", path);

        assert_eq!(path, vec![0, 5, 6, 3]);

        // Verifica anche i pesi lungo il percorso
        let total_drop: usize = path.windows(2)
            .map(|pair| {
                let (a, b) = (pair[0], pair[1]);
                let a_idx = graph.node_indices.get(&a).unwrap();
                let b_idx = graph.node_indices.get(&b).unwrap();
                let edge = graph.graph.find_edge(*a_idx, *b_idx).unwrap();
                *graph.graph.edge_weight(edge).unwrap()
            })
            .sum();
        assert_eq!(total_drop, 0);
    }
    #[test]
    fn test_remove_node_from_graph() {
        let mut graph = NetworkGraph::new();

        // Setup:
        graph.add_link(1, 2);
        graph.add_link(2, 3);
        graph.add_link(3, 4);
        println!("Graph before removing node");
        graph.print_graph();
        //assert!(graph.node_indices.contains_key(&2));
        assert!(graph.best_path(1, 4).is_some());
        // Remove node 2
        graph.remove_node(2);
        // Verify node 2 is no longer in the graph
        assert!(!graph.node_indices.contains_key(&2));
        // Any path going through node 2 should now fail
        let path = graph.best_path(1, 4);
        println!("{:?}", path);
        let path2= graph.best_path(3,4) ;
        graph.print_graph();
        assert!(path.is_none(), "Expected no path from 1 to 4 after node 2 was removed");

    }


}
