use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use std::process::Command;
use std::env::temp_dir;
use std::sync::{Arc, Mutex};
use once_cell::sync::Lazy;
use base64::{Engine, engine::general_purpose::STANDARD};
use image::load_from_memory;
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use crossbeam_channel::{select_biased, Receiver, Sender};
use log::{info, warn};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use crate::simulation_controller::gui_input_queue::{SharedGuiInput};

static SESSION_COUNTER : Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));

#[derive(Debug, Clone)]
pub struct ReceivedMessageState {
    pub data : Vec<u8>,
    pub total_fragments : u64,
    pub received_indices : HashSet<u64>,
    pub last_fragment_len: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct SentMessageInfo {
    pub fragments : Vec<Fragment>,
    pub original_routing_header : SourceRoutingHeader,
    pub received_ack_indices : HashSet<u64>,
    pub route_needs_recalculation : bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeInfo {
    id : NodeId,
    node_type : NodeType,
}

#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>, //receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, //sends packets to neighbors
    pub sent_messages: HashMap<u64, SentMessageInfo>,
    pub received_messages : HashMap<u64, ReceivedMessageState>, //to determine when the message is complete (it has all the fragments)
    pub network_graph : StableGraph<NodeInfo, usize>, //graph to memorize info about nodes
    pub node_id_to_index : HashMap<NodeId, NodeIndex>, //mapping from node_id to inner indices of the graph
    pub active_flood_discoveries: HashMap<u64, FloodDiscoveryState>, //structure to take track of flood_request/response
    pub connected_server_id : Option<NodeId>,
    pub seen_flood_ids : HashSet<(u64, NodeId)>,
    pub route_cache : HashMap<NodeId, Vec<NodeId>>,
    pub simulation_log: Arc<Mutex<Vec<String>>>,
    pub shared_senders: Option<Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>>,
    shortcut_receiver: Option<Receiver<Packet>>, // added to receive packets from sc (shortcut)
    pub pending_messages_after_flood: Vec<(NodeId, String)>, // (dest_id, gui_command)

}

#[derive(Debug, Clone)]
struct FloodDiscoveryState {
    initiator_id : NodeId,
    start_time : Instant,
    received_responses : Vec<FloodResponse>,
    //is_finalized : bool
}

impl MyClient {
    pub(crate) fn new(
        id: NodeId,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
        sent_messages: HashMap<u64, SentMessageInfo>,
        connected_server_id: Option<NodeId>,
        seen_flood_ids : HashSet<(u64, NodeId)>,
        shared_senders: Option<Arc<Mutex<HashMap<(NodeId,NodeId), Sender<Packet>>>>>,
        shortcut_receiver: Option<Receiver<Packet>>, // added to receive packets from sc (shortcut)
    ) -> Self {
        Self {
            id,
            packet_recv,
            packet_send,
            sent_messages,
            received_messages: HashMap::new(),
            network_graph: StableGraph::new(),
            node_id_to_index: HashMap::new(),
            active_flood_discoveries: HashMap::new(),
            connected_server_id,
            seen_flood_ids,
            route_cache : HashMap::new(),
            simulation_log: Arc::new(Mutex::new(Vec::new())),
            shared_senders, // ✅ store reference
            shortcut_receiver,
            pending_messages_after_flood: Vec::new(),

        }
    }



    pub(crate) fn run(&mut self, gui_input: SharedGuiInput) {

        println!("🔗🔗🔗Client {} starting run loop.", self.id);

        if self.network_graph.node_count() == 0 {
            println!("Client {} network graph is empty, starting flood discovery.", self.id);
            self.start_flood_discovery();
        }

        loop {
            // Process only ONE GUI message per iteration
            if let Ok(mut map) = gui_input.lock() {
                if let Some(msgs) = map.get_mut(&self.id) {
                    if !msgs.is_empty() {
                        let msg = msgs.remove(0); // remove the first message
                        drop(map); // release lock early
                        self.process_gui_command(self.id, msg); // process the message
                    }
                }
            }

            self.check_flood_discoveries_timeouts();

            select_biased! {
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet by client {} ...", self.id);
                    if let Ok(packet) = packet {
                        println!("❤❤❤Packet received by client {} : {:?}", self.id, packet);
                        self.process_packet(packet);
                    }
                },
                recv(self.shortcut_receiver.as_ref().unwrap()) -> packet => {
                    if let Ok(packet) = packet {
                        println!("Client {} received shortcut packet: {:?}", self.id, packet);
                        self.process_packet(packet);
                    }
                }

                default => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                },
            }
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

    fn check_flood_discoveries_timeouts(&mut self) {
        let now = Instant::now();
        let timeout_duration = Duration::from_millis(2000);
        let floods_to_finalize: Vec<u64> = self.active_flood_discoveries
            .iter()
            .filter(|(_, state)| now.duration_since(state.start_time) > timeout_duration)
            .map(|(&flood_id, _)| flood_id)
            .collect();

        let has_floods = !floods_to_finalize.is_empty(); // ✅ Save boolean before moving


        for flood_id in floods_to_finalize {
            println!("Client {} finalizing flood discovery for ID {} due to timeout.",
                     self.id, flood_id);
            self.finalize_flood_discovery_topology(flood_id);
        }

        // After updating topology, attempt to send any pending messages
        if has_floods && !self.pending_messages_after_flood.is_empty() {
            println!("Client {} processing {} pending message(s) after flood.",
                     self.id, self.pending_messages_after_flood.len());

            // Drain the pending messages to avoid double-processing
            let pending_list = std::mem::take(&mut self.pending_messages_after_flood);
            for (dest, pending_cmd) in pending_list {
                // Recompute the best path to the destination with updated graph
                if let Some(path) = self.best_path(self.id, dest) {
                    println!("Client {} computed path to {} after flood: {:?}",
                             self.id, dest, path);
                    self.route_cache.insert(dest, path.clone());
                    let routing_header = SourceRoutingHeader { hops: path, hop_index: 1 };

                    // Fragment the pending message data just like in process_gui_command
                    let data_bytes = pending_cmd.as_bytes();
                    const FRAGMENT_SIZE: usize = 128;
                    let total_frags = (data_bytes.len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
                    let session_id = {
                        let mut id_counter = SESSION_COUNTER.lock().unwrap();
                        *id_counter = id_counter.saturating_add(1);
                        *id_counter
                    };
                    // Prepare fragments
                    let mut fragments = Vec::new();
                    for i in 0..total_frags {
                        let start = i * FRAGMENT_SIZE;
                        let end = (start + FRAGMENT_SIZE).min(data_bytes.len());
                        let fragment_data = &data_bytes[start..end];

                        let mut fragment_buf = [0u8; FRAGMENT_SIZE];
                        fragment_buf[..fragment_data.len()].copy_from_slice(fragment_data);
                        fragments.push(Fragment {
                            fragment_index: i as u64,
                            total_n_fragments: total_frags as u64,
                            length: fragment_data.len() as u8,
                            data: fragment_buf,
                        });
                    }
                    // Store message info for ACK tracking
                    self.sent_messages.insert(session_id, SentMessageInfo {
                        fragments: fragments.clone(),
                        original_routing_header: routing_header.clone(),
                        received_ack_indices: HashSet::new(),
                        route_needs_recalculation: false,
                    });
                    // Send each fragment along the new route
                    if routing_header.hops.len() > routing_header.hop_index {
                        let first_hop = routing_header.hops[routing_header.hop_index];
                        for fragment in fragments {
                            let packet = Packet {
                                pack_type: PacketType::MsgFragment(fragment.clone()),
                                routing_header: routing_header.clone(),
                                session_id,
                            };
                            match self.send_to_neighbor(first_hop, packet) {
                                Ok(_) => println!(
                                    "Client {} sent pending fragment {} (session {}) to {} after flood.",
                                    self.id, fragment.fragment_index, session_id, first_hop
                                ),
                                Err(e) => eprintln!(
                                    "Client {} failed to send pending fragment {} (session {}) to {}: {}",
                                    self.id, fragment.fragment_index, session_id, first_hop, e
                                ),
                            }
                        }
                    } else {
                        eprintln!("Client {} cannot send pending message: no next hop in route to {}.",
                                  self.id, dest);
                    }

                    // If the pending command was a logout, update client state
                    if pending_cmd.starts_with("[Logout]") {
                        println!("🚪 Client {} logout completed after flood. Disconnected from server {}.",
                                 self.id, dest);
                        self.connected_server_id = None;
                    }
                } else {
                    warn!("Client {} still cannot find route to {} after flood; dropping message '{}'.",
                              self.id, dest, pending_cmd);
                    warn!("Please add a sender such that it connects the client to the server");
                }
            }
        }
    }


    fn finalize_flood_discovery_topology(&mut self, flood_id: u64) {
        if let Some(discovery_state) = self.active_flood_discoveries.remove(&flood_id) {
            println!("Client {} processing {} collected responses for flood ID {}.", self.id, discovery_state.received_responses.len(), flood_id);
            for response in discovery_state.received_responses {
                self.create_topology(&response);
            }
        } else {
            eprintln!("Client {} tried to finalize unknown flood discovery ID {}.", self.id, flood_id);
        }
    }

    fn process_packet(&mut self, mut packet: Packet) {
        let packet_type = packet.pack_type.clone();
        match packet_type {
            PacketType::FloodRequest(request) => { //CONTROLLA SE IL CLIENT PUò PROPAGARE FLOODREQUESTS
                //println!("Client {} received FloodRequest {}, ignoring as client should not propagate.", self.id, request.flood_id);
                self.process_flood_request(&request, packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                println!("Client {} received FloodResponse for flood_id {}.", self.id, response.flood_id);
                if let Some(discovery_state) = self.active_flood_discoveries.get_mut(&response.flood_id) {
                    discovery_state.received_responses.push(response.clone());
                    println!("Client {} collected {} responses for flood ID {}.", self.id, discovery_state.received_responses.len(), response.flood_id);
                } else {
                    eprintln!("Client {} received FloodResponse for unknown or inactive flood ID {}. Processing path anyway.", self.id, response.flood_id);
                    self.create_topology(&response);
                }
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut packet, &fragment);
                self.reassemble_packet(&fragment, &mut packet);
            },
            PacketType::Ack(ack) => {
                println!("Client {} received ACK for session {}, fragment {}.", self.id, packet.session_id, ack.fragment_index);
                if let Some(sent_msg_info) = self.sent_messages.get_mut(&packet.session_id) {
                    sent_msg_info.received_ack_indices.insert(ack.fragment_index);
                    println!("Client {} marked fragment {} of session {} as ACKed.", self.id, ack.fragment_index, packet.session_id);
                    if sent_msg_info.received_ack_indices.len() == sent_msg_info.fragments.len() {
                        println!("✅✅✅Client {} received all ACKs for session {}. Message considered successfully sent.", self.id, packet.session_id);
                        //self.sent_messages.remove(&packet.session_id);
                    }
                } else {
                    eprintln!("Client {} received ACK for unknown session {}.", self.id, packet.session_id);
                }
            },
            PacketType::Nack(nack) => {
                self.process_nack(&nack, &mut packet);
            }
        }
    }

    fn process_flood_request(&mut self, request: &FloodRequest, header: SourceRoutingHeader) {
        let flood_identifier = (request.flood_id, request.initiator_id);
        let sender_id = request.path_trace.last().map(|(id, _)| *id);
        if self.seen_flood_ids.contains(&flood_identifier) || (self.packet_send.len() == 1 && sender_id.is_some()){
            //- - - - sending FloodResponse back - - - -
            let mut response_path_trace = request.path_trace.clone();
            response_path_trace.push((self.id, NodeType::Client));
            let flood_response = FloodResponse {
                flood_id: request.flood_id,
                path_trace: response_path_trace,
            };
            if let Some(prev_hop_id) = sender_id {
                if let Some(sender) = self.packet_send.get(&prev_hop_id) {
                    let response_packet_to_send = Packet {
                        pack_type : PacketType::FloodResponse(flood_response.clone()),
                        routing_header: SourceRoutingHeader {
                            hop_index: 1,
                            hops: request.path_trace.iter().map(|(id, _)| *id).rev().collect(),
                        },
                        session_id : request.flood_id,
                    };
                    let mut response_route_hops : Vec<NodeId> = request.path_trace.iter().map(|(id, _)| *id).collect();
                    response_route_hops.push(self.id);
                    response_route_hops.reverse();
                    let first_response_hop = response_route_hops.get(1).cloned();
                    if let Some(next_hop_id) = first_response_hop {
                        if let Some(sender_channel) =  self.packet_send.get(&next_hop_id) {
                            let response_packet_to_send = Packet {
                                pack_type : PacketType::FloodResponse(flood_response.clone()),
                                routing_header : SourceRoutingHeader {
                                    hop_index : 1,
                                    hops : response_route_hops,
                                },
                                session_id : request.flood_id,
                            };
                            match sender_channel.send(response_packet_to_send) {
                                Ok(()) => println!("↩️↩️↩️Client {} sent FloodResponse {} back to {} (prev hop).", self.id, request.flood_id, prev_hop_id),
                                Err(e) => eprintln!("Client {} failed to send FloodResponse {} back to {}: {}", self.id, request.flood_id, prev_hop_id, e),
                            }
                        } else {
                            eprintln!("Client {} error: no sender found for previous hop {}", self.id, prev_hop_id);
                        }
                    } else {
                        eprintln!("Client {} cannot send FloodResponse back, path trace too short after adding self", self.id);
                    }
                } else {
                    eprintln!("Client {} error: could not find sender channel for previous hop {}", self.id, prev_hop_id);
                }
            } else {
                eprintln!("Client {} received FloodRequest with empty path trace or no sender info.", self.id);
            }
        } else {
            self.seen_flood_ids.insert(flood_identifier);
            let mut updated_request = request.clone();
            updated_request.path_trace.push((self.id, NodeType::Client));
            println!("Client {} processed FloodRequest {}. Path trace now: {:?}", self.id, request.flood_id, updated_request.path_trace);
            for (neighbor_id, sender_channel) in self.packet_send.clone() {
                if sender_id.is_none() || neighbor_id != sender_id.unwrap() {
                    let packet_to_forward = Packet {
                        pack_type : PacketType::FloodRequest(updated_request.clone()),
                        routing_header : SourceRoutingHeader {
                            hop_index : 0,
                            hops : vec![],
                        },
                        session_id : request.flood_id,
                    };
                    match sender_channel.send(packet_to_forward) {
                        Ok(_) => println!("⏩⏩⏩Client {} forwarded FloodRequest {} to neighbor {}.", self.id, request.flood_id, neighbor_id),
                        Err(e) => eprintln!("Client {} failed to forward FloodRequest {} to neighbor {}: {}", self.id, request.flood_id, neighbor_id, e),
                    }
                }
            }
        }
    }


    fn start_flood_discovery(&mut self) {
        println!("🦋🦋🦋Client {} starting flood discovery.", self.id);
        //1.generating unique flood_id
        let new_flood_id = {
            let mut id_counter = SESSION_COUNTER.lock().unwrap();
            *id_counter = id_counter.saturating_add(1);
            *id_counter
        };
        //2.crating flood_request
        let flood_request = FloodRequest {
            flood_id: new_flood_id,
            initiator_id: self.id,
            path_trace: vec![(self.id, NodeType::Client)],
        };
        //3.sending flood_request to neighbor
        let flood_packet = Packet {
            pack_type: PacketType::FloodRequest(flood_request),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: vec![],
            },
            session_id: new_flood_id,
        };

        println!("♥♥♥♥♥♥♥ client1 has senders towards this drones: {:?}", self.packet_send);


        println!("🌊🌊🌊Client {} sending FloodRequest {} to all neighbors.", self.id, new_flood_id);
        //4.sending flood_request to all neighbors
        for (&neighbor_id, sender) in &self.packet_send {
            match sender.send(flood_packet.clone()) {
                Ok(_) =>  println!("Client {} sent FloodRequest to neighbor {}.", self.id, neighbor_id),
                Err(e) => eprintln!("Client {} failed to send FloodRequest to neighbor {}: {}", self.id, neighbor_id, e),
            }
        }

        self.active_flood_discoveries.insert(new_flood_id, FloodDiscoveryState {
            initiator_id: self.id,
            start_time: Instant::now(),
            received_responses: Vec::new()
        });
    }

    fn reassemble_packet(&mut self, fragment: &Fragment, packet: &mut Packet) {
        let session_id = packet.session_id;
        let src_id = packet.routing_header.hops.first().copied().unwrap_or_else(|| {
            eprintln!("Client {} error: Received packet with empty hops in routing header for session {}.", self.id, session_id);
            0
        });
        let key = session_id;
        let fragment_len = fragment.length as usize;
        let offset = (fragment.fragment_index * 128) as usize;
        let state = self.received_messages.entry(key).or_insert_with(|| {
            println!("Client {} starting reassembly for session {} from source {}.", self.id, session_id, src_id);
            ReceivedMessageState {
                data: vec![0u8; (fragment.total_n_fragments * 128) as usize],
                total_fragments: fragment.total_n_fragments,
                received_indices: HashSet::new(),
                last_fragment_len: None,
            }
        });
        if state.received_indices.contains(&fragment.fragment_index) {
            println!("Received duplicate fragment {} for session {}. Ignoring.", fragment.fragment_index, session_id);
            return;
        }
        if fragment.fragment_index == fragment.total_n_fragments - 1 {
            state.last_fragment_len = Some(fragment.length);
        }

        if offset + fragment_len <= state.data.len() {
            state.data[offset..offset + fragment_len].copy_from_slice(&fragment.data[..fragment_len]);
            state.received_indices.insert(fragment.fragment_index);

            println!("Client {} received fragment {} for session {}.", self.id, fragment.fragment_index, session_id);

            if state.received_indices.len() as u64 == state.total_fragments {
                println!("🧩🧩🧩Message for session {} reassembled successfully.", session_id);
                let total_fragments_local = state.total_fragments;
                let last_fragment_len_local = state.last_fragment_len;
                let data_to_process_local = state.data.clone();
                self.received_messages.remove(&key);
                let mut full_message_data = data_to_process_local;
                if let Some(last_len) = last_fragment_len_local {
                    let expected_total_len = (total_fragments_local as usize - 1) * 128 + last_len as usize;
                    full_message_data.truncate(expected_total_len);
                } else if total_fragments_local == 1 {
                    full_message_data.truncate(fragment.length as usize);
                }
                let message_string = String::from_utf8_lossy(&full_message_data).to_string();
                self.process_received_high_level_message(message_string, src_id, session_id);
            }
        } else {
            eprintln!("Received fragment {} for session {} with offset {} and length {} which exceeds excepted message size {}. Ignoring fragment.", fragment.fragment_index, session_id, offset, fragment_len, state.data.len());
        }
    }

    //function in which the correctly reassembled high level messages are elaborated
    fn process_received_high_level_message(&mut self, message_string: String, source_id: NodeId, session_id: u64) {
        //println!("Client {} processing high-level message for session {} from source {}: {}", self.id, session_id, source_id, message_string);
        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();

        if tokens.is_empty() {
            eprintln!("Client {} received empty high-level message for session {}.", self.id, session_id);
            return;
        }

        match tokens.as_slice() {
            ["[MessageFrom]", sender_id_str, content_str] => {
                // format: [MessageFrom]::sender_id::message_content
                if let Ok(sender_id) = sender_id_str.parse::<NodeId>() {
                    let content = *content_str;
                    println!("Client {} received chat message from client {}.", self.id, sender_id);
                } else {
                    eprintln!("Client {} received MessageFrom with invalid sender_id: {}.", self.id, sender_id_str);
                }
            },
            ["[ClientListResponse]", list_str] => {
                //format: [ClientListResponse]::[client1_id, client2_id, ...]
                let client_ids: Vec<NodeId> = list_str.trim_start_matches('[').trim_end_matches(']').split(',').filter(|s| !s.trim().is_empty()).filter_map(|s| s.trim().parse::<NodeId>().ok()).collect();
                println!("Client {} received CLIENT LIST: {:?}", self.id, client_ids);
            },
            ["[ChatStart]", status_str] => {
                //format: [ChatStart]::bool
                match status_str.to_lowercase().as_str() {
                    "true" => {
                        println!("Client {} CHAT REQUEST accepted. Chat started.", self.id);
                    },
                    "false" => {
                        println!("Client {} CHAT REQUEST denied.", self.id);
                    },
                    _ => eprintln!("Client {} received ChatStart with invalid boolean value: {}.", self.id, status_str),
                }
            },
            ["[ChatFinish]"] => {
                //format: [ChatFinish]
                println!("Client {} CHAT TERMINATED.", self.id);
            },
            ["[ChatRequest]", peer_id_str] => {
                //format: [ChatRequest]::{peer_id}
                if let Ok(requester_id) = peer_id_str.parse::<NodeId>() {
                    println!("Client {} received incoming CHAT REQUEST from client {}.", self.id, requester_id);
                } else {
                    eprintln!("Client {} received ChatRequest with invalid requester_id: {}.", self.id, peer_id_str);
                }
            },
            ["[HistoryResponse]", content_str] => {
                //format: [HistoryResponse]::message_content
                let history_content = *content_str;
                println!("Client {} received CHAT HISTORY: {}", self.id, history_content);
            },
            ["[MediaUploadAck]", media] => {
                //format: [MediaUploadAck]::media_name
                if tokens.len() >= 2 {
                    let media_name = media;
                    println!("Client {} received MEDIA UPLOAD ACK for media '{}'.", self.id, media_name);
                } else {
                    eprintln!("Client {} received invalid MEDIA UPLOAD ACK format: {}.", self.id, message_string);
                }
            },
            ["[MediaDownloadResponse]", media, base64] => {
                if tokens.len() >= 3 {
                    let media_name = media;
                    let base64_data = base64;

                    if media_name.to_string() == "ERROR" && base64_data.to_string() == "NotFound" {
                        println!("Client {} received MEDIA DOWNLOAD RESPONSE: Media not found.", self.id)
                    } else {
                        println!("Client {} received MEDIA DOWNLOAD RESPONSE for media '{}'.", self.id, media_name);

                        match STANDARD.decode(&base64_data) {
                            Ok(image_bytes) => {
                                match load_from_memory(&image_bytes) {
                                    Ok(img) => {
                                        println!("Client {} successfully loaded image data for media '{}'.", self.id, media_name);

                                        let extension = match img.color() {
                                            image::ColorType::Rgb8
                                            | image::ColorType::Rgba8
                                            | image::ColorType::L8
                                            | image::ColorType::La8
                                            | image::ColorType::Rgb16
                                            | image::ColorType::Rgba16
                                            | image::ColorType::L16
                                            | image::ColorType::La16 => "png",
                                            _ => "bin",
                                        };

                                        // Create a temp file with a proper extension
                                        let mut temp_file_path = temp_dir();
                                        let filename = format!("media_preview_{}.{}", self.id, extension);
                                        temp_file_path.push(filename);

                                        // Save temporarily
                                        if let Err(e) = img.save(&temp_file_path) {
                                            eprintln!("Client {} failed to temporarily save image: {}", self.id, e);
                                            return;
                                        }

                                        // Open with system viewer
                                        let open_command = if cfg!(target_os = "windows") {
                                            "cmd"
                                        } else if cfg!(target_os = "macos") {
                                            "open"
                                        } else {
                                            "xdg-open"
                                        };

                                        let path_str = temp_file_path.to_string_lossy().into_owned();
                                        let args = if cfg!(target_os = "windows") {
                                            vec!["/C", "start", "", &path_str]
                                        } else {
                                            vec![path_str.as_str()]
                                        };

                                        match Command::new(open_command).args(args.clone()).spawn() {
                                            Ok(_) => println!("🖼️ Opened media '{}' for client {}", media_name, self.id),
                                            Err(e) => eprintln!("Failed to open image: {}", e),
                                        }
                                    },
                                    Err(e) => eprintln!("Client {} failed to load image from memory: {}", self.id, e),
                                }
                            },
                            Err(e) => eprintln!("Client {} failed to decode base64 data: {}", self.id, e),
                        }
                    }
                }
            },
            ["[MediaListResponse]", list_str] => {
                println!("Client {} received MEDIA LIST: {}.", self.id, list_str);
                let media_list : Vec<String> = list_str.split(',').filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
                println!("Available media files: {:?}", media_list);
            },
            ["[LoginAck]", session_id] => {
                //format: [LoginAck]::session_id
                if let Ok(parsed_session_id) = session_id.parse::<u64>() {
                    println!("🔑🔑🔑Client {} received LOGIN ACK for session {}. Successfully logged in!", self.id, parsed_session_id);
                } else {
                    eprintln!("Client {} received LOGIN ACK with invalid session ID: {}.", self.id, session_id);
                }
            },
            [type_tag, ..] => {
                eprintln!("Client {} received unrecognized high-level message type: {}.", self.id, type_tag);
            },
            [] => {
                eprintln!("Client {} received empty high-level message for session {}.", self.id, session_id);
            },
        }
    }

    fn send_ack(&mut self, packet: &mut Packet, fragment: &Fragment) {
        let mut ack_routing_header = packet.routing_header.clone();
        ack_routing_header.hops.reverse();
        ack_routing_header.hop_index = 1;

        let destination_node_id = ack_routing_header.hops.first().copied();
        let next_hop_for_ack_option = ack_routing_header.hops.get(1).copied();
        if let (Some(next_hop_for_ack), Some(_destination_node_id)) = (next_hop_for_ack_option, destination_node_id) {
            let ack_packet_to_send = Packet {
                pack_type: PacketType::Ack(Ack { fragment_index: fragment.fragment_index }),
                routing_header: ack_routing_header,
                session_id: packet.session_id,
            };
            match self.send_to_neighbor(next_hop_for_ack, ack_packet_to_send.clone()) {
                Ok(_) => println!("👍👍👍Client {} sent ACK for session {} fragment {} to neighbor {}.", self.id, packet.session_id, fragment.fragment_index, next_hop_for_ack),
                Err(e) => eprintln!("Client {} error sending ACK packet for session {} to neighbor {}: {}. Using ControllerShortcut.", self.id, packet.session_id, next_hop_for_ack, e),

            }
        } else {
            eprintln!("Client {} error: cannot send ACK, original routing header hops too short: {:?}. Cannot use ControllerShortcut.", self.id, packet.routing_header.hops);
        }
    }

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        println!("🚨🚨🚨Client {} processing NACK type {:?} for session {}.", self.id, nack.nack_type, packet.session_id);
        match &nack.nack_type {
            NackType::ErrorInRouting(problem_node_id) => {
                eprintln!("Client {} received ErrorInRouting NACK for session {} at node {}.", self.id, packet.session_id, problem_node_id);

                let route = &packet.routing_header.hops;
                let hop_index = packet.routing_header.hop_index;
                let from = route.get(hop_index.saturating_sub(1)).copied();

                // This flag lets us decide what to do AFTER releasing lock
                let mut crashed = None;

                if let Some(shared) = &self.shared_senders {
                    if let Ok(map) = shared.lock() {
                        let still_alive = map.keys().any(|(a, b)| *a == *problem_node_id || *b == *problem_node_id);
                        crashed = Some(!still_alive);
                    }
                }

                // 🔓 LOCK IS DROPPED NOW — safe to mutate self
                if let Some(from) = from {
                    if let Some(true) = crashed {
                        self.remove_node_from_graph(*problem_node_id);
                        self.packet_send.remove(problem_node_id);
                        self.log(format!("Node {} crashed (removed from graph)", problem_node_id));
                    } else {
                        self.packet_send.remove(problem_node_id);
                        self.remove_link_from_graph(from, *problem_node_id);
                        self.log(format!("Link removed between {} and {}", from, problem_node_id));
                    }
                }

                if let Some(info) = self.sent_messages.get_mut(&packet.session_id) {
                    info.route_needs_recalculation = true;
                    if let Some(&dest_id) = info.original_routing_header.hops.last() {
                        self.route_cache.remove(&dest_id);
                    }
                }
            }

            NackType::DestinationIsDrone => {
                eprintln!("Client {} received DestinationIsDrone NACK for session {}.", self.id, packet.session_id);
                if let Some(info) = self.sent_messages.get_mut(&packet.session_id) {
                    info.route_needs_recalculation = true;
                    println!("Client {} marked route for session {} for recalculation due to DestinationIsDrone.", self.id, packet.session_id);
                    if let Some(&dest_id) = info.original_routing_header.hops.last() {
                        self.route_cache.remove(&dest_id);
                        println!("Client {} invaliding cached route for {} due to DestinationIsDrone NACK.", self.id, dest_id);
                    }
                } else {
                    eprintln!("Client {} received DestinationIsDrone NACK for unknown session {}.", self.id, packet.session_id);
                }
            }
            NackType::UnexpectedRecipient(received_at_node_id) => {
                eprintln!("Client {} received UnexpectedRecipient NACK for session {}: packet arrived at node {} but expected a different one.", self.id, packet.session_id, received_at_node_id);
                let route = &packet.routing_header.hops;
                let index = packet.routing_header.hop_index;
                if index > 0 && index < route.len() {
                    let from = route[index - 1];
                    let to = route[index];
                    self.increment_drop(from, to);
                    self.increment_drop(to, from);
                }
                if let Some(info) = self.sent_messages.get_mut(&packet.session_id) {
                    info.route_needs_recalculation = true;
                    if let Some(&dest_id) = info.original_routing_header.hops.last() {
                        self.route_cache.remove(&dest_id);
                    }
                }
            }
            NackType::Dropped => {
                eprintln!("Client {} received Dropped NACK for session {}, fragment {}.", self.id, packet.session_id, nack.fragment_index);
                let resend_info = self.sent_messages.get(&packet.session_id).and_then(|info| {
                    info.fragments.iter().find(|f| f.fragment_index == nack.fragment_index).map(|frag| (frag.clone(), info.original_routing_header.clone()))
                });
                if let Some((fragment, routing)) = resend_info {
                    let route = &packet.routing_header.hops;
                    let index = packet.routing_header.hop_index;
                    if index > 0 && index < route.len() {
                        let from = route[index - 1];
                        let to = route[index];
                        self.increment_drop(from, to);
                        self.increment_drop(to, from);
                    }
                    let resend_packet = Packet {
                        pack_type: PacketType::MsgFragment(fragment.clone()),
                        routing_header: routing,
                        session_id: packet.session_id,
                    };
                    if resend_packet.routing_header.hops.len() > resend_packet.routing_header.hop_index {
                        let first_hop = resend_packet.routing_header.hops[resend_packet.routing_header.hop_index];
                        match self.send_to_neighbor(first_hop, resend_packet) {
                            Ok(_) => println!("🔁🔁🔁Client {} resent fragment {} for session {} to {}.", self.id, fragment.fragment_index, packet.session_id, first_hop),
                            Err(e) => eprintln!("Client {} failed to resend fragment {} for session {} to {}: {}", self.id, fragment.fragment_index, packet.session_id, first_hop, e),
                        }
                    } else {
                        eprintln!("Client {} cannot resend fragment {}: invalid hop index in original route.", self.id, fragment.fragment_index);
                    }
                } else {
                    eprintln!("Client {} could not find original fragment {} for session {}.", self.id, nack.fragment_index, packet.session_id);
                }
            }
        }
    }

    fn remove_link_from_graph(&mut self, from: NodeId, to: NodeId) {
        if let (Some(&from_idx), Some(&to_idx)) = (
            self.node_id_to_index.get(&from),
            self.node_id_to_index.get(&to),
        ) {
            let mut removed = false;

            if let Some(edge) = self.network_graph.find_edge(from_idx, to_idx) {
                self.network_graph.remove_edge(edge);
                println!("Client {} removed edge {} → {} from graph.", self.id, from, to);
                removed = true;
            }

            if let Some(edge) = self.network_graph.find_edge(to_idx, from_idx) {
                self.network_graph.remove_edge(edge);
                println!("Client {} removed edge {} → {} from graph.", self.id, to, from);
                removed = true;
            }

            if !removed {
                println!("Client {}: No link existed between {} and {}", self.id, from, to);
            }
        } else {
            println!(
                "Client {}: Cannot remove link — one or both nodes missing: {} or {}",
                self.id, from, to
            );
        }
    }


    fn create_topology(&mut self, flood_response: &FloodResponse) {
        //println!("Client {} processing FloodResponse for flood_id {}.", self.id, flood_response.flood_id);
        let path = &flood_response.path_trace;
        if path.is_empty() {
            println!("Received empty path_trace in FloodResponse for flood_id {}.", flood_response.flood_id);
            return;
        }
        let (initiator_id, initiator_type) = path.first().cloned().unwrap();
        if let std::collections::hash_map::Entry::Vacant(e) = self.node_id_to_index.entry(initiator_id) {
            let node_index = self.network_graph.add_node(NodeInfo { id: initiator_id, node_type: initiator_type });
            e.insert(node_index);
            //println!("Client {} added initiator node {} ({:?}) to graph.", self.id, initiator_id, initiator_type);
        }
        for i in 0..path.len() {
            let (current_node_id, current_node_type) = path[i];
            if let std::collections::hash_map::Entry::Vacant(e) = self.node_id_to_index.entry(current_node_id) {
                let node_index = self.network_graph.add_node(NodeInfo { id: current_node_id, node_type: current_node_type });
                e.insert(node_index);
            }
            if i > 0 {
                let (prev_node_id, _) = path[i - 1];
                let (current_node_id, current_node_type) = path[i];
                if let (Some(&prev_node_idx), Some(&current_node_idx)) = (self.node_id_to_index.get(&prev_node_id), self.node_id_to_index.get(&current_node_id)) {
                    let edge_exists = self.network_graph.contains_edge(current_node_idx, prev_node_idx) || self.network_graph.contains_edge(prev_node_idx, current_node_idx);
                    if !edge_exists {
                        // println!("Client {} adding edge: {} -> {}.", self.id, prev_node_id, current_node_id);
                        self.network_graph.add_edge(prev_node_idx, current_node_idx, 0);
                        self.network_graph.add_edge(current_node_idx, prev_node_idx, 0);
                    } else {
                        //println!("Client {}: edge {} -> {} already exists.", self.id, prev_node_id, current_node_id);
                    }
                } else {
                    eprintln!("Client {} error: current node id {} not found in node_id_to_index map while processing path trace.", self.id, current_node_id);
                }
            }
        }
    }

    fn process_gui_command(&mut self, dest_id: NodeId, command_string: String) {
        println!("Client {} processing GUI command '{}' for {}.", self.id, command_string, dest_id);
        let tokens: Vec<&str> = command_string.trim().split("::").collect();
        let command_type_str = tokens.get(0).unwrap_or(&"");

        if tokens.len() >= 2 && tokens[0] == "[FloodRequired]" {
            let action = tokens[1..].join("::");
            println!("Client {} received FLOOD REQUIRED command due to action: {}.", self.id, action);
            self.log(format!("Client {} received a call to flooding the network", self.id));

            if let Some(parts) = action.strip_prefix("AddSender::") {
                let nodes: Vec<&str> = parts.splitn(2, "::").collect();
                if nodes.len() == 2 {
                    if let (Ok(a), Ok(b)) = (nodes[0].parse::<NodeId>(), nodes[1].parse::<NodeId>()) {
                        if self.id == a || self.id == b {
                            let peer = if self.id == a { b } else { a };
                            if let Some(shared) = &self.shared_senders {
                                if let Ok(map) = shared.lock() {
                                    if let Some(sender) = map.get(&(self.id, peer)) {
                                        self.packet_send.insert(peer, sender.clone());
                                        let self_idx = *self.node_id_to_index.get(&self.id).unwrap();

                                        let peer_idx = if let Some(&idx) = self.node_id_to_index.get(&peer) {
                                            idx
                                        } else {
                                            let idx = self.network_graph.add_node(NodeInfo { id: peer, node_type: NodeType::Drone });
                                            self.node_id_to_index.insert(peer, idx);
                                            idx
                                        };

                                        if self.network_graph.find_edge(self_idx, peer_idx).is_none() {
                                            self.network_graph.add_edge(self_idx, peer_idx, 0);
                                        }
                                        if self.network_graph.find_edge(peer_idx, self_idx).is_none() {
                                            self.network_graph.add_edge(peer_idx, self_idx, 0);
                                        }
                                        println!("Client {} added link to {} via AddSender", self.id, peer);
                                    }
                                }
                                self.start_flood_discovery();
                            }
                        }
                    }
                }
                return;
            }

            if let Some(parts) = action.strip_prefix("RemoveSender::") {
                let nodes: Vec<&str> = parts.splitn(2, "::").collect();
                if nodes.len() == 2 {
                    if let (Ok(a), Ok(b)) = (nodes[0].parse::<NodeId>(), nodes[1].parse::<NodeId>()) {
                        if self.id == a || self.id == b {
                            let peer = if self.id == a { b } else { a };
                            self.packet_send.remove(&peer);
                            if let (Some(&a_idx), Some(&b_idx)) = (self.node_id_to_index.get(&a), self.node_id_to_index.get(&b)) {
                                if let Some(edge_ab) = self.network_graph.find_edge(a_idx, b_idx) {
                                    self.network_graph.remove_edge(edge_ab);
                                    println!("Client {} removed link from {} to {} via RemoveSender", self.id, a, b);
                                }
                                if let Some(edge_ba) = self.network_graph.find_edge(b_idx, a_idx) {
                                    self.network_graph.remove_edge(edge_ba);
                                    println!("Client {} removed link from {} to {} (inverse) via RemoveSender", self.id, b, a);
                                }
                            }
                        }
                        self.start_flood_discovery();
                    }
                }
                return;
            }

            if let Some(parts) = action.strip_prefix("SpawnDrone::") {
                let components: Vec<&str> = parts.splitn(2, "::").collect();
                if components.len() == 2 {
                    if let Ok(drone_id) = components[0].parse::<NodeId>() {
                        if let Ok(peer_vec) = serde_json::from_str::<Vec<NodeId>>(components[1]) {
                            println!("Client {} parsing SpawnDrone with id {} and peers {:?}", self.id, drone_id, peer_vec);
                            // Instead of checking if self.id is in peer_vec, check if there’s a sender available for this drone
                            if let Some(shared) = &self.shared_senders {
                                println!("shared senders found");
                                if let Ok(map) = shared.lock() {
                                    println!("map found");

                                    if map.contains_key(&(drone_id, self.id)) || map.contains_key(&(self.id, drone_id)) {
                                        // Proceed with insertion
                                        for ((from, to), sender) in map.iter() {
                                            if *from == self.id && *to == drone_id {
                                                self.packet_send.insert(drone_id, sender.clone());
                                                println!("Client {} added sender to drone {} (from shared_senders)", self.id, drone_id);
                                            }
                                            if *to == self.id && *from == drone_id {
                                                self.packet_send.insert(drone_id, sender.clone());
                                                println!("Client {} added sender from drone {} (from shared_senders)", self.id, drone_id);
                                            }
                                        }
                                        let idx = self.network_graph.add_node(NodeInfo { id: drone_id, node_type: NodeType::Drone });
                                        self.node_id_to_index.insert(drone_id, idx);
                                    }
                                }
                            }
                            self.start_flood_discovery();

                        } else {
                            println!("Client {} failed to parse peer list in SpawnDrone: {}", self.id, components[1]);
                        }
                    } else {
                        println!("Client {} failed to parse drone ID in SpawnDrone: {}", self.id, components[0]);
                    }
                } else {
                    println!("Client {} received malformed SpawnDrone message: {}", self.id, parts);
                }
                return;
            }

            if action.starts_with("Crash::") {
                let parts: Vec<&str> = action.split("::").collect();
                if parts.len() == 2 {
                    if let Ok(crashed_id) = parts[1].parse::<NodeId>() {
                        println!("Client {} received crash signal for node {}. Cleaning up and triggering rediscovery.", self.id, crashed_id);
                        self.packet_send.remove(&crashed_id);
                        if let Some(index) = self.node_id_to_index.remove(&crashed_id) {
                            self.network_graph.remove_node(index);
                            println!("Client {} removed node {} from graph.", self.id, crashed_id);
                        } else {
                            println!("Client {} received crash for unknown node {}.", self.id, crashed_id);
                        }
                        self.start_flood_discovery();
                    } else {
                        println!("Client {} received invalid Crash ID: {}", self.id, parts[1]);
                    }
                } else {
                    println!("Client {} received malformed Crash command: {}", self.id, action);
                }
                return;
            }
            self.start_flood_discovery();
            return;
        }

        let high_level_message_info: Option<String> = match tokens.as_slice() {
            ["[Login]", server_id] => {
                if let Ok(parsed_server_id) = server_id.parse::<NodeId>() {
                    println!("Client {} processing LOGIN command for server {}.", self.id, parsed_server_id);
                    self.connected_server_id = Some(parsed_server_id);
                    self.log(format!("Login command from client: {}", self.id));
                    Some(format!("[Login]::{}",parsed_server_id))
                } else {
                    eprintln!("Client {} received LOGIN command with invalid server {}.", self.id, server_id);
                    None
                }
            },
            ["[Logout]"] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing LOGOUT command via server {}.", self.id, mem_server_id);
                    self.log(format!("Logout command from client: {}", self.id));
                    Some("[Logout]".to_string())
                } else {
                    eprintln!("Client {} received LOGOUT command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[ClientListRequest]"] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing CLIENT LIST REQUEST command via server {}.", self.id, mem_server_id);
                    self.log(format!("ClientListRequest command from client: {}", self.id));
                    Some("[ClientListRequest]".to_string())
                } else {
                    eprintln!("Client {} received CLIENT LIST REQUEST command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[MessageTo]", target_id, message_content] => {
                if let Ok(target_client_id) = target_id.parse::<NodeId>() {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing MESSAGE TO command for client {} via server {} with content: {}.", self.id, target_client_id, mem_server_id, message_content);
                        self.log(format!("Client {} sent a message to::{target_client_id} : {message_content}", self.id));
                        Some(format!("[MessageTo]::{target_client_id}::{message_content}"))
                    } else {
                        eprintln!("Client {} received MESSAGE TO command while not logged in. Ignoring.", self.id);
                        None
                    }
                } else {
                    eprintln!("Client {} received MESSAGE TO command with invalid target_id: {}.", self.id, target_id);
                    None
                }
            },
            ["[ChatRequest]", peer_id] => {
                if let Ok(_peer_id) = peer_id.parse::<NodeId>() {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing CHAT REQUEST command for peer {} via server {}.", self.id, _peer_id, mem_server_id);
                        self.log(format!("Chat Request::{_peer_id} command from client: {}", self.id));
                        Some(format!("[ChatRequest]::{_peer_id}"))
                    } else {
                        eprintln!("Client {} received CHAT REQUEST command while not logged in. Ignoring.", self.id);
                        None
                    }
                } else {
                    eprintln!("Client {} received CHAT REQUEST command with invalid peer_id: {}.", self.id, peer_id);
                    None
                }
            },
            ["[ChatFinish]", peer_id] => {
                if let Ok(_peer_id) = peer_id.parse::<NodeId>() {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing CHAT FINISH command for peer {} via server {}.", self.id, _peer_id, mem_server_id);
                        self.log(format!("ChatFinish::{_peer_id} command from client: {}", self.id));
                        Some(format!("[ChatFinish]::{_peer_id}"))
                    } else {
                        eprintln!("Client {} received CHAT FINISH command while not logged in. Ignoring.", self.id);
                        None
                    }
                } else {
                    eprintln!("Client {} received CHAT FINISH command with invalid peer_id: {}.", self.id, peer_id);
                    None
                }
            },
            ["[MediaUpload]", media_name, base64_data] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing MEDIA UPLOAD command for media '{}' via server {}.", self.id, media_name, mem_server_id);
                    self.log(format!("Client {} uploaded the '{media_name}' to the server", self.id));
                    Some(format!("[MediaUpload]::{media_name}::{base64_data}"))
                } else {
                    eprintln!("Client {} received MEDIA UPLOAD command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[MediaDownloadRequest]", media_name] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing MEDIA DOWNLOAD REQUEST command for media '{}' via server {}.", self.id, media_name, mem_server_id);
                    self.log(format!("Client {} downloaded the '{media_name}'", self.id));
                    Some(format!("[MediaDownloadRequest]::{media_name}"))
                } else {
                    eprintln!("Client {} received MEDIA DOWNLOAD REQUEST command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[HistoryRequest]", client_id, target_id] => {
                if let (Some(_client_id), Some(_target_id)) = (client_id.parse::<NodeId>().ok(), target_id.parse::<NodeId>().ok()) {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing HISTORY REQUEST command for history between {} and {} via server {}.", self.id, _client_id, _target_id, mem_server_id);
                        self.log(format!(" Client {} requested chat history between ::{_client_id}::{_target_id}", self.id));
                        Some(format!("[HistoryRequest]::{_client_id}::{_target_id}"))
                    } else {
                        eprintln!("Client {} received HISTORY REQUEST command while not logged in. Ignoring.", self.id);
                        None
                    }
                } else {
                    eprintln!("Client {} received HISTORY REQUEST command with invalid client/target id: {}.", self.id, command_string);
                    None
                }
            },
            ["[MediaListRequest]"] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing MEDIA LIST REQUEST command via server {}.", self.id, mem_server_id);
                    self.log(format!("MediaListRequest command from client: {}", self.id));
                    Some("[MediaListRequest]".to_string())
                } else {
                    eprintln!("Client {} received MEDIA LIST REQUEST command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[MediaBroadcast]", media_name, base64_data] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing MEDIA BROADCAST command for media '{}' via server {}.", self.id, media_name, mem_server_id);
                    self.log(format!("Client {} did a Media Broadcast of '{media_name}'", self.id));
                    Some(format!("[MediaBroadcast]::{media_name}::{base64_data}"))
                } else {
                    eprintln!("Client {} received MEDIA BROADCAST command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            _ => {
                eprintln!("Client {} received unrecognized GUI command: {}.", self.id, command_string);
                None
            }
        };
        //- - - - route evaluation towards destination server - - - -
        if let Some(high_level_message_content) = high_level_message_info {
            let target_dest_id_for_sending = self.connected_server_id;
            if let Some(id_to_send_to) = target_dest_id_for_sending {
                let route_option = self.best_path(self.id, id_to_send_to);
                let route = match route_option {
                    Some(path) => {
                        println!("Client {} computed route to server {}: {:?}",
                                 self.id, id_to_send_to, path);
                        self.route_cache.insert(id_to_send_to, path.clone());
                        path
                    },
                    None => {
                        eprintln!(
                            "Client {} could not compute route to server {}. Starting flood discovery.",
                            self.id, id_to_send_to
                        );
                        // ^^^ Changed to use the actual target server ID for the log

                        // Start flood discovery (only if one isn’t already active)
                        let active_flood = self.active_flood_discoveries.values().any(|state| {
                            state.initiator_id == self.id && state.start_time.elapsed() < Duration::from_secs(5)
                        });
                        if !active_flood {
                            self.start_flood_discovery();
                        }

                        // Queue the message to send after the path is discovered
                        self.pending_messages_after_flood.push((id_to_send_to, command_string));
                        // ^^^ Store the server’s NodeId (destination) instead of self.id
                        return;
                    }
                };
                let routing_header = SourceRoutingHeader {
                    hops: route.clone(),
                    hop_index: 1,
                };
                println!("♥♥♥♥ BEST PATH IS : {:?}",routing_header.hops);
                let message_data_bytes = high_level_message_content.into_bytes();
                const FRAGMENT_SIZE: usize = 128;
                let total_fragments = (message_data_bytes.len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
                let mut fragments = Vec::new();
                let session_id = {
                    let mut id_counter = SESSION_COUNTER.lock().unwrap();
                    *id_counter = id_counter.saturating_add(1);
                    *id_counter
                };
                for i in 0..total_fragments {
                    let start = i * FRAGMENT_SIZE;
                    let end = (start + FRAGMENT_SIZE).min(message_data_bytes.len());
                    let fragment_data_slice = &message_data_bytes[start..end];
                    let mut data: [u8; FRAGMENT_SIZE] = [0; FRAGMENT_SIZE];
                    data[..fragment_data_slice.len()].copy_from_slice(fragment_data_slice);
                    let len = fragment_data_slice.len();
                    fragments.push(Fragment {
                        fragment_index: i as u64,
                        total_n_fragments: total_fragments as u64,
                        length: len as u8,
                        data,
                    });
                }
                //println!("Client {} fragmented message into {} fragments for session {}", self.id, total_fragments, session_id);
                self.sent_messages.insert(session_id, SentMessageInfo {
                    fragments: fragments.clone(),
                    original_routing_header: routing_header.clone(),
                    received_ack_indices: HashSet::new(),
                    route_needs_recalculation: false,
                });
                println!("Client {} stored message info for session {}.", self.id, session_id);
                if routing_header.hops.len() > routing_header.hop_index {
                    let first_hop = routing_header.hops[routing_header.hop_index];
                    println!("Client {} sending message fragments for session {} starting with hop {}.", self.id, session_id, first_hop);
                    for fragment in fragments {
                        let packet = Packet {
                            pack_type: PacketType::MsgFragment(fragment.clone()),
                            routing_header: routing_header.clone(),
                            session_id,
                        };
                        match self.send_to_neighbor(first_hop, packet) {
                            Ok(()) => println!("Client {} sent fragment {} for session {} to {}.", self.id, fragment.fragment_index, session_id, first_hop),
                            Err(e) => eprintln!("Client {} Failed to send fragment {} for session {} to {}: {}", self.id, fragment.fragment_index, session_id, first_hop, e),
                        }
                    }
                } else {
                    eprintln!("Client {} has no valid first hop in computed route {:?} to send the message to!", self.id, route);
                    self.start_flood_discovery();
                }
                if command_type_str == &"[Logout]" {
                    println!("🚪🚪🚪Client {} successfully sent Logout message. Disconnecting internally.", self.id);
                    self.connected_server_id = None;
                }
            } else {
                eprintln!("Client {} cannot send message: not connected to any server.", self.id);
            }
        }
    }

    fn best_path(&mut self, source: NodeId, target: NodeId) -> Option<Vec<NodeId>> {
        use std::collections::{BinaryHeap, HashMap};
        use std::cmp::Reverse;
        use petgraph::visit::EdgeRef;
        use std::sync::{Arc, Mutex};

        // Edge case: invalid target
        if target == 0 {
            return None;
        }

        // Return early if source and target are the same
        if source == target {
            return Some(vec![source]);
        }

        let shared_senders = self.shared_senders.clone();

        // --- Sync net_graph with shared_senders ---
        if let Some(shared) = &shared_senders {
            let map_guard = match shared.lock() {
                Ok(guard) => guard,
                Err(poisoned) => {
                    eprintln!("⚠ Mutex for shared_senders was poisoned — recovering...");
                    let recovered = poisoned.into_inner();
                    self.shared_senders = Some(Arc::new(Mutex::new(recovered.clone())));
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
            for edge in self.network_graph.edge_references() {
                let a_id = self.network_graph[edge.source()].id;
                let b_id = self.network_graph[edge.target()].id;
                if !map_guard.contains_key(&(a_id, b_id)) && !map_guard.contains_key(&(b_id, a_id)) {
                    to_remove.push((edge.source(), edge.target()));
                }
            }

            for (src, dst) in to_remove {
                if let Some(edge_idx) = self.network_graph.find_edge(src, dst) {
                    self.network_graph.remove_edge(edge_idx);
                }
            }
        }

        // --- Get node indices ---
        let source_idx = *self.node_id_to_index.get(&source)?;
        let target_idx = *self.node_id_to_index.get(&target)?;

        let mut distances: HashMap<NodeIndex, u32> = self.network_graph.node_indices()
            .map(|idx| (idx, u32::MAX))
            .collect();
        let mut predecessors: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        let mut heap = BinaryHeap::new();

        distances.insert(source_idx, 0);
        heap.push(Reverse((0, source_idx)));

        while let Some(Reverse((current_dist, current_node))) = heap.pop() {
            if current_dist > distances[&current_node] {
                continue;
            }

            if current_node == target_idx {
                break;
            }

            for edge in self.network_graph.edges(current_node) {
                let neighbor_idx = edge.target();
                let weight = *edge.weight() as u32;

                let can_use_neighbor = if neighbor_idx == target_idx {
                    true
                } else if neighbor_idx == source_idx {
                    false
                } else {
                    match self.network_graph.node_weight(neighbor_idx) {
                        Some(NodeInfo { node_type: NodeType::Drone, .. }) => true,
                        Some(_) => false,
                        None => {
                            warn!("❌ Node index {:?} has no type in graph", neighbor_idx);
                            false
                        }
                    }
                };

                if !can_use_neighbor {
                    continue;
                }

                let alt_dist = current_dist.saturating_add(weight);
                if alt_dist < distances[&neighbor_idx] {
                    distances.insert(neighbor_idx, alt_dist);
                    predecessors.insert(neighbor_idx, current_node);
                    heap.push(Reverse((alt_dist, neighbor_idx)));
                }
            }
        }

        if distances.get(&target_idx)? == &u32::MAX {
            return None;
        }

        let mut path = Vec::new();
        let mut current = target_idx;

        loop {
            let node_id = self.network_graph.node_weight(current)?.id;
            path.push(node_id);

            if current == source_idx {
                break;
            }

            current = *predecessors.get(&current)?;
        }

        path.reverse();
        Some(path)
    }


    fn increment_drop(&mut self, from: NodeId, to: NodeId) {
        println!("Client {} incrementing drop count for link {} -> {}.", self.id, from, to);
        let from_node_idx = self.node_id_to_index.get(&from).copied();
        let to_node_idx = self.node_id_to_index.get(&to).copied();
        if let (Some(from_idx), Some(to_idx)) = (from_node_idx, to_node_idx) {
            if let Some(edge_index) = self.network_graph.find_edge(from_idx, to_idx) {
                let current_weight = *self.network_graph.edge_weight(edge_index).unwrap();
                *self.network_graph.edge_weight_mut(edge_index).unwrap() = current_weight.saturating_add(1);
                println!("Client {} incremented weight for link {} -> {} to {}.", self.id, from, to, current_weight.saturating_add(1));
            } else {
                println!("Client {} cannot increment drop: link {} -> {} not found in graph.", self.id, from, to);
            }
        } else {
            println!("Client {} cannot increment drop: one or both nodes ({}, {}) not found in map.", self.id, from, to);
        }
    }

    fn remove_node_from_graph(&mut self, node_id: NodeId) {
        if let Some(node_index) = self.node_id_to_index.remove(&node_id) {
            self.network_graph.remove_node(node_index);
            println!("Client {} removed node {} (index {:?}) from graph.", self.id, node_id, node_index);
        } else {
            println!("Client {} tried to remove non-existent node {} from graph.", self.id, node_id);
        }
    }

    fn get_node_info(&self, node_id: NodeId) -> Option<&NodeInfo> {
        self.node_id_to_index.get(&node_id).and_then(|&idx| self.network_graph.node_weight(idx))
    }

    fn send_to_neighbor(&mut self, neighbor_id: NodeId, packet: Packet) -> Result<(), String> {
        if let Some(sender) = self.packet_send.get(&neighbor_id) {
            sender
                .send(packet.clone())
                .map_err(|e| format!("Failed to send packet to neighbor {}: {}", neighbor_id, e))
        } else if let Some(shared) = &self.shared_senders {
            if let Ok(map) = shared.lock() {
                if let Some(shared_sender) = map.get(&(self.id, neighbor_id)) {
                    return shared_sender
                        .send(packet.clone())
                        .map_err(|e| format!("Failed to send packet to neighbor {} (from shared): {}", neighbor_id, e));
                }
            }
            Err(format!("Neighbor {} not found in shared_senders", neighbor_id))
        } else {
            Err(format!("Neighbor {} not found in local or shared map", neighbor_id))
        }
    }
}


