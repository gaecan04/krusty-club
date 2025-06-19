//gaetano

use std::collections::{HashMap, HashSet, BinaryHeap};
use std::string::String;
use std::default::Default;
use std::{fs, io, thread};
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
use petgraph::visit::NodeIndexable;
use serde::de::StdError;
use serde::de::Unexpected::Str;
use std::cmp::Reverse;
use std::time::Duration;
use bincode::error::IntegerType::Usize;
use rand::random;
use crate::simulation_controller::gui_input_queue::{push_gui_message, new_gui_input_queue, SharedGuiInput};


//the first two global variable are kept to ensure consistency throughout the various chats
static FLOOD_IDS: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));
static SESSION_IDS: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));
//the chatting status is used to keep track of: chat activity, user we are chatting with, server we are connected to
static CHATTING_STATUS: Lazy<Mutex<(bool , NodeId , NodeId)>> = Lazy::new(|| Mutex::new((false , 0 , 0)));

#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_recv: Receiver<DroneCommand>,
    sent_messages: HashMap<u64, Vec<Fragment>>,//keeps track of the fragment we send for dropped fragments recovery
    net_graph: Graph<u8, u8, Undirected>,//graph to keep track of the network topology
    node_map: HashMap<NodeId , (NodeIndex , NodeType)>,//hashmap to know the types and ids of each node in the graph
    received_packets: HashMap<u64 , Vec<u8>>,//hashmap to store and reassemble fragments
    seen_flood_ids : HashSet<(u64 , NodeId)>,//hashset to keep track of the floods passing through the client
    simulation_log: Arc<Mutex<Vec<String>>>,

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
            simulation_log: Arc::new(Mutex::new(Vec::new())),

        }
    }
    pub (crate)fn run(&mut self, gui_input: SharedGuiInput) {
        loop {
            let gui_input = gui_input.clone();

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.inner_run(gui_input);
            }));

            match result {
                Ok(_) => {
                    info!("run() exited normally.");
                    break;
                }
                Err(e) => {
                    if let Some(s) = e.downcast_ref::<&str>() {
                        warn!("⚠ run() panicked: {}", s);
                    } else if let Some(s) = e.downcast_ref::<String>() {
                        warn!("⚠ run() panicked: {}", s);
                    } else {
                        warn!("⚠ run() panicked with unknown error.");
                    }
                    info!("Restarting run() after short delay...");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    }
    fn inner_run(&mut self, gui_input: SharedGuiInput) {
        info!("Client {} starting run loop", self.id);
        self.send_flood_request();
        loop {
            if let Ok(mut map) = gui_input.lock() {
                if let Some(msgs) = map.get_mut(&self.id) {
                    if !msgs.is_empty() {
                        let msg = msgs.remove(0);
                        drop(map); // Release lock early
                        match self.process_gui_command(msg) {
                            Ok(message) => {
                                if message != "NO_CHAT_COMMAND" {
                                    self.send_packet(message);
                                }
                            }
                            Err(e) => {
                                warn!("⚠ Error during process_gui_command: {:?}", e);
                            }
                        }
                    }
                }
            }

            select_biased! {
            recv(self.packet_recv) -> first_packet => {
                if let Ok(packet) = first_packet {
                    println!("♥♥ Packet received by client {} : {:?}", self.id, packet);
                    self.process_packet(packet);

                    while let Ok(packet) = self.packet_recv.try_recv() {
                        println!("♥♥ Packet received by client {} : {:?}", self.id, packet);
                        self.process_packet(packet);
                    }
                } else {
                    info!("Packet channel closed.");
                }
            },
            default => {
                thread::sleep(Duration::from_millis(1));
            }
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
                self.reassemble_packet(fragment , &mut (packet.clone()));
            },
            PacketType::Ack(_ack) => {
                //we just use it to know everything is working fine so no handling is needed
            },
            PacketType::Nack(nack ) => {
                self.process_nack(nack , &mut (packet.clone()));
            },
        }
    }

    fn process_flood_request(&mut self, request: &FloodRequest, header: SourceRoutingHeader){
        let mut updated_header = header.clone();
        updated_header.append_hop(self.id);
        updated_header.increase_hop_index();
        let mut updated_request = request.get_incremented(self.id , Client);
        if self.seen_flood_ids.contains(&(request.flood_id , request.initiator_id)) || self.packet_send.len() == 1{
            let mut response_packet = updated_request.generate_response(SESSION_IDS.lock().unwrap().clone());
            response_packet.routing_header.hop_index += 1;
            let response_sender = self.packet_send.get(&updated_request.path_trace[updated_request.path_trace.len()-2].0).unwrap();
            response_packet.clone().routing_header.hops;
            (*response_sender).send(response_packet).unwrap_or_default();
            info!("Successfully sent response packet to {:?} from {:?}, RResponse: {:?}" , request.initiator_id , self.id , updated_request.path_trace);
            self.increment_ids(&SESSION_IDS);
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
                    session_id:SESSION_IDS.lock().unwrap().clone(),
                };
                if Some(*neighbor_id) != sender_id{
                    sender.send(packet.clone()).unwrap_or_default();
                }
                if let Err(err) = sender.send(packet) {
                    println!("Error sending packet: {:?}", err);
                }

            }
            self.increment_ids(&SESSION_IDS);
        }
    }

    fn process_flood_response (&mut self, response: &FloodResponse) {
        //info!("Client {:?} is processing a FloodResponse {:?}", self.id , response.path_trace);
        let mut graph_copy = self.net_graph.clone();
        let mut map_copy = self.node_map.clone();
        for i in 0 .. response.path_trace.len()-1{
            let node1 = self.add_node_no_duplicate(&mut graph_copy, &mut map_copy, response.path_trace[i].clone().0 , response.path_trace[i].1);
            let node2 = self.add_node_no_duplicate(&mut graph_copy, &mut map_copy, response.path_trace[i+1].clone().0 , response.path_trace[i+1].1);
            self.add_edge_no_duplicate(&mut self.net_graph.clone(), node1, node2, 1);
        }
    }

    fn reassemble_packet(&mut self, fragment: &Fragment, packet: &mut Packet) {
        let session_id  = packet.session_id;
        let total_frags = fragment.total_n_fragments as usize;
        let frag_len    = fragment.length as usize;
        let slot_bytes  = 128;

        // Phase 1: borrow self to update buffer & detect completion
        // ---------------------------------------------------------
        let (need_ack, is_complete) = {
            // 1a) get-or-create the slot-capacity buffer
            let buf = self.received_packets
                .entry(session_id)
                .or_insert_with(|| vec![0u8; total_frags * slot_bytes]);
            if buf.len() != total_frags * slot_bytes {
                buf.resize(total_frags * slot_bytes, 0);
            }

            // 1b) write this fragment into its slot
            let offset = (fragment.fragment_index as usize) * slot_bytes;
            buf[offset .. offset + frag_len]
                .copy_from_slice(&fragment.data[..frag_len]);

            // 1c) we always want to ACK whatever fragment arrives
            let need_ack = true;

            // 1d) check completion by ensuring each slot has some non-zero
            let mut all_slots = true;
            for idx in 0..total_frags {
                let start = idx * slot_bytes;
                let len = if idx + 1 == total_frags { frag_len } else { slot_bytes };
                if buf[start .. start + len].iter().all(|&b| b == 0) {
                    all_slots = false;
                    break;
                }
            }

            (need_ack, all_slots)
        }; // <-- all borrows of self.received_packets are gone here

        // Phase 2: ACK that fragment
        // --------------------------
        if need_ack {
            self.send_ack(packet, fragment);
        }

        // Phase 3: if complete, remove & dispatch
        // ----------------------------------------
        if is_complete {
            // pluck out the fully‐assembled buffer
            let buf = self.received_packets.remove(&session_id).unwrap();

            // compute the true length from last fragment
            let full_len   = (total_frags - 1) * slot_bytes + frag_len;
            let message    = buf[..full_len].to_vec();

            self.packet_command_handling(message);
            info!("👻👻👻👻👻👻  Packet with session_id {} fully reassembled 👻👻👻👻👻👻", session_id);
        }
    }

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        match nack.nack_type {
            NackType::Dropped => {
                self.increase_cost(packet.routing_header.hops[0]); // to properly use our pathfinding algorithms the links between the drones are weighted based on the number of "Dropped" Nacks we receive from each drone
                if let Some(fragments) = self.sent_messages.get(&packet.session_id) {
                    for fragment in fragments {
                        if fragment.fragment_index == nack.fragment_index {
                            info!("Fragment found");
                            let mut new_packet = Packet {
                                routing_header: SourceRoutingHeader {
                                    hop_index: 1, // the hop index is initialized to 1 to stay consistent with the logic of the drones
                                    hops: self.best_path(self.id, (*CHATTING_STATUS.lock().unwrap()).2).unwrap(),
                                },
                                session_id: packet.session_id,
                                pack_type: MsgFragment(fragment.clone()),
                            };
                            let sender_id = &new_packet.routing_header.hops[1];
                            let sender = self.packet_send.get(sender_id).unwrap();
                            (*sender).send(new_packet.clone()).unwrap_or_default();
                            info!("resending packet:{:?}", new_packet.clone());
                        }
                    }
                }
            }
            NackType::ErrorInRouting(node_id) => {
                // since the error in routing most likely implies that a drone has crashed we remove the drone and its edges from the network, if the drone hasn't crashed we just add it again during the flooding
                self.node_map.remove(&node_id);
                self.remove_all_edges_with_node(node_id);
                self.send_flood_request();
            }
            _ => {
                // since the other possible Nacks that can be received presume some malfunctioning in the flooding or the drones themselves
                // we can't properly intervene on the graph, so we should try to flood as we see fit
                self.send_flood_request();
            }
        }
    }


    fn send_packet(&mut self, input : String){
        let bytes = input.trim_end();
        let chunks: Vec<Vec<u8>> = bytes.as_bytes().chunks(128).map(|chunk| chunk.to_vec()).collect();//we break down the message in smaller chunks
        for i in 0..chunks.len() {
            let mut data:[u8;128] = [0;128];
            for j in 0..chunks[i].len(){
                data[j] = chunks[i][j];
            };
            let fragment:Fragment = Fragment { // we create the fragments based on the chunks
                fragment_index: i as u64,
                total_n_fragments: chunks.len() as u64,
                length: chunks[i].len() as u8,
                data,
            };

            let packet = Packet{
                routing_header: SourceRoutingHeader{
                    hop_index : 1, // the hop index is initialized to 1 to stay consistent with the logic of the drones
                    hops : self.best_path(self.id , (*CHATTING_STATUS.lock().unwrap()).2).unwrap(),
                },
                session_id: SESSION_IDS.lock().unwrap().clone(),
                pack_type: MsgFragment(fragment.clone()),
            };
            println!("♥♥ BEST PATH IS : {:?}",packet.routing_header.hops);
            self.sent_messages.entry(packet.session_id).or_insert(Vec::new()).push(fragment.clone());


            if let Some(next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index){ //we find the channel associated with the right drone using the RoutingHeader
                if let Some(sender) = self.packet_send.get(&next_hop){
                    match sender.try_send(packet.clone()) {
                        Ok(()) => {
                            info!("Sending packet with message {:?} , path: {:?}" , data , packet.routing_header.hops);
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

    fn send_flood_request(&self) {
        println!("🦋🦋🦋Incrementing FLOOD_IDS...");
        self.increment_ids(&FLOOD_IDS);

        println!("🦋🦋🦋Building FloodRequest...");
        let flood_request = FloodRequest::initialize(FLOOD_IDS.lock().unwrap().clone(), self.id, Client);

        println!("🦋🦋🦋Building Packet...");
        let packet = Packet {
            pack_type: PacketType::FloodRequest(flood_request),
            session_id: SESSION_IDS.lock().unwrap().clone(),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: vec![self.id],
            }
        };

        println!("🦋🦋🦋 Sending to neighbors...");
        for sender in self.packet_send.values() {
            if let Err(e) = sender.send(packet.clone()) {
                println!("❌ Error sending FloodRequest: {:?}", e);
            }
        }
        info!("🦋🦋🦋Starting the flood n. {}", FLOOD_IDS.lock().unwrap());
        self.increment_ids(&SESSION_IDS);
    }



    fn send_ack (&mut self, packet: &mut Packet , fragment: &Fragment) {
        let ack = Ack {
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

    fn packet_command_handling(&self, message : Vec<u8>) {
        let chatting_status = *CHATTING_STATUS.lock().unwrap();
        let message_string = String::from_utf8_lossy(&message).to_string();
        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();
        match tokens.as_slice() {
            ["[LoginAck]" , _session]=>{
                info!("You successfully logged in!");
            },
            ["[MessageFrom]", client_id_str, msg]=>{
                let client_id: NodeId = client_id_str.parse().unwrap_or_default();

                self.change_chat_status(true, client_id , chatting_status.2);
                info!("Received message from client id {}. Message : {}", client_id , msg);
            },
            ["[ChatStart]", success]=>{
                if success.trim_end_matches('\0') == "true"{
                    info!("Chat started successfully");
                }
                else {
                    self.change_chat_status(false,0 , chatting_status.2);
                    info!("Chat start failed");
                }
            },
            ["[ClientListResponse]", client_list]=>{
                info!("Clients available for chat: {}" , client_list);
            },
            ["[HistoryResponse]", response]=>{
                info!("Most recent chat history with current client: {}" , response);
            },
            ["[MediaUploadAck]", media_name]=>{
                info!("The media {} has been uploaded", media_name);
            },
            ["[MediaListResponse]" , media_list]=>{
                info!("Here's a list of the media available for download: {}" , media_list);
            },
            ["[MediaDownloadResponse]","ERROR","NotFound"]=>{
                info!("The media could not be found.");
            },
            ["[MediaDownloadResponse]", media_name, base64_data]=>{
                let base64_data_clean = base64_data.trim_end_matches('\0');
                println!("🚀🚀🚀🚀🚀
                        ← client: {} bytes, prefix = {:?}",
                         base64_data.len(),
                         &base64_data[0..20]
                );
                if let Err(e) = Self::display_media( media_name , base64_data_clean) {
                    info!("Failed to display image: {}", e);
                }
            },
            ["[MediaBroadcastAck]", media_name, "Broadcasted"]=>{
                info!("{} successful broadcast",media_name);
            },
            _=>{
                warn!("Wrong message format. The message: {} , doesn't respect any known format", message_string);
            },
        }
    }

    fn process_gui_command(&mut self, command_string: String)->Result<String , Box<dyn std::error::Error>> {
        let chatting_status = *CHATTING_STATUS.lock().unwrap();
        println!("Client {} processing GUI command '{}'", self.id, command_string.clone());
        let tokens: Vec<&str> = command_string.trim().split("::").collect();
        match tokens.as_slice() {
            ["[Login]", server_id_str] => {
                let server_id: NodeId = match server_id_str.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        return Err(Box::new(e))
                    },
                };
                self.change_chat_status(false, 0 , server_id);
                info!("Sending login request to server: {}", chatting_status.2);
                self.log(format!("Login from client {}",self.id));
                Ok(command_string)
            },
            ["[Logout]"] => {
                if chatting_status.0 == true { //we make sure to not log out while in the middle of a chat
                    Err(Box::new(io::Error::new(io::ErrorKind::Interrupted, "You are still in a chat with another user. End the chat before logging out")))
                } else if chatting_status.2 != 0 {
                    self.log(format!("Logout from client {}",self.id));
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
                self.log(format!("Client {} is sending a message to client {}",self.id , client_id));
                info!("Sending message: {} to client {}", message_str, client_id);
                Ok(command_string)
            },
            ["[ChatRequest]", client_id] => {
                if (chatting_status.0 , chatting_status.1).eq(&(false, 0 )) { //when requesting a chat we need to make sure that we are not in the middle of chatting with someone else
                    self.log(format!("Client {} is requesting to chat with client: {}", self.id , client_id));
                    let peer_id: NodeId = match client_id.parse() {
                        Ok(id) => id,
                        Err(e) => {
                            return Err(Box::new(e))
                        },
                    };
                    self.change_chat_status(true , peer_id ,chatting_status.2);
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
                self.log(format!("Client {} is uploading media with name: {} , encoded as: {}", self.id, media_name, encoded_media));
                Ok(command_string)
            },
            ["[MediaDownloadRequest]", media_name] => {
                self.log(format!("Client {} is requesting to download media: {}", self.id, media_name));
                Ok(command_string)
            },
            ["[ChatFinish]" , _client_id] => {
                if chatting_status.0 == true {
                    self.log(format!("Client {} is trying to end current chat" , self.id));
                    self.change_chat_status(false , 0 , chatting_status.2);
                    Ok(command_string)
                } else {
                    Err(Box::new(io::Error::new(io::ErrorKind::Interrupted, "You are not chatting with any user.")))
                }
            },
            ["[MediaBroadcast]", media_name, encoded_media] => {
                self.log(format!("Client {} is broadcasting {} to all connected clients", self.id, media_name));
                if let Err(e) = Self::display_media( media_name , encoded_media) {
                    warn!("Failed to display image: {}", e);
                }
                Ok(command_string)
            },
            ["[MediaListRequest]"] => {
                info!("Requesting media list to server: {}" , chatting_status.2);
                Ok(command_string)
            },
            ["[FloodRequired]",action] => {
                info!("Starting a flood for {}", action);
                self.send_flood_request();
                Ok("NO_COMMAND".to_string())
            },
            _ => {
                println!("Unknown format");
                Err(Box::new(io::Error::new(io::ErrorKind::NotFound, "Unknown format")))
            },
        }
    }

    pub fn attach_log(&mut self, log: Arc<Mutex<Vec<String>>>) {
        self.simulation_log = log;
    }

    fn log(&self, message: impl ToString) {
        if let Ok(mut log) = self.simulation_log.lock() {
            log.push(message.to_string());
        }
    }

    fn add_node_no_duplicate(&mut self, graph: &mut Graph<u8, u8, Undirected>, node_map: &mut HashMap<NodeId, (NodeIndex, NodeType)>, value: u8 , node_type: NodeType) -> NodeIndex {
        if let Some(&idx) = node_map.get(&value) {
            // Node with this value already exists
            //info!("Node with ID: {:?} , is already present for {:?}" , value , self.id);
            idx.0
        } else {
            // Create new node
            //info!("Adding node: {:?} to {:?}" , value , self.id);
            let idx = graph.add_node(value);
            node_map.insert(value, (idx , node_type));
            self.node_map = node_map.clone();
            self.net_graph = graph.clone();
            info!("NODES => {:?}" , self.node_map);
            idx

        }
    }
    fn add_edge_no_duplicate(
        &mut self,
        graph: &mut Graph<u8, u8, Undirected>,
        a: NodeIndex,
        b: NodeIndex,
        weight: u8,
    ) -> bool {
        // Ensure both nodes exist in the graph
        let node_bound = graph.node_bound();
        if a.index() < node_bound && b.index() < node_bound {
            if !graph.contains_edge(a, b) {
                graph.add_edge(a, b, weight);
                self.net_graph = graph.clone();
                info!("Adding edge {:?} == {:?}", a, b);
                true
            } else {
                false
            }
        } else {
            // Optionally: print a warning
            info!(
                "Tried to add edge between invalid indices: a = {:?}, b = {:?}, graph.node_bound = {}",
                a,
                b,
                node_bound
            );
            false
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

    fn best_path(&self, source: NodeId, target: NodeId) -> Option<Vec<NodeId>> {
        use std::collections::{BinaryHeap, HashMap};
        use std::cmp::Reverse;

        // Handle edge cases
        if target == 0 {
            return None;
        }

        if source == target {
            return Some(vec![source]);
        }

        // Get node indices for source and target
        let source_idx = self.node_map.get(&source)?.0;
        let target_idx = self.node_map.get(&target)?.0;

        // Initialize distances and predecessors
        let mut distances: HashMap<NodeIndex, u32> = HashMap::new();
        let mut predecessors: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        let mut heap = BinaryHeap::new();

        // Set all distances to infinity except source
        for node_idx in self.net_graph.node_indices() {
            distances.insert(node_idx, u32::MAX);
        }
        distances.insert(source_idx, 0);
        heap.push(Reverse((0u32, source_idx)));

        // Dijkstra's algorithm
        while let Some(Reverse((current_distance, current_node))) = heap.pop() {
            // Skip if we've already processed this node with a better distance
            if current_distance > distances[&current_node] {
                continue;
            }

            // Stop early if we reached the target
            if current_node == target_idx {
                break;
            }

            // Process all neighbors
            for edge in self.net_graph.edges(current_node) {
                let neighbor_idx = edge.target();
                let edge_weight = *edge.weight() as u32;

                // Find the NodeId for this neighbor to check its type
                let neighbor_id = self.node_map.iter()
                    .find(|(_, &(idx, _))| idx == neighbor_idx)
                    .map(|(id, _)| *id);

                if let Some(neighbor_node_id) = neighbor_id {
                    // Check if this neighbor is a Server node
                    let is_server = if let Some(&(_, node_type)) = self.node_map.get(&neighbor_node_id) {
                        node_type == NodeType::Server
                    } else {
                        false
                    };

                    // Skip Server nodes unless they are our target
                    if is_server && neighbor_node_id != target {
                        continue;
                    }

                    let new_distance = current_distance.saturating_add(edge_weight);

                    if new_distance < distances[&neighbor_idx] {
                        distances.insert(neighbor_idx, new_distance);
                        predecessors.insert(neighbor_idx, current_node);
                        heap.push(Reverse((new_distance, neighbor_idx)));
                    }
                }
            }
        }

        // Check if target is reachable
        if distances.get(&target_idx) == Some(&u32::MAX) {
            return None;
        }

        // Reconstruct path from target back to source
        let mut path = Vec::new();
        let mut current = target_idx;

        loop {
            // Find NodeId for current NodeIndex
            let node_id = self.node_map.iter()
                .find(|(_, &(idx, _))| idx == current)
                .map(|(id, _)| *id)?;

            path.push(node_id);

            if current == source_idx {
                break;
            }

            current = *predecessors.get(&current)?;
        }

        // Reverse to get path from source to target
        path.reverse();

        Some(path)
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
            if !edge.weight().eq(&u8::MAX){
                self.net_graph.update_edge(edge.source() , edge.target() , edge.weight()+1);
                info!("{:?} new weight" , edge);
            }

        }
    }



    fn increment_ids( &self , counter: &Lazy<Mutex<u64>>) {
        let mut val = counter.lock().unwrap();
        *val += 1;
    }

    fn change_chat_status(&self,chatting: bool, peer_id : NodeId, server_id : NodeId) {
        let mut status = CHATTING_STATUS.lock().unwrap();
        (*status).0 = chatting;
        (*status).1 = peer_id;
        (*status).2 = server_id;
    }

    fn display_media(media_name: &str, base64_data: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Decode base64
        let decoded = match STANDARD.decode(base64_data) {
            Ok(d) => d,
            Err(e) => {
                warn!("Base64 decode error: {}", e);
                return Err(Box::new(e));
            }
        };

        // Determine file extension based on media type
        let file_extension = Self::detect_media_format(&decoded)?;
        let file_path = format!("{}.{}", media_name, file_extension);

        // Handle different media types
        match file_extension.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" => {
                Self::display_image(&decoded, &file_path)?;
            }
            "mp4" | "avi" | "mov" | "mkv" | "webm" => {
                Self::play_video(&decoded, &file_path)?;
            }
            "mp3" | "wav" | "flac" | "ogg" => {
                Self::play_audio(&decoded, &file_path)?;
            }
            _ => {
                // For unknown formats, just save and try to open
                fs::write(&file_path, &decoded)?;
                Self::open_with_system(&file_path)?;
            }
        }

        Ok(())
    }
    fn detect_media_format(data: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
        if data.len() < 12 {
            return Ok("bin".to_string());
        }

        // Check magic bytes for common formats
        match &data[0..4] {
            [0x89, 0x50, 0x4E, 0x47] => Ok("png".to_string()),
            [0xFF, 0xD8, 0xFF, _] => Ok("jpg".to_string()),
            [0x47, 0x49, 0x46, 0x38] => Ok("gif".to_string()),
            [0x42, 0x4D, _, _] => Ok("bmp".to_string()),
            _ => {
                // Check for other formats
                if data.len() >= 12 {
                    match &data[4..12] {
                        [0x66, 0x74, 0x79, 0x70, 0x69, 0x73, 0x6F, 0x6D] => Ok("mp4".to_string()),
                        [0x66, 0x74, 0x79, 0x70, 0x6D, 0x70, 0x34, 0x32] => Ok("mp4".to_string()),
                        _ => {
                            // Check for audio formats
                            if data.len() >= 3 && &data[0..3] == [0x49, 0x44, 0x33] {
                                Ok("mp3".to_string())
                            } else if data.len() >= 4 && &data[0..4] == [0x52, 0x49, 0x46, 0x46] {
                                Ok("wav".to_string())
                            } else if data.len() >= 4 && &data[0..4] == [0x66, 0x4C, 0x61, 0x43] {
                                Ok("flac".to_string())
                            } else {
                                Ok("bin".to_string())
                            }
                        }
                    }
                } else {
                    Ok("bin".to_string())
                }
            }
        }
    }

    fn display_image(decoded_data: &[u8], file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Read image from memory
        let cursor = Cursor::new(decoded_data);
        let img = match ImageReader::new(cursor).with_guessed_format()?.decode() {
            Ok(i) => i,
            Err(e) => {
                warn!("Image decode error: {}", e);
                return Err(Box::new(e));
            }
        };

        // Save image to file
        if let Err(e) = img.save(file_path) {
            warn!("Failed to save image: {}", e);
            return Err(Box::new(e));
        }

        // Display the image using system default application
        Self::open_with_system(file_path)?;

        Ok(())
    }

    fn play_video(decoded_data: &[u8], file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Save video file
        fs::write(file_path, decoded_data)?;

        // Open with system default video player
        Self::open_with_system(file_path)?;

        Ok(())
    }

    fn play_audio(decoded_data: &[u8], file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Save audio file
        fs::write(file_path, decoded_data)?;

        // Open with system default audio player
        Self::open_with_system(file_path)?;

        Ok(())
    }

    fn open_with_system(file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", file_path])
                .spawn()?;
        }

        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open")
                .arg(file_path)
                .spawn()?;
        }

        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open")
                .arg(file_path)
                .spawn()?;
        }

        Ok(())
    }
}