//gaetano

use std::collections::{HashMap, HashSet, BinaryHeap};
use std::string::String;
use std::default::Default;
use std::{fs, io, thread};
use std::io::{ErrorKind, Write};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NodeType, NackType,  Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;
use crossbeam_channel::{select, select_biased, Receiver, Sender};
use log::{error, info, warn};
use petgraph::graph::{Graph, NodeIndex, UnGraph};
use petgraph::algo::dijkstra;
use petgraph::data::Build;
use petgraph::prelude::EdgeRef;
use petgraph::Undirected;
use wg_2024::packet::NodeType::{Client,Server};
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
use std::fs::OpenOptions;
use std::path::Path;
use std::time::Duration;
use bincode::error::IntegerType::Usize;
use rand::random;
use crate::simulation_controller::gui_input_queue::{push_gui_message, new_gui_input_queue, SharedGuiInput};
use std::process::{Command, exit};




//the first two global variable are kept to ensure consistency throughout the various chats
static FLOOD_IDS: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));
static SESSION_IDS: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));
//the chatting status is used to keep track of: chat activity, user we are chatting with, server we are connected to
static CHATTING_STATUS: Lazy<Mutex<(bool , NodeId , NodeId)>> = Lazy::new(|| Mutex::new((false , 0 , 0)));

#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>,
    pub packet_send: HashMap<NodeId, Sender<Packet>>,
    pub sim_contr_recv: Receiver<DroneCommand>,
    pub shared_senders: Option<Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>>,
    sent_messages: HashMap<u64, Vec<Fragment>>,
    net_graph: Graph<u8, u8, Undirected>,
    node_map: HashMap<NodeId , (NodeIndex , NodeType)>,
    received_packets: HashMap<u64 , Vec<u8>>,
    seen_flood_ids : HashSet<(u64 , NodeId)>,
    simulation_log: Arc<Mutex<Vec<String>>>,
    shortcut_receiver: Option<Receiver<Packet>>,

}

impl MyClient {
    pub(crate) fn new(
        id: NodeId,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
        shared_senders: Option<Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>>,
        shortcut_receiver: Option<Receiver<Packet>>,

    ) -> Self {
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
            shared_senders,
            shortcut_receiver,
        }
    }

    pub(crate) fn run(&mut self, gui_input: SharedGuiInput) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.inner_run(gui_input.clone());
        }));

        match result {
            Ok(_) => {
                info!("run() exited normally.");
            }
            Err(e) => {
                if let Some(s) = e.downcast_ref::<&str>() {
                    warn!("⚠ run() panicked: {}", s);
                } else if let Some(s) = e.downcast_ref::<String>() {
                    warn!("⚠ run() panicked: {}", s);
                } else {
                    warn!("⚠ run() panicked with unknown error.");
                }

                // Prevent multiple restarts with a lock file
                let lock_path = Path::new("/tmp/drone_client_restart.lock");
                let lock_result = OpenOptions::new().write(true).create_new(true).open(lock_path);

                match lock_result {
                    Ok(mut file) => {
                        let current_exe = std::env::current_exe().expect("Failed to get current executable path");

                        info!("🔁 Restarting the application...");

                        // Write process ID to lock for debugging
                        let _ = writeln!(file, "Restarted by PID: {}", std::process::id());

                        // Launch new instance (non-blocking is fine here)
                        let _ = Command::new(current_exe)
                            .args(std::env::args().skip(1))
                            .spawn()
                            .expect("Failed to restart application");

                        // Give the child time to start and take over
                        thread::sleep(Duration::from_secs(1));

                        // Cleanup: remove lock file so future restarts work
                        let _ = fs::remove_file(lock_path);

                        // Exit old instance
                        exit(0);
                    }

                    Err(ref e) if e.kind() == ErrorKind::AlreadyExists => {
                        warn!("⚠ Restart already in progress (lock file present). Skipping restart.");
                        // Sleep to avoid respamming restarts
                        thread::sleep(Duration::from_secs(2));
                    }

                    Err(e) => {
                        warn!("⚠ Failed to create lock file for restart protection: {}", e);
                    }
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
            recv(self.shortcut_receiver.as_ref().unwrap()) -> packet => {
                    if let Ok(packet) = packet {
                        println!("Client {} received shortcut packet: {:?}", self.id, packet);
                        self.process_packet(packet);
                    }
                }
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

    fn process_flood_response(&mut self, response: &FloodResponse) {
        info!(
        "Client {:?} is processing a FloodResponse {:?}",
        self.id, response.path_trace
    );

        for i in 0..response.path_trace.len() - 1 {
            let node1 = Self::add_node_no_duplicate(
                &mut self.net_graph,
                &mut self.node_map,
                response.path_trace[i].0,
                response.path_trace[i].1,
            );
            let node2 = Self::add_node_no_duplicate(
                &mut self.net_graph,
                &mut self.node_map,
                response.path_trace[i + 1].0,
                response.path_trace[i + 1].1,
            );
            Self::add_edge_no_duplicate(&mut self.net_graph, node1, node2, 1);
        }
        info!("NODES = {:?}", self.node_map);
    }

    fn reassemble_packet(&mut self, fragment: &Fragment, packet: &mut Packet) {
        let session_id  = packet.session_id;
        let total_frags = fragment.total_n_fragments as usize;
        let frag_len    = fragment.length as usize;
        let slot_bytes  = 128;

        let (need_ack, is_complete) = {
            let buf = self.received_packets
                .entry(session_id)
                .or_insert_with(|| vec![0u8; total_frags * slot_bytes]);
            if buf.len() != total_frags * slot_bytes {
                buf.resize(total_frags * slot_bytes, 0);
            }

            let offset = (fragment.fragment_index as usize) * slot_bytes;
            buf[offset .. offset + frag_len]
                .copy_from_slice(&fragment.data[..frag_len]);

            let need_ack = true;

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
        };

        if need_ack {
            self.send_ack(packet, fragment);
        }

        if is_complete {
            let buf = self.received_packets.remove(&session_id).unwrap();
            let full_len= (total_frags - 1) * slot_bytes + frag_len;
            let message= buf[..full_len].to_vec();

            self.packet_command_handling(message);
            info!("👻👻👻👻👻👻  Packet with session_id {} fully reassembled 👻👻👻👻👻👻", session_id);
        }
    }

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        let message_list = self.sent_messages.clone();
        match nack.nack_type {
            NackType::Dropped => {
                self.increase_cost(packet.routing_header.hops[0]); // to properly use our pathfinding algorithms the links between the drones are weighted based on the number of "Dropped" Nacks we receive from each drone
                if let Some(fragments) = message_list.get(&packet.session_id) {
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
            },
            NackType::ErrorInRouting(node_id) => {
                let involved_in_any = {
                    let shared_senders = self.shared_senders.as_ref().unwrap().lock().unwrap();
                    shared_senders.keys().any(|(a, b)| *a == node_id || *b == node_id)
                };

                if involved_in_any {
                    if let Some(pos) = packet.routing_header.hops.iter().position(|&n| n == node_id) {
                        if pos > 0 {
                            let prev_node = packet.routing_header.hops[pos - 1];

                            if let (Some((from_index, _)), Some((to_index, _))) =
                                (self.node_map.get(&prev_node), self.node_map.get(&node_id))
                            {
                                if let Some(edge) = self.net_graph.find_edge(*from_index, *to_index) {
                                    self.net_graph.remove_edge(edge);
                                }
                            }
                        }
                    }

                    self.send_flood_request();
                } else {
                    self.safe_remove_node(node_id);
                    self.send_flood_request();
                }
            },
            _ => {
                // since the other possible Nacks that can be received presume some malfunctioning in the flooding or the drones themselves
                // we can't properly intervene on the graph, so we should try to flood as we see fit
                self.send_flood_request();
            }
        }
    }

    pub fn send_packet(&mut self, input: String) {
        let message = input.trim_end();
        let chunks: Vec<&[u8]> = message.as_bytes().chunks(128).collect();
        let total_fragments = chunks.len() as u64;
        let target = (*CHATTING_STATUS.lock().unwrap()).2;

        let Some(hops) = self.best_path(self.id, target) else {
            error!("❌ Still no best path. Aborting message.");
            self.log("Client could not calculate a best path after".to_string());
            return;
        };

        let session_id = SESSION_IDS.lock().unwrap().clone();

        for (i, chunk) in chunks.iter().enumerate() {
            let mut data = [0u8; 128];
            data[..chunk.len()].copy_from_slice(chunk);

            let fragment = Fragment {
                fragment_index: i as u64,
                total_n_fragments: total_fragments,
                length: chunk.len() as u8,
                data,
            };

            let packet = Packet {
                routing_header: SourceRoutingHeader {
                    hop_index: 1,
                    hops: hops.clone(),
                },
                session_id,
                pack_type: MsgFragment(fragment.clone()),
            };

            println!("♥♥ BEST PATH IS : {:?}", packet.routing_header.hops);

            self.sent_messages.entry(session_id).or_insert_with(Vec::new).push(fragment);

            if let Some(&next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index) {
                if let Some(sender) = self.packet_send.get(&next_hop) {
                    match sender.try_send(packet.clone()) {
                        Ok(()) => {
                            info!(
                            "📤 Sent fragment {} of {} to {} (path: {:?})",
                            i + 1,
                            total_fragments,
                            next_hop,
                            packet.routing_header.hops
                        );
                        }
                        Err(e) => {
                            warn!("❌ Failed to send fragment {} to {}: {:?}", i + 1, next_hop, e);
                        }
                    }
                } else {
                    warn!("❌ No sender found for next hop {}", next_hop);
                }
            } else {
                warn!("❌ No next hop at index {} in path {:?}", packet.routing_header.hop_index, packet.routing_header.hops);
            }
        }

        self.increment_ids(&SESSION_IDS);
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
        let chatting_status = match CHATTING_STATUS.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => {
                eprintln!("⚠ Mutex poisoned! Recovering.");
                *poisoned.into_inner()
            }
        };
        println!("Client {} processing GUI command '{}'", self.id, command_string.clone());
        let tokens: Vec<&str> = command_string.trim().split("::").collect();
        if tokens.len() >= 2 && tokens[0] == "[FloodRequired]" {
            let action = tokens[1..].join("::");
            println!("Client {} received FLOOD REQUIRED command due to action: {}.", self.id, action);
            self.log(format!("Client {} received a call to flooding the network", self.id));
            if let Some(parts) = action.strip_prefix("AddSender::") {
                let shared_senders = self.shared_senders.clone();
                let nodes: Vec<&str> = parts.splitn(2, "::").collect();
                if nodes.len() == 2 {
                    if let (Ok(a), Ok(b)) = (nodes[0].parse::<NodeId>(), nodes[1].parse::<NodeId>()) {
                        if self.id == a || self.id == b {
                            let peer = if self.id == a { b } else { a };
                            if let Some(shared) = &shared_senders {
                                if let Ok(map) = shared.lock() {
                                    if let Some(sender) = map.get(&(self.id, peer)) {
                                        self.packet_send.insert(peer, sender.clone());
                                        let self_idx = (*self.node_map.get(&self.id).unwrap_or(&(NodeIndex::default(), NodeType::Drone))).0;
                                        let maybe_node_index: Option<&NodeIndex> = self.node_map.get(&peer).map(|(index, _)| index);
                                        let peer_idx = if let Some(idx) = maybe_node_index {
                                            *idx
                                        } else {
                                            let idx = Self::add_node_no_duplicate(&mut (self.net_graph.clone()), &mut (self.node_map.clone()) , peer , NodeType::Drone);
                                            idx
                                        };
                                        Self::add_edge_no_duplicate(&mut (self.net_graph.clone()), self_idx , peer_idx, 1);
                                        println!("Client {} added link to {} via AddSender", self.id, peer);
                                        self.send_flood_request();
                                        info!("retunring from addsender");
                                        return Ok("NO_CHAT_COMMAND".to_string());
                                    }
                                }

                            }
                        }
                    }
                }

            }

            if let Some(parts) = action.strip_prefix("RemoveSender::") {
                let nodes: Vec<&str> = parts.splitn(2, "::").collect();
                if nodes.len() == 2 {
                    if let (Ok(a), Ok(b)) = (nodes[0].parse::<NodeId>(), nodes[1].parse::<NodeId>()) {
                        if self.id == a || self.id == b {
                            let peer = if self.id == a { b } else { a };
                            self.packet_send.remove(&peer);
                            if let (a_idx, b_idx) = ((*self.node_map.get(&a).unwrap_or(&(NodeIndex::default(), NodeType::Drone))).0, (*self.node_map.get(&b).unwrap_or(&(NodeIndex::default(), NodeType::Drone))).0) {
                                if let Some(edge) = self.net_graph.find_edge(a_idx, b_idx).or_else(|| self.net_graph.find_edge(b_idx, a_idx)) {
                                    self.net_graph.remove_edge(edge);
                                    println!("Client {} removed link to {} via RemoveSender", self.id, peer);
                                    self.send_flood_request();
                                    info!("returning from remove sender");
                                    return Ok("NO_CHAT_COMMAND".to_string());
                                }

                            }
                        }

                    }
                }

            }

            if let Some(parts) = action.strip_prefix("SpawnDrone::") {
                let shared_senders = self.shared_senders.clone();
                let components: Vec<&str> = parts.splitn(2, "::").collect();
                if components.len() == 2 {
                    if let Ok(drone_id) = components[0].parse::<NodeId>() {
                        if let Ok(peer_vec) = serde_json::from_str::<Vec<NodeId>>(components[1]) {
                            println!("Client {} parsing SpawnDrone with id {} and peers {:?}", self.id, drone_id, peer_vec);
                            if let Some(shared) = &shared_senders {
                                println!("shared senders found");
                                if let Ok(map) = shared.lock() {
                                    println!("map found");
                                    if map.contains_key(&(drone_id, self.id)) || map.contains_key(&(self.id, drone_id)) {
                                        // Proceed with insertion
                                        for ((from, to), sender) in map.iter() {
                                            if *from == self.id && *to == drone_id {
                                                self.packet_send.insert(drone_id, sender.clone());
                                                println!("Client {} added sender to drone {} (from shared_senders)", self.id, drone_id);
                                                self.send_flood_request();
                                                return Ok("NO_CHAT_COMMAND".to_string());
                                            }
                                            else if *to == self.id && *from == drone_id {
                                                self.packet_send.insert(drone_id, sender.clone());
                                                println!("Client {} added sender from drone {} (from shared_senders)", self.id, drone_id);
                                                self.send_flood_request();
                                                return Ok("NO_CHAT_COMMAND".to_string());
                                            }
                                        }
                                        self.send_flood_request();
                                        return Ok("NO_CHAT_COMMAND".to_string());
                                    }
                                }
                            }


                        } else {
                            println!("Client {} failed to parse peer list in SpawnDrone: {}", self.id, components[1]);
                        }
                    } else {
                        println!("Client {} failed to parse drone ID in SpawnDrone: {}", self.id, components[0]);
                    }
                } else {
                    println!("Client {} received malformed SpawnDrone message: {}", self.id, parts);
                }
                info!("returning from spawn drone");
            }

            if action.starts_with("Crash::") {
                let parts: Vec<&str> = action.split("::").collect();
                if parts.len() == 2 {
                    if let Ok(crashed_id) = parts[1].parse::<NodeId>() {
                        println!("Client {} received crash signal for node {}. Cleaning up and triggering rediscovery.", self.id, crashed_id);
                        self.node_map.remove(&crashed_id).map(|(index, _)| index);
                        self.safe_remove_node(crashed_id);
                        self.send_flood_request();
                        return Ok("NO_CHAT_COMMAND".to_string());

                    } else {
                        println!("Client {} received invalid Crash ID: {}", self.id, parts[1]);
                    }
                } else {
                    println!("Client {} received malformed Crash command: {}", self.id, action);
                }
                return Ok("NO_CHAT_COMMAND".to_string());
            }
            //info!("returning from crash");
            //self.send_flood_request();
            return Ok("NO_CHAT_COMMAND".to_string());
        }
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
                    Err(Box::new(io::Error::new(ErrorKind::Interrupted, "You are still in a chat with another user. End the chat before logging out")))
                } else if chatting_status.2 != 0 {
                    self.log(format!("Logout from client {}",self.id));
                    Ok(command_string)
                } else { //if we are yet to log in to any server we can log out of it
                    Err(Box::new(io::Error::new(ErrorKind::NotFound, "You have yet to login to any server")))
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
                    Err(Box::new(io::Error::new(ErrorKind::Interrupted, "You are already in a chat with another user.")))
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
                    Err(Box::new(io::Error::new(ErrorKind::Interrupted, "You are not chatting with any user.")))
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
            _ => {
                println!("Unknown format");
                Err(Box::new(io::Error::new(ErrorKind::NotFound, "Unknown format")))
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

    fn add_node_no_duplicate(graph: &mut Graph<u8, u8, Undirected>, node_map: &mut HashMap<NodeId, (NodeIndex, NodeType)>, value: u8, node_type: NodeType) -> NodeIndex {
        if let Some(&(idx, _)) = node_map.get(&value) {
            idx
        } else {
            let idx = graph.add_node(value);
            node_map.insert(value, (idx, node_type));
            idx
        }
    }

    fn add_edge_no_duplicate(graph: &mut Graph<u8, u8, Undirected>, a: NodeIndex, b: NodeIndex, weight: u8) -> bool {
        if graph.node_weight(a).is_some() && graph.node_weight(b).is_some() {
            if !graph.contains_edge(a, b) {
                graph.add_edge(a, b, weight);
                info!("Adding edge {:?} == {:?}", a, b);
                true
            } else {
                false
            }
        } else {
            info!(
            "Tried to add edge between invalid indices: a = {:?}, b = {:?}",
            a, b
        );
            false
        }
    }

    fn safe_remove_node(&mut self, crash_id: NodeId) {
        if let Some((idx, _)) = self.node_map.remove(&crash_id) {
            // Remove all connected edges
            let edges: Vec<_> = self.net_graph.edges(idx).map(|e| e.id()).collect();
            for edge_id in edges {
                self.net_graph.remove_edge(edge_id);
            }
            // Remove the node itself
            self.net_graph.remove_node(idx);
        }
        self.packet_send.remove(&crash_id);
    }

    fn best_path(&mut self, source: NodeId, target: NodeId) -> Option<Vec<NodeId>> {
        // Check if the target has a valid id or a default one caused by an error
        if target == 0 {
            return None;
        }
        // Check if we received different ids for target and source
        if source == target {
            return Some(vec![source]);
        }
        let shared_senders = self.shared_senders.clone();
        if let Some(shared) = &shared_senders {
            let map_guard = match shared.lock() {
                Ok(guard) => guard,
                Err(poisoned) => {
                    eprintln!("⚠ Mutex for shared_senders was poisoned — recovering...");
                    let recovered = poisoned.into_inner();
                    // We fix the poisoned shared_senders with the recovered map
                    self.shared_senders = Some(Arc::new(Mutex::new(recovered.clone())));
                    // Then we try locking again
                    match self.shared_senders.as_ref().unwrap().lock() {
                        Ok(guard) => guard,
                        Err(_) => {
                            eprintln!("❌ Failed to recover shared_senders after poison");
                            return None;
                        }
                    }
                }
            };

            let mut to_remove = Vec::new();
            for edge in self.net_graph.edge_references() {
                let a_id = self.net_graph[edge.source()];
                let b_id = self.net_graph[edge.target()];
                if !map_guard.contains_key(&(a_id, b_id)) && !map_guard.contains_key(&(b_id, a_id)) {
                    to_remove.push((edge.source(), edge.target()));
                }
            }

            for (src, dst) in to_remove {
                if let Some(edge_idx) = self.net_graph.find_edge(src, dst) {
                    self.net_graph.remove_edge(edge_idx);
                }
            }
        }

        //We find each node NodeIndex
        let source_idx = self.node_map.get(&source)?.0;
        let target_idx = self.node_map.get(&target)?.0;

        let mut distances: HashMap<NodeIndex, u32> = self.net_graph.node_indices()
            .map(|idx| (idx, u32::MAX))
            .collect();
        let mut predecessors: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        let mut heap = BinaryHeap::new();

        distances.insert(source_idx, 0);
        heap.push(Reverse((0, source_idx)));

        //We apply Dijkstra's routing algorithm
        while let Some(Reverse((current_dist, current_node))) = heap.pop() {
            if current_dist > distances[&current_node] {
                continue;
            }

            if current_node == target_idx {
                break;
            }
            for edge in self.net_graph.edges(current_node) {
                let neighbor_idx = edge.target();
                let weight = *edge.weight() as u32;
                let neighbor_id = self.node_map.iter()
                    .find(|(_, &(idx, _))| idx == neighbor_idx)
                    .map(|(id, _)| *id)?;

                //We can only use Drone nodes as intermediate steps
                if neighbor_id != target && neighbor_id != source {
                    match self.node_map.get(&neighbor_id) {
                        Some(&(_, NodeType::Drone)) => {}, // Valid
                        _ => continue, // Skip
                    }
                }

                let alt_dist = current_dist.saturating_add(weight);
                if alt_dist < distances[&neighbor_idx] {
                    distances.insert(neighbor_idx, alt_dist);
                    predecessors.insert(neighbor_idx, current_node);
                    heap.push(Reverse((alt_dist, neighbor_idx)));
                }
            }

        }

        //We reconstruct path
        if distances.get(&target_idx)? == &u32::MAX {
            return None;
        }

        let mut path = Vec::new();
        let mut current = target_idx;

        loop {
            let node_id = self.node_map.iter()
                .find(|(_, &(idx, _))| idx == current)
                .map(|(id, _)| *id)?;
            path.push(node_id);

            if current == source_idx {
                break;
            }

            current = *predecessors.get(&current)?;
        }

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

    fn change_chat_status(&self, chatting: bool, peer_id: NodeId, server_id: NodeId) {
        match CHATTING_STATUS.lock() {
            Ok(mut status) => {
                status.0 = chatting;
                status.1 = peer_id;
                status.2 = server_id;
            }
            Err(poisoned) => {
                eprintln!("⚠ CHATTING_STATUS mutex was poisoned! Recovering and updating anyway.");
                let mut status = poisoned.into_inner();
                status.0 = chatting;
                status.1 = peer_id;
                status.2 = server_id;
            }
        }
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

        match &data[0..4] {
            [0x89, 0x50, 0x4E, 0x47] => Ok("png".to_string()),
            [0xFF, 0xD8, 0xFF, _] => Ok("jpg".to_string()),
            [0x47, 0x49, 0x46, 0x38] => Ok("gif".to_string()),
            [0x42, 0x4D, _, _] => Ok("bmp".to_string()),
            _ => {
                if data.len() >= 12 {
                    match &data[4..12] {
                        [0x66, 0x74, 0x79, 0x70, 0x69, 0x73, 0x6F, 0x6D] => Ok("mp4".to_string()),
                        [0x66, 0x74, 0x79, 0x70, 0x6D, 0x70, 0x34, 0x32] => Ok("mp4".to_string()),
                        _ => {
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
            Command::new("cmd")
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
