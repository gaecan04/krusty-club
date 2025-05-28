//gaetano

use std::collections::{HashMap, HashSet};
use std::string::String;
use std::default::Default;
use std::io;
use std::io::Write;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NodeType, NackType,  Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;
use crossbeam_channel::{select, select_biased, Receiver, Sender};
use log::{info, warn};
use petgraph::graph::{Graph, NodeIndex, UnGraph};
use petgraph::algo::dijkstra;
use petgraph::data::Build;
use petgraph::prelude::EdgeRef;
use petgraph::Undirected;
use wg_2024::packet::NodeType::{Client, Server};
use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::sync::Arc;
use base64::{decode, Engine};
use base64::engine::general_purpose::STANDARD;
use std::io::Cursor;
use image::{ImageReader, DynamicImage, open};
use serde::de::StdError;
use serde::de::Unexpected::Str;
use crate::simulation_controller::gui_input_queue::{push_gui_message, new_gui_input_queue, SharedGuiInput};


//the two global variable are kept to ensure consistency throughout the various chats
static FLOOD_IDS: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));
static SESSION_IDS: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));
static CHATTING_STATUS: Lazy<Mutex<(bool , NodeId)>> = Lazy::new(|| Mutex::new((false , 0)));

#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_recv: Receiver<DroneCommand>,
    pub sent_messages: HashMap<u64, Vec<Fragment>>,
    pub net_graph: Graph<u8, u8, Undirected>,
    pub node_map: HashMap<NodeId , (NodeIndex , NodeType)>,
    pub received_packets: HashMap<u64 , Vec<u8>>,
    pub seen_flood_ids : HashSet<(u64 , NodeId)>,
    pub received_messages : HashMap<u64 , Vec<u8>>,
    pub available_servers: HashSet<(NodeId, bool)>,
}

impl MyClient {
    pub(crate) fn new(id: NodeId, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>) -> Self {
        Self {
            id,
            sim_contr_recv: crossbeam_channel::never(),
            packet_recv,
            packet_send,
            sent_messages: HashMap::new(),
            net_graph: Graph::<u8, u8, Undirected>::new_undirected(),
            node_map: HashMap::new(),
            received_packets: HashMap::new(),
            seen_flood_ids: HashSet::new(),
            received_messages: HashMap::new(),
            available_servers: HashSet::new(),
        }
    }

    pub (crate) fn run(&mut self, gui_input: SharedGuiInput) {

        println!("Client {} starting run loop", self.id);
        self.send_flood_request();
        loop {
            if let Ok(mut map) = gui_input.lock() {

                if let Some(msgs) = map.get_mut(&self.id) {
                    if !msgs.is_empty() {

                        let msg = msgs.remove(0);
                        drop(map); // Release lock early
                        let message = self.process_gui_command(msg).unwrap();
                        self.send_packet(message);
                    }
                }
            }
            select_biased! {
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet by client {}...",self.id);
                    if let Ok(packet) = packet {
                        info!("Packet received by drone {} : {:?}",packet.routing_header.hops[packet.routing_header.hop_index], packet);
                        self.process_packet(packet);
                    } else {
                        info!("No packet received or channel closed.");
                    }
                },
                default => {
                   std::thread::sleep(std::time::Duration::from_millis(1));
                 }
                /*recv(self.sim_contr_recv) -> msg => {
                    info!("Checking for received command...");
                    if let Ok(msg) = msg {
                        info!("Command received: {:?}", msg);
                        self.process_controller_command(msg);
                    } else {
                        info!("No command received or channel closed.");
                    }
                }*/
            }
        }
    }
    /*
    fn process_controller_command(&mut self, msg: DroneCommand) {
        match &mut msg.clone(){
            DroneCommand::RemoveSender(drone_id)=>{
                self.packet_send.remove(drone_id);
                info!("Removed sender packet for {}", drone_id);
            }
            DroneCommand::AddSender(drone_id, sender)=>{
                self.packet_send.insert(*drone_id , sender.clone());
                info!("Added sender packet for {}", drone_id);
            }
            _=>{
                warn!("The command is not recognized by the client")
            }
        }
    }*/

    fn process_packet (&mut self, packet: Packet) {
        match &mut packet.clone().pack_type {
            PacketType::FloodRequest(request) => {
                self.process_flood_request(request, packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                self.process_flood_response(response);
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut (packet.clone()) , fragment);
                self.reassemble_packet(fragment , &mut (packet.clone()));
            },
            PacketType::Ack(_ack) => {
                //we just use it to know everything is working fine so no handling is needed
            },
            PacketType::Nack(nack ) => {
                self.process_nack(nack , &mut (packet.clone()));
            }
        }
    }

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        match nack.nack_type {
            NackType::Dropped=>{
                self.increase_cost(packet.routing_header.hops[0]); // to properly use our pathfinding algorithms the links between the drones are weighted based on the number of "Dropped" Nacks we receive from each drone
                if let Some(fragments) = self.sent_messages.get(&packet.session_id){
                    for  fragment in fragments{
                        if fragment.fragment_index == nack.fragment_index{
                            let sender_id = &packet.routing_header.hops[packet.routing_header.hops.len()-2];
                            let sender = self.packet_send.get(sender_id).unwrap();
                            let mut new_packet = packet.clone();
                            new_packet.routing_header.hops = self.best_path(self.id , self.get_server_id().unwrap_or(0)).unwrap(); // since the packet has already been dropped on the previous route we try to find out if there is a better route
                            (*sender).send(packet.clone()).unwrap_or_default();
                        }
                    }
                }
            },
            NackType::ErrorInRouting(node_id)=>{ // since the error in routing most likely implies that a drone has crashed we remove the drone and its edges from the network, if the drone hasn't crashed we just add it again during the flooding
                self.node_map.remove(&node_id);
                self.remove_all_edges_with_node(node_id);
                self.send_flood_request();
            }
            _ => {
                // since the other possible Nacks that can be received presume some malfunctioning in the flooding or the drones themselves
                //we can't properly intervene on the graph, so we should try to flood as we see fit
                self.send_flood_request();
            }
        }
    }

    /*
    pub fn pop_all_gui_messages(&self, queue: &SharedGuiInput, client_id: NodeId){
            if let Ok(mut map) = queue.lock() {
                println!("Client {} is looking for messages. Available keys: {:?}", client_id, map.keys());
                if let Some(msgs) = map.get_mut(&client_id) {
                    if !msgs.is_empty() {
                        let msg = msgs.remove(0); // only ONE message per loop
                        drop(map); // release lock early
                        println!("Client {} popped: {}", client_id, msg);
                        match msg {
                            Ok(msg) => {
                                self.send_packet(msg);
                            }
                            Err(e)=>{}
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
    }
     */



    fn send_packet(&self, input : String){
        let bytes = input.trim_end();
        let chunks: Vec<Vec<u8>> = bytes.as_bytes().chunks(128).map(|chunk| chunk.to_vec()).collect();//we break down the message in smaller chunks
        for i in 0..chunks.len() {
            let mut data:[u8;128] = [0;128];
            for j in 0..chunks[i].len(){
                data[i] = chunks[i][j];
            };
            let fragment:Fragment = Fragment { // we create the fragments based on the chunks
                fragment_index: i as u64,
                total_n_fragments: chunks.len() as u64,
                length: chunks[i].len() as u8,
                data,
            };

            let packet = Packet{ // we encapsulate the fragment in the packet to be sent through the threads
                routing_header: SourceRoutingHeader{
                    hop_index : 1, // the hop index is initialized to 1 to stay consistent with the logic of the drones
                    hops : self.best_path(self.id , self.get_server_id().unwrap_or(0)).unwrap_or_default(),
                },
                session_id: SESSION_IDS.lock().unwrap().clone(),
                pack_type: MsgFragment(fragment),
            };
            if let Some(next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index){ //we find the channel associated with the right drone using the RoutingHeader
                if let Some(sender) = self.packet_send.get(&next_hop){
                    match sender.try_send(packet.clone()) {
                        Ok(()) => {
                                info!("Sending packet with message {:?} , path: {:?}" , data , packet.routing_header.hops);
                            sender.send(packet.clone()).unwrap_or_default();
                        }
                        Err(e)=>{
                            info!("Error sending packet: {:?}", e);
                        }
                    }
                }
            }
        }
        self.increment_ids(&SESSION_IDS)
    }

    fn send_flood_request(&self){
        self.increment_ids(&FLOOD_IDS);
        let flood_request  = FloodRequest :: initialize(FLOOD_IDS.lock().unwrap().clone(), self.id, Client);
        let packet = Packet {
            pack_type: PacketType::FloodRequest(flood_request),
            session_id: 18446744073709551615, // a session_id is reserved for the flood_requests
            routing_header: SourceRoutingHeader{
                hop_index: 0,
                hops: [self.id].to_vec(),
            }
        };
        for  sender_tuple in self.packet_send.iter() { // we go through all the neighbours of the Client, and we send a FloodRequest to each of them
            let sender = sender_tuple.1.clone();
            sender.send(packet.clone()).unwrap_or_default();
        }
        info!("Starting the flood n. {}", FLOOD_IDS.lock().unwrap());
    }

    fn reassemble_packet (&mut self, fragment: &Fragment, packet : &mut Packet){
        let session_id = packet.session_id; // since each message sent has its own, that is the same for each of its fragments, session_id we use them to store
        if let Some(content)= self.received_packets.get_mut(&session_id){
            for i in 0..fragment.data.len() {
                content.insert( (fragment.fragment_index*128+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*128{
                self.send_ack(packet, fragment);
            }
        }
        else {
            let mut content = vec![];
            for i in 0..fragment.data.len() {
                content.insert( (fragment.fragment_index*128+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*128{
                self.send_ack(packet, fragment);
                self.packet_command_handling(content.clone());
            }
            self.received_packets.insert(session_id ,content.clone());
            info!("Packet with session_id {} fully received", session_id);
        }

    }

    fn packet_command_handling(&self, message : Vec<u8>) {
        let message_string = String::from_utf8_lossy(&message).to_string();
        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();
        match tokens.as_slice() {
            ["LoginAck" , _session]=>{
                info!("You successfully logged in!");
            }
            ["[MessageFrom]", client_id, msg]=>{
                info!("Received message from client id {}. Message : {}", client_id , msg);
            }
            ["[ChatStart]", success]=>{
                if success.to_string() == "true"{
                    info!("Chat started successfully");
                }
                else {
                    self.change_chat_status(0);
                    info!("Chat start failed");
                }
            }
            ["[ClientListResponse]", client_list]=>{
                info!("Clients available for chat: {}" , client_list);
            }
            ["[HistoryResponse]", response]=>{
                info!("Most recent chat history with current client: {}" , response);
            }
            ["MediaUploadAck", media_name]=>{
                info!("The media {} has been uploaded", media_name);
            }
            ["MediaListResponse" , media_list]=>{
                info!("Here's a list of the media available for download: {}" , media_list);
            }
            ["[MediaDownloadResponse]","ERROR","NotFound"]=>{
                info!("The media could not be found.");
            }
            ["[MediaDownloadResponse]", media_name, base64_data]=>{
                if let Err(e) = Self::display_image(base64_data, media_name) {
                    info!("Failed to display image: {}", e);
                }
            }
            _=>{
                warn!("Wrong message format. The message: {} , doesn't respect any known format", message_string);
            }
        }
    }


    fn process_flood_request(&mut self, request: &FloodRequest, header: SourceRoutingHeader){
        let mut updated_header = header.clone();
        updated_header.append_hop(self.id);
        updated_header.increase_hop_index();
        let mut updated_request = request.clone();
        if self.seen_flood_ids.contains(&(request.flood_id , request.initiator_id)) || self.packet_send.len() == 1{
            updated_request.path_trace.push((self.id , Client) );
            //each time a FloodRequest arrives and its flood_id and initiator_id tuple matches one present in our seen_flood_ids we create a FloodResponse based on the request
            let response_packet = request.generate_response(0);
            let response_sender = self.packet_send.get(&updated_request.path_trace[updated_request.path_trace.len()-2].0).unwrap();
            (*response_sender).send(response_packet).unwrap_or_default();
            println!("Successfully sent response packet");
        }
        else {
            self.seen_flood_ids.insert((request.flood_id , request.initiator_id));
            updated_request.path_trace.push((self.id , Client));
            let sender_id = if updated_request.path_trace.len() > 1 {
                Some(updated_request.path_trace[updated_request.path_trace.len() - 2].0)
            } else {
                None
            };

            //keeping the flood going
            for (neighbor_id, sender) in self.packet_send.iter() {
                let packet = Packet{
                    pack_type: PacketType::FloodRequest(FloodRequest{
                        flood_id: updated_request.flood_id,
                        initiator_id:updated_request.path_trace[0].0.clone(),
                        path_trace: updated_request.path_trace.clone(),
                    }),
                    routing_header: updated_header.clone(),
                    session_id:0,
                };
                if Some(*neighbor_id) != sender_id{
                    sender.send(packet.clone()).unwrap_or_default();
                }
                if let Err(err) = sender.send(packet) {
                    println!("Error sending packet: {:?}", err);
                }

            }
        }
    }

    fn send_ack (&mut self, packet: &mut Packet , fragment: &Fragment) {
        let ack= Ack {
            fragment_index : fragment.fragment_index,
        };
        let ack_packet = Packet {
            pack_type: PacketType::Ack(ack),
            routing_header: packet.routing_header.clone(),
            session_id: packet.session_id,
        };
        if let Some(next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index){
            if let Some(sender) = self.packet_send.get(&next_hop){
                match sender.try_send(ack_packet.clone()) {
                    Ok(()) => {
                        sender.send(ack_packet.clone()).unwrap_or_default();
                    }
                    Err(e)=>{
                        println!("Error sending packet: {:?}", e);
                    }
                }
            }
        }
    }

    fn process_flood_response (&mut self, response: &FloodResponse) {
        let mut graph_copy = self.net_graph.clone();
        let mut map_copy = self.node_map.clone();
        for i in 0 .. response.path_trace.len()-1{
            let node1 = self.add_node_no_duplicate(&mut graph_copy, &mut map_copy, response.path_trace[i].clone().0 , response.path_trace[i].1);
            let node2 = self.add_node_no_duplicate(&mut graph_copy, &mut map_copy, response.path_trace[i+1].clone().0 , response.path_trace[i+1].1);

            self.add_edge_no_duplicate(&mut self.net_graph.clone(), node1, node2, 1);
        }
        for i in 0 .. response.path_trace.len(){
            if response.path_trace[i].1 == Server {
                if !self.available_servers.contains(&(response.path_trace[i].0, true)) && !self.available_servers.contains(&(response.path_trace[i].0, false)) {
                    self.available_servers.insert((response.path_trace[i].0, false));
                }
            }
        }

        info!("Updating known network topology.")
    }
    fn add_edge_no_duplicate(&mut self, graph: &mut Graph<u8, u8, Undirected>, a: NodeIndex, b: NodeIndex, weight: u8) -> bool {
        if !graph.contains_edge(a, b) {
            graph.add_edge(a, b, weight);
            true
        } else {
            false
        }
    }
    fn add_node_no_duplicate(&mut self, graph: &mut Graph<u8, u8, Undirected>, node_map: &mut HashMap<NodeId, (NodeIndex, NodeType)>, value: u8 , node_type: NodeType) -> NodeIndex {
        if let Some(&idx) = node_map.get(&value) {
            // Node with this value already exists
            idx.0
        } else {
            // Create new node
            let idx = graph.add_node(value);
            node_map.insert(value, (idx , node_type));
            idx
        }
    }

    fn remove_all_edges_with_node(&mut self, crash_id: NodeId) {
        let index = *(self.node_map.get(&crash_id).unwrap());
        let edges_to_remove: Vec<_> = self.net_graph.edges(index.0).filter_map(|edge_ref| {
            // Get the source and target of the edge
            let source = edge_ref.source();
            let target = edge_ref.target();

            // If either endpoint matches our target node, keep this edge
            if source == index.0 || target == index.0 {
                Some(edge_ref.id())
            } else {
                None
            }
        }).collect();

        for edge_id in edges_to_remove {
            self.net_graph.remove_edge(edge_id);
        }
    }

    fn increase_cost(&mut self , dropper_id: NodeId) {
        let index = *(self.node_map.get(&dropper_id).unwrap());
        let graph_copy = self.net_graph.clone();
        let edges_to_increase: Vec<_> = graph_copy.edges(index.0).filter_map(|edge_ref| {
            // Get the source and target of the edge
            let source = edge_ref.source();
            let target = edge_ref.target();

            // If either endpoint matches our target node, keep this edge
            if source == index.0 || target == index.0 {
                Some(edge_ref)
            } else {
                None
            }
        }).collect();

        for edge in edges_to_increase {
            self.net_graph.update_edge(edge.source() , edge.target() , edge.weight()+1);
        }
    }

    fn best_path(&self, source: NodeId, target: NodeId) -> Option<Vec<NodeId>> {
        let source_idx = *self.node_map.get(&source)?;
        let target_idx = *self.node_map.get(&target)?;

        // Dijkstra: I take note of where I come from (predecessor)
        let mut predecessors: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        let _ = dijkstra(&self.net_graph, source_idx.0, Some(target_idx.0), |e| {
            let from = e.source();
            let to = e.target();
            // I keep track of each node I can reach
            predecessors.entry(to).or_insert(from);
            *e.weight()
        });

        // If I fail to find my target I exit
        if !predecessors.contains_key(&target_idx.0) {
            warn!("No path found");
            return None;
        }

        // I recreate the path using the predecessors
        let mut path = vec![self.net_graph[target_idx.0]];
        let mut current = target_idx.0;
        while current != source_idx.0 {
            if let Some(&prev) = predecessors.get(&current) {
                path.push(self.net_graph[prev]);
                current = prev;
            } else {
                warn!("No path found");
                return None;
            }
        }

        path.reverse();
        Some(path)
    }
    fn get_server_id(&self) -> Option<NodeId> {
        let mut server_value= (NodeIndex::new(0) , Server);
        //I look for the tuple of the server based on the knowledge that it is of NodeType::Server
        while let Some(indexes) = self.node_map.values().next(){
            if indexes.1 == Server {
                server_value = *indexes;
            }
        }
        //Once I hve found the Server tuple I find the associated NodeId
        let found_key = self.node_map.iter()
            .find(|(_, v)| (*v).eq(&server_value))
            .map(|(server_id, _)| server_id);
        let found_server = self.available_servers.get(&(*(found_key?), true));

        match found_server {
            Some(k) =>{
                info!("Found server {} with status {}", k.0, k.1);
                Some(*(found_key?))},
            None=>{
                None
            },
        }
    }

    fn increment_ids( &self , counter: &Lazy<Mutex<u64>>) {
        let mut val = counter.lock().unwrap();
        *val += 1;
    }

    fn change_chat_status(&self, peer_id : NodeId){
        let mut status = CHATTING_STATUS.lock().unwrap();
        (*status).0 = !(*status).0;
        (*status).1 = peer_id;
    }

    fn display_image(base64_data: &str, media_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Decode base64
        let decoded = match STANDARD.decode( base64_data) {
            Ok(d) => d,
            Err(e) => {
                info!("Base64 decode error: {}", e);
                return Err(Box::new(e));
            }
        };

        // Read image from memory
        let cursor = Cursor::new(decoded.clone());
        let img = match ImageReader::new(cursor).with_guessed_format()?.decode() {
            Ok(i) => i,
            Err(e) => {
                info!("Image decode error: {}", e);
                return Err(Box::new(e));
            }
        };

        // Save image to file
        let image_path = format!("{}.png", media_name);
        if let Err(e) = img.save(&image_path) {
            info!("Failed to save image: {}", e);
            return Err(Box::new(e));
        }

        // Display the image in a window (non-blocking, auto closes when user exits)

        match open(image_path){
            Ok(display)=>{display}
            Err(e) => {
                info!("Failed to display: {}", e);
                return Err(Box::new(e));
            }
        };

        Ok(())
    }

    fn process_gui_command(&mut self, command_string: String)->Result<String , Box<dyn std::error::Error>> {
        println!("Client {} processing GUI command '{}'", self.id, command_string);

        let tokens: Vec<&str> = command_string.trim().split("::").collect();
        match tokens.as_slice() {
            ["[Login]", server_id_str] => { // when logging in to a server we change its connection status from false to true
                let server_id: u64 = (*server_id_str).parse().expect("Failed to parse u64");
                self.available_servers.remove(&(server_id as NodeId, false));
                self.available_servers.insert((server_id as NodeId, true));
                info!("Login to {}", server_id_str);
                Ok(command_string)
            },
            ["[Logout]"] => {
                if (*CHATTING_STATUS.lock().unwrap()).0 == true { //we make sure to not log out while in the middle of a chat
                    Err(Box::new(io::Error::new(io::ErrorKind::Interrupted, "You are still in a chat with another user. End the chat before logging out")))
                } else if let Some(to_disconnect) = self.available_servers.iter().find(|(_, connected)| *connected == true).cloned() {
                    let mut new_status = self.available_servers.take(&to_disconnect).unwrap();
                    new_status.1 = false; //since we are connected to a server we can actually log out and change the status of the server back to false
                    self.available_servers.insert(new_status);
                    info!("Logout from {:#?}", self.get_server_id());
                    Ok(command_string)
                } else { //if we are yet to log in to any server we can log out of it
                    Err(Box::new(io::Error::new(io::ErrorKind::NotFound, "You have yet to login to any server")))
                }
            },
            ["[ClientListRequest]"] => {
                info!("Requesting the list of clients available for chat");
                Ok(command_string)
            },
            ["[MessageTo]", client_id, message_str] => {
                info!("Sending message: {} to client {}", message_str, client_id);
                Ok(command_string)
            },
            ["[ChatRequest]", client_id] => {
                if (*CHATTING_STATUS.lock().unwrap()).eq(&(false, 0)) { //when requesting a chat we need to make sure that we are not in the middle of chatting with someone else
                    info!("Requesting to chat with client: {}", client_id);
                    let peer_id: NodeId = (*client_id).parse().expect("Failed to parse u64");
                    self.change_chat_status(peer_id);
                    Ok(command_string)
                } else {
                    Err(Box::new(io::Error::new(io::ErrorKind::Interrupted, "You are already in a chat with another user.")))
                }
            },
            ["[HistoryRequest]", personal_id, peer_id] => {
                info!("Requesting chat history between client {} and client {}", personal_id, peer_id);
                Ok(command_string)
            },
            ["[MediaUpload]", media_name, encoded_media] => {
                info!("Uploading media with name: {} , encoded as: {}", media_name, encoded_media);
                Ok(command_string)
            },
            ["[MediaDownloadRequest]", media_name] => {
                info!("Requesting to download media: {}", media_name);
                Ok(command_string)
            },
            ["[ChatFinish]"] => {
                if (*CHATTING_STATUS.lock().unwrap()).0 == true {
                    info!("Requesting to end current chat");
                    self.change_chat_status(0);
                    Ok(command_string)
                } else {
                    Err(Box::new(io::Error::new(io::ErrorKind::Interrupted, "You are not chatting with any user.")))
                }
            },
            ["[MediaBroadcast]"] => {
                info!("Broadcasting media to all connected clients");
                Ok(command_string)
            },
            ["[MediaListRequest]"] => {
                info!("Requesting media list to server: {}" , self.get_server_id().unwrap_or(0));
                Ok(command_string)
            },
            ["[FloodRequired]",action] => {
                Ok(command_string)
            },
            _ => {
                info!("Unknown format");
                Err(Box::new(io::Error::new(io::ErrorKind::NotFound, "Unknown format")))
            },
        }
    }
}




/*use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use crossbeam_channel::{Sender, Receiver};
use std::thread;
use rand::Rng;
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Packet, PacketType, FloodRequest, FloodResponse, Fragment};
use wg_2024::packet::NodeType;
use crate::simulation_controller::gui_input_queue::{pop_all_gui_messages, SharedGuiInput};


pub fn start_client(
    client_id: NodeId,
    packet_rx: Receiver<Packet>,
    packet_senders: HashMap<NodeId, Sender<Packet>>,
    gui_input: SharedGuiInput,
) {
    thread::spawn(move || {
        println!("Temp Client {} started", client_id);
        println!("🧠 Arc ptr of client: {:p}", Arc::as_ptr(&gui_input));


                // Flooding (unchanged)
                let flood_id = rand::thread_rng().gen::<u64>();
                let flood_request = Packet {
                    session_id: flood_id,
                    routing_header: Default::default(),
                    pack_type: PacketType::FloodRequest(FloodRequest {
                        flood_id,
                        initiator_id: client_id,
                        path_trace: vec![(client_id, NodeType::Client)],
                    }),
                };

                for (_, sender) in &packet_senders {
                    let _ = sender.send(flood_request.clone());
                }

                let mut responses = HashSet::new();
                let timeout = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while std::time::Instant::now() < timeout {
                    if let Ok(packet) = packet_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                        if let PacketType::FloodResponse(resp) = packet.pack_type {
                            if responses.insert(resp.flood_id) {
                                println!("Client {} got response: {:?}", client_id, resp.path_trace);
                            }
                        }
                    }
                }

                println!("Client {} finished flood test", client_id);

        // 🆕 Poll and send one message every second
        loop {
            if let Ok(mut map) = gui_input.lock() {
                if let Some(msgs) = map.get_mut(&client_id) {
                    if !msgs.is_empty() {
                        let msg = msgs.remove(0); // only ONE message per loop
                        drop(map); // release lock early

                        // Send to all servers
                        for (&server_id, sender) in &packet_senders {
                            let raw_bytes = msg.as_bytes();
                            let max_len = 128;
                            let total_frags = ((raw_bytes.len() + max_len - 1) / max_len) as u64;
                            let session_id = rand::random();

                            for (i, chunk) in raw_bytes.chunks(max_len).enumerate() {
                                let mut buf = [0u8; 128];
                                buf[..chunk.len()].copy_from_slice(chunk);

                                let fragment = Fragment {
                                    fragment_index: i as u64,
                                    total_n_fragments: total_frags,
                                    length: chunk.len() as u8,
                                    data: buf,
                                };

                                let packet = Packet {
                                    session_id,
                                    routing_header: SourceRoutingHeader::with_first_hop(vec![client_id, server_id]),
                                    pack_type: PacketType::MsgFragment(fragment),
                                };

                                let _ = sender.send(packet);
                            }
                        }
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_secs(1));
        }

    });
}



let messages = pop_all_gui_messages(&self.gui_input, self.id);
for msg in messages {
    let fragment = make_fragment(&msg); // crea i fragment
    let packet = Packet {
        session_id: 0,
        routing_header: SourceRoutingHeader {
            hop_index: 0,
            hops: vec![self.id, server_id], // dinamico se necessario
        },
        pack_type: PacketType::MsgFragment(fragment),
    };
    if let Some(sender) = self.packet_send.get(&server_id) {
        let _ = sender.send(packet);
    }
}



use std::collections::{HashMap, HashSet};
use std::io;
use std::io::Write;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;
use crossbeam_channel::{select, Receiver, Sender};
use petgraph::graph::{Graph, NodeIndex, UnGraph};
use petgraph::algo::dijkstra;
use petgraph::Undirected;

#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_recv: Receiver<DroneCommand>,
    pub sent_messages: HashMap<u64, Vec<Fragment>>,
}

impl MyClient{
    fn new(id: NodeId, sim_contr_recv: Receiver<DroneCommand>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, sent_messages:HashMap<u64 , Vec<Fragment>>) -> Self {
        Self {
            id,
            sim_contr_recv,
            packet_recv,
            packet_send,
            sent_messages,
        }
    }

    fn run (&mut self){
        let mut seen_flood_ids = HashSet::new();
        let mut received_messages = HashMap::new();
        let mut graph = UnGraph::<i32, f32>::new_undirected();
        let mut node_map: HashMap<u8, NodeIndex> = HashMap::new();
        //aggiungere anche quello dei pacchetti mandati volendo oppure mettere il pacchetto che si sta mandando in una variabile e impedire di mandare altro fino a che non ha finito
        loop{
            select! {
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet...");
                    if let Ok(packet) = packet {
                        println!("Packet received by drone {} : {:?}",self.id, packet);
                        let mut message = vec![];
                        self.process_packet(packet, &mut seen_flood_ids, &mut received_messages, message);
                    } else {
                        println!("No packet received or channel closed.");
                    }
                },
            }
        }
    }

    fn process_packet (&mut self, mut packet: Packet, seen_flood_ids: &mut HashSet<u64>, received_packets: &mut HashMap<u64 , Vec<u8>>, message: Vec<MsgFragment()>) {

        match &packet.pack_type {
            PacketType::FloodRequest(request) => {
                self.process_flood_request(request, seen_flood_ids , packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                self.create_topology(response);
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut packet , fragment);
                self.reassemble_packet(fragment , received_packets, &mut packet);
            },
            PacketType::Ack(ack) => {
                //ok.
            },
            PacketType::Nack(nack ) => {
                self.process_nack(nack , &mut packet);
            }
        }
    }


    i messaggi che vengono mandati vengono divisi in un vettore di frammenti associato al suo id e conservati in un hashmap
    quando recuperiamo il messaggio cerchiamo l'id e troviamo il fragment index

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        match nack.nack_type {
            NackType::Dropped=>{
                if let Some(fragments) = self.sent_messages.get(&packet.session_id){
                    for  fragment in fragments{
                        if fragment.fragment_index == nack.fragment_index{
                            self.send(fragment);
                        }
                    }
                }
            },
            _ => {
                let new_hops :Vec<NodeId> = Vec::new(); // da fare con djikstra
                packet.routing_header.hops = new_hops;
                packet.routing_header.hop_index = 0;
                self.send(packet);
            }
        }
    }

    fn send_packet(&self){
        print!("Enter message type: ");
        io::stdout().flush().unwrap();
        let mut message_type = String::new();
        io::stdin().read_line(&mut message_type).expect("Failed to read line");
        let mut input = String::new();
        io::stdin().read_line(&mut input).expect("Failed to read line");
        let bytes = input.trim_end().as_bytes().to_vec();
        let chunks: Vec<Vec<u8>> = bytes.chunks(127).map(|chunk| chunk.to_vec()).collect();
        for i in 0..chunks.len()-1 {
            let mut fragment:Fragment = Fragment {
                fragment_index: i as u64,
                total_n_fragments: chunks.len() as u64,
                length: chunks[i].len() as u8,
                data,
            };
            fragment.data[1 .. 127] = *chunks[i];
            fragment.data[0] = message_type.as_bytes()[0];
            let  packet = Packet{
                routing_header: Default::default(), //Djikstra
                session_id: 0,
                pack_type: MsgFragment(fragment),
            };
            if let Some(next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index){
                if let Some(sender) = self.packet_send.get(&next_hop){
                    match sender.try_send(packet.clone()) {
                        Ok(()) => {
                            sender.send(packet.clone()).unwrap();
                        }
                        Err(e)=>{
                            println!("Error sending packet: {:?}", e);
                        }
                    }
                }
            }
        }

    }

    fn reassemble_packet (&mut self, fragment: &Fragment, received_packets: &mut HashMap<u64 , Vec<u8>>, packet : &mut Packet){
        let session_id = packet.session_id;
        if let Some(mut content)= received_packets.get(&session_id){
            content.len()+= fragment.data.len();
            for i in 1..fragment.data.len() {
                content.insert( (fragment.fragment_index*127+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*127{
                self.send_ack(packet, fragment);
            }
        }
        else {
            let mut content = vec![];
            content.len()+= fragment.data.len();
            for i in 1..fragment.data.len() {
                content.insert( (fragment.fragment_index*127+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*127{
                self.send_ack(packet, fragment);
            }
            received_packets.insert(session_id ,content.clone());
        }

    }

    fn process_flood_request(&mut self, request: &FloodRequest, seen_flood_ids: &mut HashSet<u64>, header: SourceRoutingHeader){
        let mut updated_request = request.clone();
        if (seen_flood_ids.contains(&request.flood_id) || self.packet_send.len() == 1){
            updated_request.path_trace.push((self.id , NodeType::Client));
            //send flood response
        }
        else {
            seen_flood_ids.insert(request.flood_id);
            updated_request.path_trace.push((self.id, NodeType::Client));
            let sender_id = if updated_request.path_trace.len() > 1 {
                Some(updated_request.path_trace[updated_request.path_trace.len() - 2].0)
            } else {
                None
            };

            for (neighbor_id, sender) in self.packet_send.iter() {
                if Some(*neighbor_id) != sender_id{
                    updated_request.path_trace.push((*neighbor_id, NodeType::Drone));
                    let updated_path_trace = updated_request.path_trace.iter().map(|(id, )| *id).collect::<Vec<>>();
                    updated_request.path_trace.pop();
                    let packet = Packet{
                        pack_type: PacketType::FloodRequest(FloodRequest{
                            flood_id: updated_request.flood_id,
                            initiator_id:updated_request.path_trace[0].0.clone(),
                            path_trace: updated_request.path_trace.clone(),
                        }),
                        routing_header: header.clone(), // to fix
                        session_id:0,
                    };
                    if let Err(err) = sender.send(packet) {
                        println!("Error sending packet: {:?}", err);
                    }
                }
            }
        }
    }

    fn send_ack (&mut self, packet: &mut Packet , fragment: &Fragment) {
        let ack= Ack {
            fragment_index : fragment.fragment_index,
        };
        let ack_packet = Packet {
            pack_type: PacketType::Ack(ack),
            routing_header: packet.routing_header.clone(),
            session_id: packet.session_id,
        };
        if let Some(next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index){
            if let Some(sender) = self.packet_send.get(&next_hop){
                match sender.try_send(ack_packet.clone()) {
                    Ok(()) => {
                        sender.send(ack_packet.clone()).unwrap();
                    }
                    Err(e)=>{
                        println!("Error sending packet: {:?}", e);
                    }
                }
            }
        }
    }
    fn add_edge_no_duplicate<N, E>(
        graph: &mut UnGraph<N, E>,
        a: NodeIndex,
        b: NodeIndex,
        weight: f64
    ) -> bool {
        if !graph.contains_edge(a, b) {
            graph.add_edge(a, b, weight);
            true
        } else {
            false
        }
    }
    fn add_node_no_duplicate<N: std::cmp::Eq + std::hash::Hash + Copy, E>(
        graph: &mut UnGraph<N, E>,
        node_map: &mut HashMap<(u8 , NodeType), NodeIndex>,
        value: (u8 , NodeType)
    ) -> NodeIndex {
        if let Some(&idx) = node_map.get(&value) {
            // Node with this value already exists
            idx
        } else {
            // Create new node
            let idx = graph.add_node(value);
            node_map.insert(value, idx);
            idx
        }
    }
    fn process_flood_response (&mut self, response: &FloodResponse, mut graph: &mut Graph<u8, u8, Undirected>, mut node_map: HashMap<(u8 , NodeType), NodeIndex>) {
        for i in 0 .. response.path_trace.len()-1{
            let node1 = Self::add_node_no_duplicate(&mut graph, &mut node_map, response.path_trace[i].clone());
            let node2 = Self::add_node_no_duplicate(&mut graph, &mut node_map, response.path_trace[i+1].clone());
            Self::add_edge_no_duplicate(graph , node1 , node2 , 1.0);
        }
    }
}
 */