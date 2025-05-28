/*
COSE DA FARE :
- le varie funzioni di interazione con i client: con i vari codici da mettere nei frammenti
- implementare sto grafo dimmerda
- fare simulazioni

*/

use petgraph::algo::dijkstra;
use std::collections::HashMap;
use std::collections::HashSet;
use std::mem::transmute;
use crossbeam_channel::{Receiver, RecvError, Sender};
use eframe::egui::accesskit::Node;
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use log::{info, error, warn, debug};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::VecDeque;
use crate::simulation_controller::gui_input_queue::SharedGuiInput;


#[derive(Clone, Debug)]
pub struct NetworkGraph {
    graph: DiGraph<NodeId, usize>,
    node_indices: HashMap<NodeId, NodeIndex>,
    node_types: HashMap<NodeId, NodeType>,

}

impl NetworkGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            node_indices: HashMap::new(),
            node_types: HashMap::new(),
        }
    }
    pub fn set_node_type(&mut self, node_id: NodeId, node_type: NodeType) { //method to set NodeType
        self.node_types.insert(node_id, node_type);
    }
    pub fn get_node_type(&self, node_id: NodeId) -> Option<&NodeType> {
        self.node_types.get(&node_id)
    }
    pub fn add_node(&mut self, id: NodeId, node_type: NodeType) -> NodeIndex {
        if let Some(&index) = self.node_indices.get(&id) {
            self.node_types.insert(id, node_type); // this is to update the hashmap with nodetype associated to nodeId
            index
        } else {
            let index = self.graph.add_node(id);
            self.node_indices.insert(id, index);
            self.node_types.insert(id, node_type);
            index
        }
    }
    pub fn remove_node(&mut self, node_id: NodeId) {
        if let Some(node_index) = self.node_indices.remove(&node_id) {
            self.graph.remove_node(node_index);
            self.node_types.remove(&node_id);
        } else {
            warn!("Tried to remove non-existing node {}", node_id);
        }
    }

    pub fn add_link(&mut self, a: NodeId, node_type_a: NodeType, b: NodeId, node_type_b: NodeType) {
        let a_idx = self.add_node(a, node_type_a);
        let b_idx = self.add_node(b, node_type_b);
        self.graph.update_edge(a_idx, b_idx, 1);
        self.graph.update_edge(b_idx, a_idx, 1);
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
            let source_type = self.node_types.get(&source).map(|t| format!("{:?}", t)).unwrap_or("Unknown".to_string());
            let target_type = self.node_types.get(&target).map(|t| format!("{:?}", t)).unwrap_or("Unknown".to_string());

            println!(
                "{} ({}) <-> {} ({}) with {} drops",
                source, source_type, target, target_type, weight
            );
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

    seen_floods:HashSet<(u64, NodeId)>,
    registered_clients: Vec<NodeId>,
    network_graph: NetworkGraph,
    chat_history: HashMap<(NodeId,NodeId), VecDeque<String>>,
    media_storage: HashMap<String, (NodeId,String)>, // media name --> (uploader_id, associated base64 encoding as String)

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
            seen_floods: HashSet::new(),
            registered_clients: Vec::new(),
            network_graph: NetworkGraph::new(),
            chat_history:HashMap::new(),
            media_storage: HashMap::new(),
        }
    }

    /// Run the server to process incoming packets and handle fragment assembly
    pub fn run(&mut self, gui_buffer_input:SharedGuiInput) {
        // Initialize the logger

        /*if let Err(e) = env_logger::builder().try_init() {
            eprintln!("Failed to initialize logger: {}", e);
        }*/

        info!("Server {} started running.", self.id);
        loop {
            //check if the server is receiving MediaBroadcast commands from the gui
            if let Ok(mut buffer )= gui_buffer_input.lock(){ //block the arc mutex
                if let Some(messages) = buffer.get_mut(&(self.id as NodeId)) { //if i have pending messages i take them
                    if !messages.is_empty() {
                        let message = messages.remove(0); // I pop the first message until the buffer is empty
                        println!("🧹 Server {} popped one msg from GUI", self.id);
                        drop(buffer); // ✅ release lock early
                        if let Some(stripped) = message.strip_prefix("[MediaBroadcast]::") {
                            info!("Server received a MediaBroadcast request");
                            let parts: Vec<&str> = stripped.splitn(2, "::").collect();
                            if parts.len() == 2 {
                                let media_name = parts[0].to_string();
                                let base64_data = parts[1].to_string();

                                self.media_storage.insert(media_name.clone(), (self.id, base64_data.clone()));
                                for &target_id in &self.registered_clients {
                                    let forward = format!("[MediaDownloadResponse]::{}::{}", media_name, base64_data);
                                    self.send_chat_message(0, target_id, forward, SourceRoutingHeader::empty_route());
                                }
                                info!("Broadcasted media '{}' from GUI for server {}", media_name, self.id);
                            }
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1)); // we process one msg per second
            //first checking if I have pending messages from GUI, then treat everything from the receiver channel
            match self.packet_receiver.recv() {
                Ok(mut packet) => {
                    match &packet.pack_type {
                        PacketType::MsgFragment(fragment) => {
                            info!("Server received fragment");
                            self.send_ack(&packet, &fragment);
                            self.handle_fragment(packet.session_id, fragment, packet.routing_header);
                        }
                        PacketType::Nack(nack) => {
                            info!("Server received NACK");
                            self.handle_nack(packet.session_id, nack, packet.routing_header);
                        }
                        PacketType::Ack(ack) => {
                            // Process ACKs, needed for simulation controller to know when to print the message
                            //self.handle_ack(packet.session_id, ack, packet.routing_header); --> no need to do anything
                        }
                        PacketType::FloodRequest(flood_request) => {
                            info!("Server received FloodRequest");
                            // Process flood requests from clients trying to discover the network
                            // Server should send back a flood response and forward the FloodRequest to its neighbors except the one sending it
                            self.handle_flood_request(packet.session_id, flood_request, packet.routing_header);
                        }
                        PacketType::FloodResponse(flood_response) => {
                            info!("Server received FloodResponse");
                            // Process flood responses containing network information --> used to modify the configuration of the netwrok graph.
                            self.handle_flood_response(packet.session_id, flood_response, packet.routing_header);
                        }
                        _ => {
                            warn!("Server {} received unexpected packet type.", self.id);
                        }
                    }
                }
                //server is non receiving any packet
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

        // Check if the fragment is already received --> if already received return and do nothing
        if entry[fragment.fragment_index as usize].is_some() { //check if the option in the vec is Some(...)
            warn!("Duplicate fragment {} received for session {:?}", fragment.fragment_index, key);
            return;
        }
        //if not received yet --> Store the fragment data
        entry[fragment.fragment_index as usize] = Some(fragment.data);
        // Update the last fragment's length if applicable
        if fragment.fragment_index == fragment.total_n_fragments - 1 { // in the case we are receiving the last fragment we have not to consider the max length but rather the fragment length itself.
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
        //now trasform the message to string.
        let message_string = String::from_utf8_lossy(&message).to_string();
        let session_id: u64 = key.0;
        let client_id = key.1;

        //create a format to handle the
        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();

        match tokens.as_slice() {

            ["[Login]", server_id_str] => {
                if server_id_str.parse::<NodeId>() == Ok(self.id) {
                    if !self.registered_clients.contains(&client_id) {
                        self.registered_clients.push(client_id);
                        info!("Client {} registered to this server", client_id);

                        let login_acknowledgement= format!("[LoginAck]::{}", session_id);
                        self.send_chat_message(session_id, client_id,login_acknowledgement, routing_header);
                    }
                } else {
                    error!("server_id in Login request is not the id of the server receiving the fragment!")
                }

            }
            ["[ClientListRequest]"] => {
                info!("Server received ClientListRequest");
                if let Some(sender) = self.packet_sender.get(&client_id) {
                    let clients = self.registered_clients.clone();
                    let response = format!("[ClientListResponse]::{:?}", clients);
                    self.send_chat_message(session_id, client_id, response, routing_header);
                }
            }
            ["[ChatRequest]", target_id_str] => {
                info!("Server received ChatRequest");
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    let success = self.registered_clients.contains(&target_id);
                    let response = format!("[ChatStart]::{}", success);
                    self.send_chat_message(session_id, client_id, response, routing_header);
                }
            }
            ["[MessageTo]", target_id_str, msg] => {
                info!("Server received MessageTo");
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    if self.registered_clients.contains(&target_id) {
                        let response = format!("[MessageFrom]::{}::{}", client_id, msg);
                        self.send_chat_message(session_id, target_id, response, routing_header);

                        let entry = self
                            .chat_history
                            .entry((client_id.min(target_id), client_id.max(target_id)))
                            .or_insert_with(VecDeque::new);
                        let chat_entry = format!("{}: {}", client_id, msg);
                        entry.push_back(chat_entry);

                        if entry.len() > MAX_CHAT_HISTORY {
                            entry.pop_front();
                        }
                    } else {
                        self.send_chat_message(session_id, client_id, "error_wrong_client_id!".to_string(), routing_header);
                    }
                }
            }
            ["[HistoryRequest]", source_id, target_id_str,] => { //when client wants to see chronology
                info!("Server received HistoryRequest");
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    let client_1 = source_id.parse::<NodeId>().unwrap_or(client_id);
                    let client_2 = target_id;
                    let key = (client_1.min(client_2), client_1.max(client_2));
                    let history = self.chat_history.get(&key);
                    let response = match history {
                        Some(messages) => messages.iter().cloned().collect::<Vec<String>>().join("\n"),
                        None => "No history available".to_string(),
                    };
                    self.send_chat_message(session_id, client_id, format!("[HistoryResponse]::{}", response), routing_header);
                }
            }
            //chat_history: HashMap<(NodeId,NodeId), VecDeque<String>>,
            ["[ChatHistoryUpdate]", source_server, serialized_entry] => {
                info!("Server received ChatHistoryUpdate");
                if let Ok(((id1, id2), history)) = serde_json::from_str::<((NodeId, NodeId), VecDeque<String>)>(serialized_entry) {
                    self.chat_history.insert((id1, id2), history);
                    info!("Received and saved full chat history entry from server {}", source_server);
                } else {
                    error!("Failed to parse full chat history entry from {}", source_server);
                }
            }

            ["[MediaUpload]", image_info] => {
                info!("Server received MediaUpload");
                let parts: Vec<&str> = image_info.splitn(2, "::").collect();
                if parts.len() == 2 {
                    let media_name = parts[0].to_string();
                    let base64_data = parts[1].to_string();
                    // Save the image media in the hashmap
                    self.media_storage.insert(media_name.clone(), (client_id, base64_data));
                    let confirm = format!("[MediaUploadAck]::{}", media_name);
                    self.send_chat_message(session_id, client_id, confirm, routing_header);
                }
            }
            //Providing Media list if asked by client --> so they can get to know before what to download
            ["[MediaListRequest]"] => {
                info!("Server received MediaListRequest");
                let list = self.media_storage.keys()
                    .cloned()
                    .collect::<Vec<String>>()
                    .join(",");
                let response = format!("[MediaListResponse]::{}", list);
                self.send_chat_message(session_id, client_id, response, routing_header);
            }
            ["[MediaDownloadRequest]", media_name] => {
                info!("Server received MediaDownloadRequest");
                let response = if let Some((owner,base64_data)) = self.media_storage.get(*media_name) {
                    format!("[MediaDownloadResponse]::{}::{}", media_name, base64_data)
                } else {
                    "[MediaDownloadResponse]::ERROR::NotFound".to_string()
                };
                self.send_chat_message(session_id, client_id, response, routing_header);
            }
            // da chi lo ricevo??? La richiesta dovrebbe mandarmela un client. Oppure il simulationController dalla GUI???
            //MEDIABROADCAST --> sending to all registered clients
            ["[MediaBroadcast]", media_name, base64_data] => {
                info!("Server received MediaBroadcast command");
                self.media_storage.insert(media_name.to_string(), (client_id, base64_data.to_string()));
                for &target_id in &self.registered_clients {
                    // Avoid sending to the sender
                    if target_id != client_id {
                        let msg = format!("[MediaDownloadResponse]::{}::{}", media_name, base64_data);
                        self.send_chat_message(session_id, target_id, msg.clone(), routing_header.clone());
                    }
                }
                // Confirm broadcast to the sender
                let ack = format!("[MediaBroadcastAck]::{}::Broadcasted", media_name);
                self.send_chat_message(session_id, client_id, ack, routing_header);
            }
            ["[ChatFinish]", target_client_str] => {
                info!("Client {} finished chat in session {}", client_id, session_id);
                //get the chat history for the session --> only one for each client.
                // Find the exact chat history (could be based on two clients)
                let target_client_id= target_client_str.parse::<NodeId>().unwrap();
                let entry = self.chat_history.iter()
                    .find(|((a, b), _)| *a == client_id || *b == target_client_id); // optionally refine match

                if let Some(((id1, id2), history)) = entry {
                    let full_entry = ((*id1, *id2), history.clone());
                    if let Ok(serialized) = serde_json::to_string(&full_entry) {
                        for (&node_id, _ ) in self.network_graph.node_types.iter() {
                            if self.network_graph.get_node_type(node_id) == Some(&NodeType::Server) && node_id != self.id {
                                if let Some(route) = self.compute_best_path(self.id, node_id) {
                                    let routing_header = SourceRoutingHeader::with_first_hop(route);
                                    let msg = format!("[ChatHistoryUpdate]::{}::{}", self.id, serialized);
                                    self.send_chat_message(session_id, node_id, msg, routing_header);
                                    info!("Server sent ChatHistoryUpdate");
                                }
                            }
                        }
                    }
                }
                //update the chat history on the other servers.
            }
            ["[Logout]"] => {
                self.registered_clients.retain(|&id| id != client_id);
                info!("Client {} logged out from session {}", client_id, session_id);
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
                /*
                    Una volta costruito il grafo ad ogni nack::Dropped che ricevo vado a modificare il peso di tutti i
                    collegamenti relativi a quel drone. Gli altri nack implicano che c'è qualche problema del tipo crashed Drone.
                    Come gestisco? Mando floodRequest, riceverò floodResponse che mi indicheranno il drone problematico, carpita
                    questa informazione lo vado a rimuovere dal grafo.
                 */
                //quando ricevo un nack::ErrorInRouting ... faccio un flooding normale (creo una flood_request normale),
                //poi riceverò flood_response e agisco sul grafo come al solito.

                // grafo fatto con pet_graph.
                warn!("Server {}: Received Nack::Dropped", self.id);
                //ricevo nack::Dropped --> modifico il costo nel grafo
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

                let node_type = self.network_graph.node_types.get(&crashed_node_id);

                match node_type {
                    // ????? review protocol nack behaviour
                    Some(NodeType:: Server) => {
                        warn!("Attempted to remove Server node {} — action skipped.", crashed_node_id);
                    }
                    Some(NodeType:: Client) => {
                        warn!("Attempted to remove client node {} — action skipped.", crashed_node_id);
                    }
                    Some(drone_type) => {
                        warn!("Detected crashed {:?} node {} due to ErrorInRouting.", drone_type, crashed_node_id);
                        // REMOVE the crashed node from the network graph
                        self.network_graph.remove_node(crashed_node_id);
                        info!("Updated graph after removing {:?}", crashed_node_id);
                        self.network_graph.print_graph();
                    }
                    None => {
                        warn!("Node type for {} not found — skipping removal.", crashed_node_id);
                    }
                }

            },

            _ => {
                    warn!("Received DestinationIsDrone/UnexpectedRecipient NACK type, sending flood request");
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
        //info!("Received Flood request for session {} with flood id {}", session_id, flood_request.flood_id);
        info!("Received FloodRequest from {} with ID {}", flood_request.initiator_id, flood_request.flood_id);

        let flood_req= (flood_request.flood_id, flood_request.initiator_id);
        //we have to check if it has already been received --> check in seen_floods HashSet
        //if it contains the flood_req:
        if self.seen_floods.contains(&flood_req){
            let flood_response= flood_request.generate_response(session_id);
            if let Some(sender)= self.packet_sender.get(&flood_response.routing_header.hops[1]){
                sender.send(flood_response).unwrap_or_else(|e| { error!("Failed to send flood_response: {:?}", e);});
            }
            return
        }
        //if it doesn't contain it --> we got to insert the flood_req in the HashSet
        self.seen_floods.insert(flood_req);
        //we then forward it to all neighbors except to the one who sent it to server
        let mut forwarded_request = flood_request.clone();
        forwarded_request.path_trace.push((self.id, NodeType::Server)); //we push as last element in the path_trace the server.

        for (&vicino, sender_channel) in &self.packet_sender {
            // Avoid sending back to the one we received it from
            if let Some(&(last_visited, _)) = flood_request.path_trace.last() {
                //checking the last element of flood_request (note that it is different from forwarded_request , which will be forwarded by the server itself
                if vicino == last_visited {
                    continue;
                }
            }
            //create the new flood_request packet from forwarded_request.
            let packet = Packet::new_flood_request(
                SourceRoutingHeader::empty_route(),
                session_id,
                forwarded_request.clone(),
            );
            if let Err(errore) = sender_channel.send(packet) {
                error!("Failed to forward FloodRequest to {}: {}", vicino, errore);
            }
        }
    }

    fn handle_flood_response(&mut self, session_id: u64, flood_response: &FloodResponse, routing_header: SourceRoutingHeader) {
        info!("Received FloodResponse for flood_id {} in session {}", flood_response.flood_id, session_id);
        //obiettivo: fare tutta quella roba strana con il grafo.
        //
        // let nodes: Vec<NodeId> = flood_response.path_trace.iter().map(|(id, _)| *id).collect();
        let path = &flood_response.path_trace;
        for pair in path.windows(2) {
            if let [(a_id, a_type), (b_id, b_type)] = pair {
                self.network_graph.add_link(*a_id, *a_type, *b_id, *b_type);
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
/*
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
*/