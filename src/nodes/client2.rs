use std::collections::{HashMap, HashSet, VecDeque};
use std::string::String;
use std::default::Default;
use std::io;
use std::io::Write;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NodeType, NackType,  Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;
use crossbeam_channel::{select, Receiver, Sender};
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
use serde::de::Unexpected::Str;
//use crate::simulation_controller::gui_input_queue::{push_gui_message, new_gui_input_queue, SharedGuiInput};


//the two global variable are kept to ensure consistency throughout the various chats
static FLOOD_IDS: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));
static SESSION_IDS: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));

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
    //pub gui_input : SharedGuiInput,
    pub avaible_servers: HashSet<(NodeId , bool)>,
}

impl MyClient{
    fn new(id: NodeId,  packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>,/*gui_input : SharedGuiInput */ ) -> Self {
        Self {
            id,
            sim_contr_recv: crossbeam_channel::never(),
            packet_recv,
            packet_send,
            sent_messages: HashMap::new(),
            net_graph : Graph::<u8, u8, Undirected>::new_undirected(),
            node_map: HashMap::new(),
            received_packets: HashMap::new(),
            seen_flood_ids: HashSet::new(),
            received_messages: HashMap::new(),
            //gui_input,
            avaible_servers : HashSet::new(),
        }
    }

    fn run (&mut self){
        loop{
            select! {
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet...");
                    if let Ok(packet) = packet {
                        println!("Packet received by drone {} : {:?}",packet.routing_header.hops[packet.routing_header.hop_index], packet);
                        self.process_packet(packet);
                    } else {
                        println!("No packet received or channel closed.");
                    }
                },
            }
        }
    }


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
                self.increase_cost(packet.routing_header.hops[0]); // to properly use our pathfinding algorithms the links between the drones are weighted based on the number of "Dropped" nacks we receive from each drone
                if let Some(fragments) = self.sent_messages.get(&packet.session_id){
                    for  fragment in fragments{
                        if fragment.fragment_index == nack.fragment_index{
                            let sender_id = &packet.routing_header.hops[packet.routing_header.hops.len()-2];
                            let sender = self.packet_send.get(sender_id).unwrap();
                            let mut new_packet = packet.clone();
                            new_packet.routing_header.hops = self.best_path(self.id , self.get_server_id()).unwrap(); // since the packet has already been dropped on the previous route we try to find out if there is a better route
                            (*sender).send(packet.clone()).unwrap();
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
                // since the other possible nacks that can be received presume some malfunctioning in the flooding or the dornes themselves
                //we can't properly intervene on the graph, so we should try to flood as we see fit
                self.send_flood_request();
            }
        }
    }

/*
    pub fn pop_all_gui_messages(queue: &SharedGuiInput, client_id: NodeId) -> Vec<(NodeId,String)> {
        if let Ok(mut map) = queue.lock() {
            map.remove(&client_id).unwrap_or_default()
        } else {
            vec![]
        }
    }
*/

    fn send_packet(&self){
        //let messages = self.pop_all_gui_messages(&self.gui_input, self.id); // we get the input from the common buffer
        //for msg in messages{
        //let input = msg
        let input = String::new();
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
                    hops : self.best_path(self.id , self.get_server_id()).unwrap(),
                },
                session_id: SESSION_IDS.lock().unwrap().clone(),
                pack_type: MsgFragment(fragment),
            };
            if let Some(next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index){ //we find the channel associated with the right drone using the RoutingHeader
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
        Self::increment_ids(&SESSION_IDS)
        //}
    }

    fn send_flood_request(&mut self){
        Self::increment_ids(&FLOOD_IDS);
        let flood_request  = FloodRequest :: initialize(FLOOD_IDS.lock().unwrap().clone(), self.id, Client);
        let packet = Packet {
            pack_type: PacketType::FloodRequest(flood_request),
            session_id: 18446744073709551615,
            routing_header: SourceRoutingHeader{
                hop_index: 0,
                hops: [self.id].to_vec(),
            }
        };
        for  sender_tuple in self.packet_send.iter() { // we go through all the neighbours of the Client, and we send a FloodRequest to each of them
            let sender = sender_tuple.1.clone();
            sender.send(packet.clone()).unwrap();
        }
        println!("Starting the flood n. {}", FLOOD_IDS.lock().unwrap());
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
            println!("Packet with session_id {} fully received", session_id);
        }

    }

    fn packet_command_handling(&mut self, message : Vec<u8>) {
        let message_string = String::from_utf8_lossy(&message).to_string();
        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();
        match tokens.as_slice() {
            ["[MessageFrom]", client_id, msg]=>{
                println!("Received message from client id {}. Message : {}", client_id , msg);
            }
            ["[ChatStart]", success]=>{
                if success.to_string() == "true"{
                    println!("Chat started successfully");
                }
                else {
                    println!("Chat start failed");
                }
            }
            ["[ClientListResponse]", clients]=>{
                println!("Clients aviable for chat: {}" , clients);
            }
            ["[HistoryResponse]", response]=>{
                println!("Most recent chat history with current client: {}" , response);
            }
            ["MediaUploadAck", media_name]=>{
                println!("The media {} has been uploaded", media_name);
            }
            ["[MediaDownloadResponse]","ERROR","NotFound"]=>{
                println!("The media could not be found.");
            }
            ["[MediaDownloadResponse]", media_name, base64_data]=>{
                if let Err(e) = Self::display_image(base64_data, media_name) {
                    eprintln!("Failed to display image: {}", e);
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
            let  response_sender = self.packet_send.get(&updated_request.initiator_id).unwrap();
            (*response_sender).send(response_packet).unwrap();
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
                    sender.send(packet.clone()).unwrap();
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
                        sender.send(ack_packet.clone()).unwrap();
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
                if !self.avaible_servers.contains(&(response.path_trace[i].0, true)) && !self.avaible_servers.contains(&(response.path_trace[i].0 , false)) {
                    self.avaible_servers.insert((response.path_trace[i].0, false));
                }
            }
        }

        println!("Updating known network topology.")
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
                return None;
            }
        }

        path.reverse();
        Some(path)
    }
    fn get_server_id(&self) -> NodeId {
        let mut server_value= (NodeIndex::new(0) , NodeType::Server);
        //I look for the tuple of the server based on the knowledge that it is of NodeType::Server
        while let Some(indexes) = self.node_map.values().next(){
            if indexes.1 == NodeType::Server {
                server_value = *indexes;
            }
        }
        //Once I hve found the Server tuple I find the associated NodeId
        let found_key = self.node_map.iter()
            .find(|(_, v)| (*v).eq(&server_value))
            .map(|(server_id, _)| server_id);
        let found_server = self.avaible_servers.get(&(*(found_key.unwrap()) , true));

        match found_server {
            Some(k) =>{
                println!("Found server {} with status {}", k.0, k.1);
                *(found_key.unwrap())},
            None =>0,
        }
    }

    fn increment_ids( counter: &Lazy<Mutex<u64>>) {
        let mut val = counter.lock().unwrap();
        *val += 1;
    }

    fn display_image(base64_data: &str, media_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Decode base64
        let decoded = match STANDARD.decode( base64_data) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Base64 decode error: {}", e);
                return Err(Box::new(e));
            }
        };

        // Read image from memory
        let cursor = Cursor::new(decoded.clone());
        let img = match ImageReader::new(cursor).with_guessed_format()?.decode() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("Image decode error: {}", e);
                return Err(Box::new(e));
            }
        };

        // Save image to file
        let image_path = format!("{}.png", media_name);
        if let Err(e) = img.save(&image_path) {
            eprintln!("Failed to save image: {}", e);
            return Err(Box::new(e));
        }

        // Display the image in a window (non-blocking, auto closes when user exits)

            match open(image_path){
                Ok((display))=>{display}
                Err(e) => {
                    eprintln!("Failed to display: {}", e);
                    return Err(Box::new(e));
                }
            };

        Ok(())
    }

    fn process_gui_command(&mut self, command_string: String) {
        let tokens: Vec<&str> = command_string.trim().split("::").collect();
        match tokens.as_slice() {
            ["[Login]" , server_id_str] => { // when logging in to a server we change its connection status from false to true
                let server_id: u64 = (*server_id_str).parse().expect("Failed to parse u64");
                self.avaible_servers.remove(&(server_id as NodeId, false));
                self.avaible_servers.insert((server_id as NodeId, true));
            },
            ["[Logout]"] => {// the connection status goes back to false once the logout message is sent
                if let Some(to_disconnect) = self.avaible_servers.iter().find(|(_, connected)| *connected == true).cloned() {
                    let mut new_status = self.avaible_servers.take(&to_disconnect).unwrap();
                    new_status.1 = false;
                    self.avaible_servers.insert(new_status);
                }
            },
            _=>{
                println!("No specific action needed");
            }
        }
    }
}

