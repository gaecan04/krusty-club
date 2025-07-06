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
use log::{info, warn, error};
use petgraph::visit::{IntoEdgeReferences};
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

        info!("🔗 🔗 🔗  Client {} starting run loop", self.id);

        if self.network_graph.node_count() == 0 {
            info!("Client {} network graph is empty, starting flood discovery", self.id);
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
                    info!("Checking for received packet by client {} ...", self.id);
                    if let Ok(packet) = packet {
                        info!("❤ ❤ ❤  Packet received by client {} : {:?}", self.id, packet);
                        self.process_packet(packet);
                    }
                },
                recv(self.shortcut_receiver.as_ref().unwrap()) -> packet => {
                    if let Ok(packet) = packet {
                        info!("Client {} received shortcut packet: {:?}", self.id, packet);
                        self.process_packet(packet);
                    }
                }

                default => {
                        std::thread::sleep(Duration::from_millis(1));
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
            info!("Client {} finalizing flood discovery for ID {} due to timeout",
                     self.id, flood_id);
            self.finalize_flood_discovery_topology(flood_id);
        }

        // After updating topology, attempt to send any pending messages
        if has_floods && !self.pending_messages_after_flood.is_empty() {
            info!("Client {} processing {} pending message(s) after flood",
                     self.id, self.pending_messages_after_flood.len());

            // Drain the pending messages to avoid double-processing
            let pending_list = std::mem::take(&mut self.pending_messages_after_flood);
            for (dest, pending_cmd) in pending_list {
                // Recompute the best path to the destination with updated graph
                if let Some(path) = self.best_path(self.id, dest) {
                    info!("Client {} computed path to {} after flood: {:?}",
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
                                Ok(_) => info!(
                                    "Client {} sent pending fragment {} (session {}) to {} after flood",
                                    self.id, fragment.fragment_index, session_id, first_hop
                                ),
                                Err(e) => warn!(
                                    "Client {} failed to send pending fragment {} (session {}) to {}: {}",
                                    self.id, fragment.fragment_index, session_id, first_hop, e
                                ),
                            }
                        }
                    } else {
                        error!("Client {} cannot send pending message: no next hop in route to {}",
                                  self.id, dest);
                    }

                    // If the pending command was a logout, update client state
                    if pending_cmd.starts_with("[Logout]") {
                        info!("🚪 🚪 🚪  Client {} logout completed after flood. Disconnected from server {}",
                                 self.id, dest);
                        self.connected_server_id = None;
                    }
                } else {
                    warn!("Client {} still cannot find route to {} after flood; dropping message '{}'",
                              self.id, dest, pending_cmd);
                    warn!("Please add a sender such that it connects the client to the server");
                }
            }
        }
    }


    fn finalize_flood_discovery_topology(&mut self, flood_id: u64) {
        if let Some(discovery_state) = self.active_flood_discoveries.remove(&flood_id) {
            info!("Client {} processing {} collected responses for flood ID {}", self.id, discovery_state.received_responses.len(), flood_id);
            for response in discovery_state.received_responses {
                self.create_topology(&response);
            }
        } else {
            warn!("Client {} tried to finalize unknown flood discovery ID {}", self.id, flood_id);
        }
    }

    fn process_packet(&mut self, mut packet: Packet) {
        let packet_type = packet.pack_type.clone();
        match packet_type {
            PacketType::FloodRequest(request) => { //CONTROLLA SE IL CLIENT PUò PROPAGARE FLOODREQUESTS
                //println!("Client {} received FloodRequest {}, ignoring as client should not propagate", self.id, request.flood_id);
                self.process_flood_request(&request, packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                info!("Client {} received FloodResponse for flood_id {}", self.id, response.flood_id);
                if let Some(discovery_state) = self.active_flood_discoveries.get_mut(&response.flood_id) {
                    discovery_state.received_responses.push(response.clone());
                    info!("Client {} collected {} responses for flood ID {}", self.id, discovery_state.received_responses.len(), response.flood_id);
                } else {
                    warn!("Client {} received FloodResponse for unknown or inactive flood ID {}. Processing path anyway", self.id, response.flood_id);
                    self.create_topology(&response);
                }
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut packet, &fragment);
                self.reassemble_packet(&fragment, &mut packet);
            },
            PacketType::Ack(ack) => {
                info!("Client {} received ACK for session {}, fragment {}", self.id, packet.session_id, ack.fragment_index);
                if let Some(sent_msg_info) = self.sent_messages.get_mut(&packet.session_id) {
                    sent_msg_info.received_ack_indices.insert(ack.fragment_index);
                    info!("Client {} marked fragment {} of session {} as ACKed", self.id, ack.fragment_index, packet.session_id);
                    if sent_msg_info.received_ack_indices.len() == sent_msg_info.fragments.len() {
                        info!("✅ ✅ ✅  Client {} received all ACKs for session {}. Message considered successfully sent", self.id, packet.session_id);
                        //self.sent_messages.remove(&packet.session_id);
                    }
                } else {
                    warn!("Client {} received ACK for unknown session {}", self.id, packet.session_id);
                }
            },
            PacketType::Nack(nack) => {
                self.process_nack(&nack, &mut packet);
            }
        }
    }

    fn process_flood_request(&mut self, request: &FloodRequest, _header: SourceRoutingHeader) {
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
                if let Some(_sender) = self.packet_send.get(&prev_hop_id) {
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
                                Ok(()) => info!("↩️ ↩️ ↩️  Client {} sent FloodResponse {} back to {} (prev hop)", self.id, request.flood_id, prev_hop_id),
                                Err(e) => warn!("Client {} failed to send FloodResponse {} back to {}: {}", self.id, request.flood_id, prev_hop_id, e),
                            }
                        } else {
                            error!("Client {} error: no sender found for previous hop {}", self.id, prev_hop_id);
                        }
                    } else {
                        error!("Client {} cannot send FloodResponse back, path trace too short after adding self", self.id);
                    }
                } else {
                    error!("Client {} error: could not find sender channel for previous hop {}", self.id, prev_hop_id);
                }
            } else {
                warn!("Client {} received FloodRequest with empty path trace or no sender info", self.id);
            }
        } else {
            self.seen_flood_ids.insert(flood_identifier);
            let mut updated_request = request.clone();
            updated_request.path_trace.push((self.id, NodeType::Client));
            info!("Client {} processed FloodRequest {}. Path trace now: {:?}", self.id, request.flood_id, updated_request.path_trace);
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
                        Ok(_) => info!("⏩ ⏩ ⏩  Client {} forwarded FloodRequest {} to neighbor {}", self.id, request.flood_id, neighbor_id),
                        Err(e) => warn!("Client {} failed to forward FloodRequest {} to neighbor {}: {}", self.id, request.flood_id, neighbor_id, e),
                    }
                }
            }
        }
    }

    fn start_flood_discovery(&mut self) {
        info!("🦋 🦋 🦋  Client {} starting flood discovery", self.id);
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

        info!("♥ ♥ ♥  client {} has senders towards this drones: {:?}", self.id, self.packet_send);


        info!("🌊 🌊 🌊  Client {} sending FloodRequest {} to all neighbors", self.id, new_flood_id);
        //4.sending flood_request to all neighbors
        for (&neighbor_id, sender) in &self.packet_send {
            match sender.send(flood_packet.clone()) {
                Ok(_) =>  info!("Client {} sent FloodRequest to neighbor {}", self.id, neighbor_id),
                Err(e) => warn!("Client {} failed to send FloodRequest to neighbor {}: {}", self.id, neighbor_id, e),
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
            error!("Client {} error: Received packet with empty hops in routing header for session {}", self.id, session_id);
            0
        });
        let key = session_id;
        let fragment_len = fragment.length as usize;
        let offset = (fragment.fragment_index * 128) as usize;
        let state = self.received_messages.entry(key).or_insert_with(|| {
            info!("Client {} starting reassembly for session {} from source {}", self.id, session_id, src_id);
            ReceivedMessageState {
                data: vec![0u8; (fragment.total_n_fragments * 128) as usize],
                total_fragments: fragment.total_n_fragments,
                received_indices: HashSet::new(),
                last_fragment_len: None,
            }
        });
        if state.received_indices.contains(&fragment.fragment_index) {
            info!("Received duplicate fragment {} for session {}. Ignoring", fragment.fragment_index, session_id);
            return;
        }
        if fragment.fragment_index == fragment.total_n_fragments - 1 {
            state.last_fragment_len = Some(fragment.length);
        }

        if offset + fragment_len <= state.data.len() {
            state.data[offset..offset + fragment_len].copy_from_slice(&fragment.data[..fragment_len]);
            state.received_indices.insert(fragment.fragment_index);

            info!("Client {} received fragment {} for session {}", self.id, fragment.fragment_index, session_id);

            if state.received_indices.len() as u64 == state.total_fragments {
                info!("🧩 🧩 🧩  Message for session {} reassembled successfully", session_id);
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
            warn!("Received fragment {} for session {} with offset {} and length {} which exceeds excepted message size {}. Ignoring fragment", fragment.fragment_index, session_id, offset, fragment_len, state.data.len());
        }
    }

    //function in which the correctly reassembled high level messages are elaborated
    fn process_received_high_level_message(&mut self, message_string: String, _source_id: NodeId, session_id: u64) {
        //println!("Client {} processing high-level message for session {} from source {}: {}", self.id, session_id, source_id, message_string);
        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();

        if tokens.is_empty() {
            warn!("Client {} received empty high-level message for session {}", self.id, session_id);
            return;
        }

        match tokens.as_slice() {
            ["[MessageFrom]", sender_id_str, content_str] => {
                // format: [MessageFrom]::sender_id::message_content
                if let Ok(sender_id) = sender_id_str.parse::<NodeId>() {
                    let content = *content_str;
                    info!("Client {} received chat message from client {}", self.id, sender_id);
                } else {
                    warn!("Client {} received MessageFrom with invalid sender_id: {}", self.id, sender_id_str);
                }
            },
            ["[ClientListResponse]", list_str] => {
                //format: [ClientListResponse]::[client1_id, client2_id, ...]
                let client_ids: Vec<NodeId> = list_str.trim_start_matches('[').trim_end_matches(']').split(',').filter(|s| !s.trim().is_empty()).filter_map(|s| s.trim().parse::<NodeId>().ok()).collect();
                info!("Client {} received CLIENT LIST: {:?}", self.id, client_ids);
            },
            ["[ChatStart]", status_str] => {
                //format: [ChatStart]::bool
                match status_str.to_lowercase().as_str() {
                    "true" => {
                        info!("Client {} CHAT REQUEST accepted. Chat started", self.id);
                    },
                    "false" => {
                        info!("Client {} CHAT REQUEST denied", self.id);
                    },
                    _ => warn!("Client {} received ChatStart with invalid boolean value: {}", self.id, status_str),
                }
            },
            ["[ChatFinish]"] => {
                //format: [ChatFinish]
                info!("Client {} CHAT TERMINATED", self.id);
            },
            ["[ChatRequest]", peer_id_str] => {
                //format: [ChatRequest]::{peer_id}
                if let Ok(requester_id) = peer_id_str.parse::<NodeId>() {
                    info!("Client {} received incoming CHAT REQUEST from client {}", self.id, requester_id);
                } else {
                    warn!("Client {} received ChatRequest with invalid requester_id: {}", self.id, peer_id_str);
                }
            },
            ["[HistoryResponse]", content_str] => {
                //format: [HistoryResponse]::message_content
                let history_content = *content_str;
                info!("Client {} received CHAT HISTORY: {}", self.id, history_content);
            },
            ["[MediaUploadAck]", media] => {
                //format: [MediaUploadAck]::media_name
                if tokens.len() >= 2 {
                    let media_name = media;
                    info!("Client {} received MEDIA UPLOAD ACK for media '{}'.", self.id, media_name);
                } else {
                    warn!("Client {} received invalid MEDIA UPLOAD ACK format: {}.", self.id, message_string);
                }
            },
            ["[MediaDownloadResponse]", media, base64] => {
                if tokens.len() >= 3 {
                    let media_name = media;
                    let base64_data = base64;

                    if media_name.to_string() == "ERROR" && base64_data.to_string() == "NotFound" {
                        info!("Client {} received MEDIA DOWNLOAD RESPONSE: Media not found.", self.id)
                    } else {
                        info!("Client {} received MEDIA DOWNLOAD RESPONSE for media '{}'.", self.id, media_name);

                        match STANDARD.decode(&base64_data) {
                            Ok(image_bytes) => {
                                match load_from_memory(&image_bytes) {
                                    Ok(img) => {
                                        info!("Client {} successfully loaded image data for media '{}'.", self.id, media_name);

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
                                            warn!("Client {} failed to temporarily save image: {}", self.id, e);
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
                                            Ok(_) => info!("🖼️ 🖼️ 🖼️ Opened media '{}' for client {}", media_name, self.id),
                                            Err(e) => error!("Failed to open image: {}", e),
                                        }
                                    },
                                    Err(e) => error!("Client {} failed to load image from memory: {}", self.id, e),
                                }
                            },
                            Err(e) => error!("Client {} failed to decode base64 data: {}", self.id, e),
                        }
                    }
                }
            },
            ["[MediaListResponse]", list_str] => {
                info!("Client {} received MEDIA LIST: {}.", self.id, list_str);
                let media_list : Vec<String> = list_str.split(',').filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
                info!("Available media files: {:?}", media_list);
            },

            ["[LoginAck]", session_id] => {
                //format: [LoginAck]::session_id
                if let Ok(parsed_session_id) = session_id.parse::<u64>() {
                    info!("🔑 🔑 🔑  Client {} received LOGIN ACK for session {}. Successfully logged in!", self.id, parsed_session_id);
                } else {
                    warn!("Client {} received LOGIN ACK with invalid session ID: {}", self.id, session_id);
                }
            },
            [type_tag, ..] => {
                warn!("Client {} received unrecognized high-level message type: {}", self.id, type_tag);
            },
            [] => {
                warn!("Client {} received empty high-level message for session {}", self.id, session_id);
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
                Ok(_) => info!("👍 👍 👍  Client {} sent ACK for session {} fragment {} to neighbor {}", self.id, packet.session_id, fragment.fragment_index, next_hop_for_ack),
                Err(e) => warn!("Client {} error sending ACK packet for session {} to neighbor {}: {}. Using ControllerShortcut", self.id, packet.session_id, next_hop_for_ack, e),

            }
        } else {
            error!("Client {} error: cannot send ACK, original routing header hops too short: {:?}. Cannot use ControllerShortcut", self.id, packet.routing_header.hops);
        }
    }

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        info!("🚨 🚨 🚨  Client {} processing NACK type {:?} for session {}", self.id, nack.nack_type, packet.session_id);
        match &nack.nack_type {
            NackType::ErrorInRouting(problem_node_id) => {
                info!("Client {} received ErrorInRouting NACK for session {} at node {}", self.id, packet.session_id, problem_node_id);

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
                info!("Client {} received DestinationIsDrone NACK for session {}", self.id, packet.session_id);
                if let Some(info) = self.sent_messages.get_mut(&packet.session_id) {
                    info.route_needs_recalculation = true;
                    info!("Client {} marked route for session {} for recalculation due to DestinationIsDrone", self.id, packet.session_id);
                    if let Some(&dest_id) = info.original_routing_header.hops.last() {
                        self.route_cache.remove(&dest_id);
                        info!("Client {} invaliding cached route for {} due to DestinationIsDrone NACK", self.id, dest_id);
                    }
                } else {
                    warn!("Client {} received DestinationIsDrone NACK for unknown session {}", self.id, packet.session_id);
                }
            }
            NackType::UnexpectedRecipient(received_at_node_id) => {
                info!("Client {} received UnexpectedRecipient NACK for session {}: packet arrived at node {} but expected a different one", self.id, packet.session_id, received_at_node_id);
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
                info!("Client {} received Dropped NACK for session {}, fragment {}", self.id, packet.session_id, nack.fragment_index);
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
                            Ok(_) => info!("🔁 🔁 🔁  Client {} resent fragment {} for session {} to {}", self.id, fragment.fragment_index, packet.session_id, first_hop),
                            Err(e) => warn!("Client {} failed to resend fragment {} for session {} to {}: {}", self.id, fragment.fragment_index, packet.session_id, first_hop, e),
                        }
                    } else {
                        error!("Client {} cannot resend fragment {}: invalid hop index in original route", self.id, fragment.fragment_index);
                    }
                } else {
                    warn!("Client {} could not find original fragment {} for session {}", self.id, nack.fragment_index, packet.session_id);
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
                info!("Client {} removed edge {} → {} from graph", self.id, from, to);
                removed = true;
            }

            if let Some(edge) = self.network_graph.find_edge(to_idx, from_idx) {
                self.network_graph.remove_edge(edge);
                info!("Client {} removed edge {} → {} from graph", self.id, to, from);
                removed = true;
            }

            if !removed {
                info!("Client {}: No link existed between {} and {}", self.id, from, to);
            }
        } else {
            info!(
                "Client {}: Cannot remove link — one or both nodes missing: {} or {}",
                self.id, from, to
            );
        }
    }

    fn create_topology(&mut self, flood_response: &FloodResponse) {
        //println!("Client {} processing FloodResponse for flood_id {}", self.id, flood_response.flood_id);
        let path = &flood_response.path_trace;
        if path.is_empty() {
            info!("Received empty path_trace in FloodResponse for flood_id {}", flood_response.flood_id);
            return;
        }
        let (initiator_id, initiator_type) = path.first().cloned().unwrap();
        if let std::collections::hash_map::Entry::Vacant(e) = self.node_id_to_index.entry(initiator_id) {
            let node_index = self.network_graph.add_node(NodeInfo { id: initiator_id, node_type: initiator_type });
            e.insert(node_index);
            //println!("Client {} added initiator node {} ({:?}) to graph", self.id, initiator_id, initiator_type);
        }
        for i in 0..path.len() {
            let (current_node_id, current_node_type) = path[i];
            if let std::collections::hash_map::Entry::Vacant(e) = self.node_id_to_index.entry(current_node_id) {
                let node_index = self.network_graph.add_node(NodeInfo { id: current_node_id, node_type: current_node_type });
                e.insert(node_index);
            }
            if i > 0 {
                let (prev_node_id, _) = path[i - 1];
                let (current_node_id, _current_node_type) = path[i];
                if let (Some(&prev_node_idx), Some(&current_node_idx)) = (self.node_id_to_index.get(&prev_node_id), self.node_id_to_index.get(&current_node_id)) {
                    let edge_exists = self.network_graph.contains_edge(current_node_idx, prev_node_idx) || self.network_graph.contains_edge(prev_node_idx, current_node_idx);
                    if !edge_exists {
                        // println!("Client {} adding edge: {} -> {}", self.id, prev_node_id, current_node_id);
                        self.network_graph.add_edge(prev_node_idx, current_node_idx, 0);
                        self.network_graph.add_edge(current_node_idx, prev_node_idx, 0);
                    } else {
                        //println!("Client {}: edge {} -> {} already exists", self.id, prev_node_id, current_node_id);
                    }
                } else {
                    error!("Client {} error: current node id {} not found in node_id_to_index map while processing path trace", self.id, current_node_id);
                }
            }
        }
    }

    fn process_gui_command(&mut self, dest_id: NodeId, command_string: String) {
        info!("Client {} processing GUI command '{}' for {}", self.id, command_string, dest_id);
        let tokens: Vec<&str> = command_string.trim().split("::").collect();
        let command_type_str = tokens.get(0).unwrap_or(&"");

        if tokens.len() >= 2 && tokens[0] == "[FloodRequired]" {
            let action = tokens[1..].join("::");
            info!("Client {} received FLOOD REQUIRED command due to action: {}", self.id, action);
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
                                        info!("Client {} added link to {} via AddSender", self.id, peer);
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
                            self.remove_link_from_graph(a, b);
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
                            info!("Client {} parsing SpawnDrone with id {} and peers {:?}", self.id, drone_id, peer_vec);
                            // Instead of checking if self.id is in peer_vec, check if there’s a sender available for this drone
                            if let Some(shared) = &self.shared_senders {
                                info!("shared senders found");
                                if let Ok(map) = shared.lock() {
                                    info!("map found");

                                    if map.contains_key(&(drone_id, self.id)) || map.contains_key(&(self.id, drone_id)) {
                                        // Proceed with insertion
                                        for ((from, to), sender) in map.iter() {
                                            if *from == self.id && *to == drone_id {
                                                self.packet_send.insert(drone_id, sender.clone());
                                                info!("Client {} added sender to drone {} (from shared_senders)", self.id, drone_id);
                                            }
                                            if *to == self.id && *from == drone_id {
                                                self.packet_send.insert(drone_id, sender.clone());
                                                info!("Client {} added sender from drone {} (from shared_senders)", self.id, drone_id);
                                            }
                                        }
                                        let idx = self.network_graph.add_node(NodeInfo { id: drone_id, node_type: NodeType::Drone });
                                        self.node_id_to_index.insert(drone_id, idx);
                                    }
                                }
                            }
                            self.start_flood_discovery();

                        } else {
                            warn!("Client {} failed to parse peer list in SpawnDrone: {}", self.id, components[1]);
                        }
                    } else {
                        warn!("Client {} failed to parse drone ID in SpawnDrone: {}", self.id, components[0]);
                    }
                } else {
                    warn!("Client {} received malformed SpawnDrone message: {}", self.id, parts);
                }
                return;
            }

            if action.starts_with("Crash::") {
                let parts: Vec<&str> = action.split("::").collect();
                if parts.len() == 2 {
                    if let Ok(crashed_id) = parts[1].parse::<NodeId>() {
                        info!("Client {} received crash signal for node {}. Cleaning up and triggering rediscovery", self.id, crashed_id);
                        self.packet_send.remove(&crashed_id);
                        if let Some(index) = self.node_id_to_index.remove(&crashed_id) {
                            self.network_graph.remove_node(index);
                            info!("Client {} removed node {} from graph", self.id, crashed_id);
                        } else {
                            warn!("Client {} received crash for unknown node {}", self.id, crashed_id);
                        }
                        self.start_flood_discovery();
                    } else {
                        warn!("Client {} received invalid Crash ID: {}", self.id, parts[1]);
                    }
                } else {
                    warn!("Client {} received malformed Crash command: {}", self.id, action);
                }
                return;
            }
            self.start_flood_discovery();
            return;
        }

        let high_level_message_info: Option<String> = match tokens.as_slice() {
            ["[Login]", server_id] => {
                if let Ok(parsed_server_id) = server_id.parse::<NodeId>() {
                    info!("Client {} processing LOGIN command for server {}", self.id, parsed_server_id);
                    self.connected_server_id = Some(parsed_server_id);
                    self.log(format!("Login command from client: {}", self.id));
                    Some(format!("[Login]::{}",parsed_server_id))
                } else {
                    warn!("Client {} received LOGIN command with invalid server {}", self.id, server_id);
                    None
                }
            },
            ["[Logout]"] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    info!("Client {} processing LOGOUT command via server {}", self.id, mem_server_id);
                    self.log(format!("Logout command from client: {}", self.id));
                    Some("[Logout]".to_string())
                } else {
                    info!("Client {} received LOGOUT command while not logged in. Ignoring", self.id);
                    None
                }
            },
            ["[ClientListRequest]"] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    info!("Client {} processing CLIENT LIST REQUEST command via server {}", self.id, mem_server_id);
                    self.log(format!("ClientListRequest command from client: {}", self.id));
                    Some("[ClientListRequest]".to_string())
                } else {
                    info!("Client {} received CLIENT LIST REQUEST command while not logged in. Ignoring", self.id);
                    None
                }
            },
            ["[MessageTo]", target_id, message_content] => {
                if let Ok(target_client_id) = target_id.parse::<NodeId>() {
                    if let Some(mem_server_id) = self.connected_server_id {
                        info!("Client {} processing MESSAGE TO command for client {} via server {} with content: {}", self.id, target_client_id, mem_server_id, message_content);
                        self.log(format!("Client {} sent a message to::{target_client_id} : {message_content}", self.id));
                        Some(format!("[MessageTo]::{target_client_id}::{message_content}"))
                    } else {
                        info!("Client {} received MESSAGE TO command while not logged in. Ignoring", self.id);
                        None
                    }
                } else {
                    warn!("Client {} received MESSAGE TO command with invalid target_id: {}", self.id, target_id);
                    None
                }
            },
            ["[ChatRequest]", peer_id] => {
                if let Ok(_peer_id) = peer_id.parse::<NodeId>() {
                    if let Some(mem_server_id) = self.connected_server_id {
                        info!("Client {} processing CHAT REQUEST command for peer {} via server {}", self.id, _peer_id, mem_server_id);
                        self.log(format!("Chat Request::{_peer_id} command from client: {}", self.id));
                        Some(format!("[ChatRequest]::{_peer_id}"))
                    } else {
                        info!("Client {} received CHAT REQUEST command while not logged in. Ignoring", self.id);
                        None
                    }
                } else {
                    warn!("Client {} received CHAT REQUEST command with invalid peer_id: {}", self.id, peer_id);
                    None
                }
            },
            ["[ChatFinish]", peer_id] => {
                if let Ok(_peer_id) = peer_id.parse::<NodeId>() {
                    if let Some(mem_server_id) = self.connected_server_id {
                        info!("Client {} processing CHAT FINISH command for peer {} via server {}", self.id, _peer_id, mem_server_id);
                        self.log(format!("ChatFinish::{_peer_id} command from client: {}", self.id));
                        Some(format!("[ChatFinish]::{_peer_id}"))
                    } else {
                        info!("Client {} received CHAT FINISH command while not logged in. Ignoring", self.id);
                        None
                    }
                } else {
                    warn!("Client {} received CHAT FINISH command with invalid peer_id: {}", self.id, peer_id);
                    None
                }
            },
            ["[MediaUpload]", media_name, base64_data] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    info!("Client {} processing MEDIA UPLOAD command for media '{}' via server {}.", self.id, media_name, mem_server_id);
                    self.log(format!("Client {} uploaded the '{media_name}' to the server", self.id));
                    Some(format!("[MediaUpload]::{media_name}::{base64_data}"))
                } else {
                    info!("Client {} received MEDIA UPLOAD command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[MediaDownloadRequest]", media_name] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    info!("Client {} processing MEDIA DOWNLOAD REQUEST command for media '{}' via server {}.", self.id, media_name, mem_server_id);
                    self.log(format!("Client {} downloaded the '{media_name}'", self.id));
                    Some(format!("[MediaDownloadRequest]::{media_name}"))
                } else {
                    info!("Client {} received MEDIA DOWNLOAD REQUEST command while not logged in. Ignoring.", self.id);
                    None
                }
            },

            ["[HistoryRequest]", client_id, target_id] => {
                if let (Some(_client_id), Some(_target_id)) = (client_id.parse::<NodeId>().ok(), target_id.parse::<NodeId>().ok()) {
                    if let Some(mem_server_id) = self.connected_server_id {
                        info!("Client {} processing HISTORY REQUEST command for history between {} and {} via server {}", self.id, _client_id, _target_id, mem_server_id);
                        self.log(format!(" Client {} requested chat history between ::{_client_id}::{_target_id}", self.id));
                        Some(format!("[HistoryRequest]::{_client_id}::{_target_id}"))
                    } else {
                        info!("Client {} received HISTORY REQUEST command while not logged in. Ignoring", self.id);
                        None
                    }
                } else {
                    warn!("Client {} received HISTORY REQUEST command with invalid client/target id: {}", self.id, command_string);
                    None
                }
            },
            ["[MediaListRequest]"] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    info!("Client {} processing MEDIA LIST REQUEST command via server {}.", self.id, mem_server_id);
                    self.log(format!("MediaListRequest command from client: {}", self.id));
                    Some("[MediaListRequest]".to_string())
                } else {
                    info!("Client {} received MEDIA LIST REQUEST command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[MediaBroadcast]", media_name, base64_data] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    info!("Client {} processing MEDIA BROADCAST command for media '{}' via server {}.", self.id, media_name, mem_server_id);
                    self.log(format!("Client {} did a Media Broadcast of '{media_name}'", self.id));
                    Some(format!("[MediaBroadcast]::{media_name}::{base64_data}"))
                } else {
                    info!("Client {} received MEDIA BROADCAST command while not logged in. Ignoring.", self.id);
                    None
                }
            },

            _ => {
                warn!("Client {} received unrecognized GUI command: {}", self.id, command_string);
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
                        info!("Client {} computed route to server {}: {:?}",
                                 self.id, id_to_send_to, path);
                        self.route_cache.insert(id_to_send_to, path.clone());
                        path
                    },
                    None => {
                        warn!(
                            "Client {} could not compute route to server {}. Starting flood discovery",
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
                info!("♥♥♥♥ BEST PATH IS : {:?}",routing_header.hops);
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
                info!("Client {} stored message info for session {}", self.id, session_id);
                if routing_header.hops.len() > routing_header.hop_index {
                    let first_hop = routing_header.hops[routing_header.hop_index];
                    info!("Client {} sending message fragments for session {} starting with hop {}", self.id, session_id, first_hop);
                    for fragment in fragments {
                        let packet = Packet {
                            pack_type: PacketType::MsgFragment(fragment.clone()),
                            routing_header: routing_header.clone(),
                            session_id,
                        };
                        match self.send_to_neighbor(first_hop, packet) {
                            Ok(()) => info!("Client {} sent fragment {} for session {} to {}", self.id, fragment.fragment_index, session_id, first_hop),
                            Err(e) => warn!("Client {} failed to send fragment {} for session {} to {}: {}", self.id, fragment.fragment_index, session_id, first_hop, e),
                        }
                    }
                } else {
                    error!("Client {} has no valid first hop in computed route {:?} to send the message to!", self.id, route);
                    self.start_flood_discovery();
                }
                if command_type_str == &"[Logout]" {
                    info!("🚪 🚪 🚪  Client {} successfully sent Logout message. Disconnecting internally", self.id);
                    self.connected_server_id = None;
                }
            } else {
                info!("Client {} cannot send message: not connected to any server", self.id);
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
                    error!("⚠ ⚠ ⚠  Mutex for shared_senders was poisoned — recovering...");
                    let recovered = poisoned.into_inner();
                    self.shared_senders = Some(Arc::new(Mutex::new(recovered.clone())));
                    match self.shared_senders.as_ref().unwrap().lock() {
                        Ok(guard) => guard,
                        Err(_) => {
                            warn!("❌ ❌ ❌  Failed to recover shared_senders after poison");
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
                            warn!("❌ ❌ ❌  Node index {:?} has no type in graph", neighbor_idx);
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
        info!("Client {} incrementing drop count for link {} -> {}", self.id, from, to);
        let from_node_idx = self.node_id_to_index.get(&from).copied();
        let to_node_idx = self.node_id_to_index.get(&to).copied();
        if let (Some(from_idx), Some(to_idx)) = (from_node_idx, to_node_idx) {
            if let Some(edge_index) = self.network_graph.find_edge(from_idx, to_idx) {
                let current_weight = *self.network_graph.edge_weight(edge_index).unwrap();
                *self.network_graph.edge_weight_mut(edge_index).unwrap() = current_weight.saturating_add(1);
                info!("Client {} incremented weight for link {} -> {} to {}", self.id, from, to, current_weight.saturating_add(1));
            } else {
                warn!("Client {} cannot increment drop: link {} -> {} not found in graph", self.id, from, to);
            }
        } else {
            warn!("Client {} cannot increment drop: one or both nodes ({}, {}) not found in map", self.id, from, to);
        }
    }

    fn remove_node_from_graph(&mut self, node_id: NodeId) {
        if let Some(node_index) = self.node_id_to_index.remove(&node_id) {
            self.network_graph.remove_node(node_index);
            info!("Client {} removed node {} (index {:?}) from graph", self.id, node_id, node_index);
        } else {
            info!("Client {} tried to remove non-existent node {} from graph", self.id, node_id);
        }
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



#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{unbounded, Sender, Receiver};
    use std::collections::{HashMap, HashSet};
    use wg_2024::network::{NodeId, SourceRoutingHeader};
    use wg_2024::packet::{FloodRequest, Packet, PacketType, NodeType as PktNodeType};
    use petgraph::stable_graph::{StableGraph, NodeIndex};
    use std::time::{Instant, Duration};
    use crate::simulation_controller::gui_input_queue::new_gui_input_queue;

    fn has_edge(graph: &StableGraph<NodeInfo, usize>, a: NodeIndex, b: NodeIndex) -> bool {
        graph.contains_edge(a, b)
    }

    fn reassemble_fragments(mut packets: Vec<Packet>) -> String {
        if packets.is_empty() {
            return String::new();
        }

        packets.sort_by_key(|p| match &p.pack_type {
            PacketType::MsgFragment(f) => f.fragment_index,
            _ => 0,
        });

        let mut reassembled_data = Vec::new();
        let mut total_fragments_expected = 0;
        let mut last_fragment_len = None;

        for p in packets {
            if let PacketType::MsgFragment(fragment) = p.pack_type {
                total_fragments_expected = fragment.total_n_fragments;
                if fragment.fragment_index == total_fragments_expected - 1 {
                    last_fragment_len = Some(fragment.length);
                }
                let start_index = fragment.fragment_index as usize * 128;
                let end_index = start_index + fragment.length as usize;

                if reassembled_data.len() < end_index {
                    reassembled_data.resize(end_index, 0);
                }
                reassembled_data[start_index..end_index].copy_from_slice(&fragment.data[..fragment.length as usize]);
            }
        }

        if let Some(last_len) = last_fragment_len {
            let expected_total_len = (total_fragments_expected as usize - 1) * 128 + last_len as usize;
            reassembled_data.truncate(expected_total_len);
        }
        String::from_utf8_lossy(&reassembled_data).trim().to_string()
    }

    fn setup_client_with_custom_shared_senders(
        client_id: NodeId,
        neighbor_ids: Vec<NodeId>,
        initial_shared_senders: HashMap<(NodeId, NodeId), Sender<Packet>>
    ) -> (MyClient, Sender<Packet>, HashMap<NodeId, Receiver<Packet>>, SharedGuiInput, Sender<Packet>) {
        let (_packet_recv_tx, packet_recv_rx) = unbounded::<Packet>();
        let mut packet_send_map = HashMap::new();
        let mut neighbor_receivers = HashMap::new();

        for &id in &neighbor_ids {
            let (tx, rx) = unbounded::<Packet>();
            packet_send_map.insert(id, tx);
            neighbor_receivers.insert(id, rx);
        }

        let (shortcut_tx, shortcut_rx) = unbounded::<Packet>();
        let gui_input_queue = new_gui_input_queue();
        let shared_senders_arc = Arc::new(Mutex::new(initial_shared_senders));

        let client = MyClient::new(
            client_id,
            packet_recv_rx,
            packet_send_map,
            HashMap::new(), // sent_messages
            None, // connected_server_id
            HashSet::new(), // seen_flood_ids
            Some(shared_senders_arc), // shared_senders
            Some(shortcut_rx), // shortcut_receiver
        );

        (client, _packet_recv_tx, neighbor_receivers, gui_input_queue, shortcut_tx)
    }

    fn setup_client (client_id: NodeId, neighbor_ids: Vec<NodeId>) -> (MyClient, Sender<Packet>, HashMap<NodeId, Receiver<Packet>>, SharedGuiInput, Sender<Packet>) {
        let (_packet_recv_tx, packet_recv_rx) = unbounded::<Packet>();
        let mut packet_send_map = HashMap::new();
        let mut neighbor_receivers = HashMap::new();
        for &id in &neighbor_ids {
            let (tx, rx) = unbounded::<Packet>();
            packet_send_map.insert(id, tx);
            neighbor_receivers.insert(id, rx);
        }
        let (shortcut_tx, shortcut_rx) = unbounded::<Packet>();
        let gui_input_queue = new_gui_input_queue();
        let shared_senders_mock = Arc::new(Mutex::new(HashMap::new()));
        let client = MyClient::new(
            client_id,
            packet_recv_rx,
            packet_send_map,
            HashMap::new(),
            None,
            HashSet::new(),
            Some(shared_senders_mock.clone()),
            Some(shortcut_rx),
            //Vec::new(),
        );
        (client, _packet_recv_tx, neighbor_receivers, gui_input_queue, shortcut_tx)
    }

    #[test]
    fn test_start_flood_discovery_generates_unique_id_and_sends_to_neighbors() {
        let client_id = 101;
        let neighbor1_id = 1;
        let neighbor2_id = 2;
        let neighbor_ids = vec![neighbor1_id, neighbor2_id];

        //- - - - preparing the environment for the test - - - -
        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }
        let (mut client, _client_incoming_packet_tx, neighbor_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, neighbor_ids.clone());
        let initial_session_counter_value = *SESSION_COUNTER.lock().unwrap();

        client.start_flood_discovery();

        //- - - - verifying - - - -
        //1.unique and progressive flood_id
        let expected_flood_id = initial_session_counter_value + 1;
        assert_eq!(
            *SESSION_COUNTER.lock().unwrap(),
            expected_flood_id,
            "SESSION_COUNTER should be incremented by 1 to have a unique id"
        );
        assert!(
            client.active_flood_discoveries.contains_key(&expected_flood_id),
            "flood_id should be registered in `active_flood_discoveries`"
        );
        let flood_state = client.active_flood_discoveries.get(&expected_flood_id).unwrap();
        assert_eq!(
            flood_state.initiator_id,
            client_id,
            "id of the initiator should be equal to client's id"
        );

        //2.FloodRequests sent to all neighbors
        assert_eq!(
            neighbor_receivers.len(),
            neighbor_ids.len(),
            "there should be a receiver for every attended neighbor"
        );
        for &neighbor_id in &neighbor_ids {
            let receiver = neighbor_receivers.get(&neighbor_id)
                .expect(&format!("Receiver for the neighbor {} not found", neighbor_id));
            let received_packet = receiver.try_recv()
                .expect(&format!("No FloodRequest packet received by neighbor {}.", neighbor_id));
            assert_eq!(
                received_packet.session_id,
                expected_flood_id,
                "session_id of the packet should be equal to flood_id"
            );
            match received_packet.pack_type {
                PacketType::FloodRequest(req) => {
                    assert_eq!(
                        req.flood_id,
                        expected_flood_id,
                        "FloodRequest's id should be equal to the temporary flood_id"
                    );
                    assert_eq!(
                        req.initiator_id,
                        client_id,
                        "initiator's id should be equal to client's id"
                    );
                    assert_eq!(
                        req.path_trace,
                        vec![(client_id, PktNodeType::Client)],
                        "the path should begin with the client initiator's id"
                    );
                    assert_eq!(
                        received_packet.routing_header.hop_index,
                        0,
                        "hop_index fo the routing header should be 0"
                    );
                    assert!(
                        received_packet.routing_header.hops.is_empty(),
                        "hops of the routing header should be empty"
                    );
                },
                _ => panic!(
                    "expected FloodRequest, but received different packet type for neighbor {}.",
                    neighbor_id
                ),
            }
            assert!(
                receiver.try_recv().is_err(),
                "expected only one FloodRequest packet per neighbor, but found many"
            );
        }
    }

    #[test]
    fn test_client_does_not_propagate_seen_flood_request() {
        let client_id = 101;
        let incoming_flood_sender_id = 1;
        let other_neighbor_id = 2;
        let neighbor_ids = vec![incoming_flood_sender_id, other_neighbor_id];
        let test_flood_id = 54321;
        let test_initiator_id = 250;

        //- - - - preparing the environment for the test - - - -
        let (mut client, client_incoming_packet_tx, neighbor_outbound_receivers, _gui_input, _shortcut_tx) =
            setup_client(client_id, neighbor_ids.clone());
        let flood_identifier = (test_flood_id, test_initiator_id);
        client.seen_flood_ids.insert(flood_identifier);
        assert!(client.packet_send.len() > 1, "client must have more than one neighbor to correctly test the propagation logic");
        let incoming_flood_request_payload = FloodRequest {
            flood_id: test_flood_id,
            initiator_id: test_initiator_id,
            path_trace: vec![
                (test_initiator_id, PktNodeType::Client),
                (10, PktNodeType::Drone),
                (incoming_flood_sender_id, PktNodeType::Drone),
            ],
        };
        let packet_to_client = Packet {
            pack_type: PacketType::FloodRequest(incoming_flood_request_payload.clone()),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: vec![],
            },
            session_id: test_flood_id,
        };

        client_incoming_packet_tx.send(packet_to_client.clone()).unwrap();
        client.process_packet(packet_to_client.clone());

        //- - - - verifying - - - -
        assert!(
            client.seen_flood_ids.contains(&flood_identifier),
            "flood_id should remain in `seen_flood_ids` after being elaborated"
        );
        for (&neighbor_id, receiver) in &neighbor_outbound_receivers {
            if neighbor_id == incoming_flood_sender_id {
                let received_packet = receiver.try_recv()
                    .expect(&format!("expected FloodResponse from client to node {}, but not received", neighbor_id));
                match received_packet.pack_type {
                    PacketType::FloodResponse(response) => {
                        assert_eq!(response.flood_id, test_flood_id, "FloodResponse's id is different from the predicted flood_id");
                        assert!(response.path_trace.starts_with(&incoming_flood_request_payload.path_trace), "path_trace of the FloodResponse should begin with the path_trace of the incoming request");
                        assert_eq!(response.path_trace.last().map(|(id, _)| *id), Some(client_id), "path_trace of the FloodResponse should end with the client's id");
                        assert_eq!(received_packet.session_id, test_flood_id, "session_id of the FloodResponse packet should be equal to flood_id");
                        let mut expected_hops_for_response: Vec<NodeId> = response.path_trace.iter().map(|(id, _)| *id).collect();
                        expected_hops_for_response.reverse();
                        assert_eq!(received_packet.routing_header.hops, expected_hops_for_response, "hops in the routing header of the FloodResponse don't correspond to the attended inverse path");
                        assert_eq!(received_packet.routing_header.hop_index, 1, "hop_index in the routing header of the FloodResponse should be 1.");
                    }
                    _ => panic!("expected FloodResponse, but received different packet type: {:?}", received_packet.pack_type),
                }
                assert!(receiver.try_recv().is_err(), "expected only one FloodResponse packet for the receiver, but found many");
            } else {
                assert!(
                    receiver.try_recv().is_err(),
                    "client shouldn't have propagated FloodRequest to neighbor {} since the id was already seen",
                    neighbor_id
                );
            }
        }
    }

    #[test]
    fn test_client_forwards_new_flood_request_to_other_neighbors() {
        let client_id = 101;
        let incoming_flood_sender_id = 1;
        let other_neighbor_id = 2;
        let neighbor_ids = vec![incoming_flood_sender_id, other_neighbor_id];
        let test_flood_id = 12345;
        let test_initiator_id = 50;

        //- - - - preparing the environment for the test - - - -
        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }
        let (mut client, client_incoming_packet_tx, neighbor_outbound_receivers, _gui_input, _shortcut_tx) =
            setup_client(client_id, neighbor_ids.clone());
        assert!(!client.seen_flood_ids.contains(&(test_flood_id, test_initiator_id)),
                "flood_id should not be already present in `seen_flood_ids` at the beginning of the test");
        assert!(client.packet_send.len() > 1,
                "client must have more than one neighbor to correctly test the propagation logic");
        let incoming_flood_request_payload = FloodRequest {
            flood_id: test_flood_id,
            initiator_id: test_initiator_id,
            path_trace: vec![
                (test_initiator_id, PktNodeType::Client),
                (10, PktNodeType::Drone),
                (incoming_flood_sender_id, PktNodeType::Drone),
            ],
        };
        let packet_to_client = Packet {
            pack_type: PacketType::FloodRequest(incoming_flood_request_payload.clone()),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: vec![],
            },
            session_id: test_flood_id,
        };

        client_incoming_packet_tx.send(packet_to_client.clone()).unwrap();
        client.process_packet(packet_to_client.clone());

        //- - - - verifying - - - -
        //1.flood_id now marked as "seen" by client
        assert!(
            client.seen_flood_ids.contains(&(test_flood_id, test_initiator_id)),
            "flood_id should be marked as seen after a new FloodRequest"
        );
        //2.sending to `other_neighbor_id` (which should receive a FloodRequest)
        let other_neighbor_receiver = neighbor_outbound_receivers.get(&other_neighbor_id)
            .expect(&format!("receiver for neighbor id {} not found", other_neighbor_id));
        let forwarded_packet = other_neighbor_receiver.try_recv()
            .expect(&format!("no FloodRequest packet receiver from neighbor {}.", other_neighbor_id));
        match forwarded_packet.pack_type {
            PacketType::FloodRequest(req) => {
                //forwarded FloodRequest has correct id
                assert_eq!(
                    req.flood_id,
                    test_flood_id,
                    "flood_id of the forwarded FloodRequest should be equal to the original one"
                );
                //correct initiator id
                assert_eq!(
                    req.initiator_id,
                    test_initiator_id,
                    "initiator_id of the forwarded FloodRequest should be equal to the original one"
                );
                let mut expected_path_trace = incoming_flood_request_payload.path_trace.clone();
                expected_path_trace.push((client_id, PktNodeType::Client));
                assert_eq!(
                    req.path_trace,
                    expected_path_trace,
                    "path_trace of the forwarded FloodRequest should include client's id"
                );
                assert_eq!(
                    forwarded_packet.routing_header.hop_index,
                    0,
                    "routing_header.hop_index of the forwarded FloodRequest should be 0"
                );
                assert!(
                    forwarded_packet.routing_header.hops.is_empty(),
                    "routing_header.hops of the forwarded FloodRequest should be empty"
                );
            },
            _ => panic!(
                "expected FloodRequest, but received different packet type for the neighbor {}: {:?}",
                other_neighbor_id, forwarded_packet.pack_type
            ),
        }
        assert!(
            other_neighbor_receiver.try_recv().is_err(),
            "expected only one FloodRequest packet for the other neighbor, but found many"
        );

        //3.no back-propagation to `incoming_flood_sender_id` (it should not receive FloodRequest in this scenario)
        let incoming_sender_receiver = neighbor_outbound_receivers.get(&incoming_flood_sender_id)
            .expect(&format!("receiver for the incoming sender {} not found", incoming_flood_sender_id));
        assert!(
            incoming_sender_receiver.try_recv().is_err(),
            "client shouldn't have sent FloodRequest to the receiver (neighbor {}) since it's the node from which it received the request",
            incoming_flood_sender_id
        );
    }

    #[test]
    fn test_create_topology_and_no_duplicate_edges() {
        let client_id = 101;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![]);
        assert_eq!(client.network_graph.node_count(), 0, "the network graph should be empty at the beginning");
        assert!(client.node_id_to_index.is_empty(), "the map node_id_to_index should be empty at the beginning");

        //first FloodResponse: path 101(Client) -> 1(Drone) -> 2(Drone) -> 200(Server)
        let path_trace_1: Vec<(NodeId, PktNodeType)> = vec![
            (client_id, PktNodeType::Client),
            (1, PktNodeType::Drone),
            (2, PktNodeType::Drone),
            (200, PktNodeType::Server),
        ];
        let flood_response_1 = FloodResponse { flood_id: 1, path_trace: path_trace_1.clone() };
        client.create_topology(&flood_response_1);

        //- - - - verifying - - - -
        //1.all the nodes from path_trace are added
        assert_eq!(client.network_graph.node_count(), 4, "there should be 4 nodes in the graph after the first FloodResponse");
        assert_eq!(client.node_id_to_index.len(), 4, "the map node_id_to_index should have 4 items");
        let expected_nodes_1 = vec![client_id, 1, 2, 200];
        for &id in &expected_nodes_1 {
            assert!(client.node_id_to_index.contains_key(&id), "node {} should be present", id);
        }

        //2.all the edges of the path_trace has been added bidirectionally with 0 weight
        let expected_edges_1 = vec![
            (client_id, 1),
            (1, 2),
            (2, 200),
        ];
        for (u, v) in &expected_edges_1 {
            let u_idx = *client.node_id_to_index.get(u).unwrap();
            let v_idx = *client.node_id_to_index.get(v).unwrap();
            assert!(has_edge(&client.network_graph, u_idx, v_idx), "the edge {} -> {} should exist", u, v);
            assert!(has_edge(&client.network_graph, v_idx, u_idx), "the edge {} -> {} (inverse) should exist", v, u);
            assert_eq!(*client.network_graph.edge_weight(client.network_graph.find_edge(u_idx, v_idx).unwrap()).unwrap(), 0, "the edge {} -> {} should have 0 weight", u, v);
        }
        assert_eq!(client.network_graph.edge_count(), 6, "there should be 6 edges (3 bidirectional) after the first FloodResponse");

        //second FloodResponse: path 101(Client) -> 1(Drone) -> 3(Drone) -> 200(Server)
        //edges 101-1 and 1-101 are duplicated. Edges 1-3, 3-1, 3-200, 200-3 are new
        let path_trace_2: Vec<(NodeId, PktNodeType)> = vec![
            (client_id, PktNodeType::Client),
            (1, PktNodeType::Drone),
            (3, PktNodeType::Drone),
            (200, PktNodeType::Server),
        ];
        let flood_response_2 = FloodResponse { flood_id: 2, path_trace: path_trace_2.clone() };
        client.create_topology(&flood_response_2);

        //- - - - verifying - - - -
        //1.only the new node (3) has been added
        assert_eq!(client.network_graph.node_count(), 5, "there should be 5 nodes in total (101,1,2,200,3).");
        assert_eq!(client.node_id_to_index.len(), 5, "the map node_id_to_index should have 5 items");
        assert!(client.node_id_to_index.contains_key(&3), "node 3 should be present");
        let node3_idx = *client.node_id_to_index.get(&3).unwrap();
        assert_eq!(client.network_graph.node_weight(node3_idx).unwrap().node_type, PktNodeType::Drone, "node 3 should be of Drone type");
        assert_eq!(client.network_graph.edge_count(), 10, "there should be 10 edges in total after the second FloodResponse (without duplicates)");
        let new_edges = vec![
            (1, 3),
            (3, 200),
        ];
        for (u, v) in &new_edges {
            let u_idx = *client.node_id_to_index.get(u).unwrap();
            let v_idx = *client.node_id_to_index.get(v).unwrap();
            assert!(has_edge(&client.network_graph, u_idx, v_idx), "the new edge {} -> {} should exist", u, v);
            assert!(has_edge(&client.network_graph, v_idx, u_idx), "the new edge {} -> {} (inverse) should exist", v, u);
            assert_eq!(*client.network_graph.edge_weight(client.network_graph.find_edge(u_idx, v_idx).unwrap()).unwrap(), 0, "the new edge {} -> {} should have 0 weight", u, v);
        }

        let client_idx = *client.node_id_to_index.get(&client_id).unwrap();
        let node1_idx = *client.node_id_to_index.get(&1).unwrap();
        assert_eq!(*client.network_graph.edge_weight(client.network_graph.find_edge(client_idx, node1_idx).unwrap()).unwrap(), 0, "edge 101 -> 1 should still have 0 weight");
    }

    #[test]
    fn test_process_flood_response_unknown_id_still_creates_topology() {
        let client_id = 101;
        let (mut client, client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![]);
        let unknown_flood_id = 99999;
        assert!(!client.active_flood_discoveries.contains_key(&unknown_flood_id), "flood_id should not be initially active");
        assert_eq!(client.network_graph.node_count(), 0, "the network graph must be empty at the beginning");
        //Client 101 -> Drone 5 -> Drone 6 -> Server 201
        let path_trace: Vec<(NodeId, PktNodeType)> = vec![
            (client_id, PktNodeType::Client),
            (5, PktNodeType::Drone),
            (6, PktNodeType::Drone),
            (201, PktNodeType::Server),
        ];
        let flood_response = FloodResponse { flood_id: unknown_flood_id, path_trace: path_trace.clone() };
        let packet_to_client = Packet {
            pack_type: PacketType::FloodResponse(flood_response.clone()),
            routing_header: SourceRoutingHeader { hop_index: 0, hops: vec![] },
            session_id: unknown_flood_id,
        };

        client_incoming_packet_tx.send(packet_to_client.clone()).unwrap();
        client.process_packet(packet_to_client.clone());

        //- - - - verifying - - - -
        //topology created even if the flood_id is unknown/inactive
        assert_eq!(client.network_graph.node_count(), 4, "topology should have been uploaded with 4 nodes");
        assert_eq!(client.node_id_to_index.len(), 4, "the map node_id_to_index should have 4 items");
        //presence of specific nodes and edges
        assert!(client.node_id_to_index.contains_key(&client_id), "the node client_id should be present");
        assert!(client.node_id_to_index.contains_key(&5), "the node 5 should be present");
        assert!(client.node_id_to_index.contains_key(&6), "the node 6 should be present");
        assert!(client.node_id_to_index.contains_key(&201), "the node 201 should be present");
        let client_idx = *client.node_id_to_index.get(&client_id).unwrap();
        let node5_idx = *client.node_id_to_index.get(&5).unwrap();
        let node6_idx = *client.node_id_to_index.get(&6).unwrap();
        let server201_idx = *client.node_id_to_index.get(&201).unwrap();
        assert!(has_edge(&client.network_graph, client_idx, node5_idx), "the edge client_id -> 5 should exist");
        assert!(has_edge(&client.network_graph, node5_idx, client_idx), "the edge 5 -> client_id should exist");
        assert!(has_edge(&client.network_graph, node5_idx, node6_idx), "th edge 5 -> 6 should exist");
        assert!(has_edge(&client.network_graph, node6_idx, node5_idx), "the edge 6 -> 5 should exist");
        assert!(has_edge(&client.network_graph, node6_idx, server201_idx), "the edge 6 -> 201 should exist");
        assert!(has_edge(&client.network_graph, server201_idx, node6_idx), "the edge 201 -> 6 should exist");
        assert_eq!(client.network_graph.edge_count(), 6, "there should be 6 bidirectional edges");
    }

    #[test]
    fn test_finalize_flood_discovery_after_timeout() {
        let client_id = 101;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbond_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![]);
        assert_eq!(client.network_graph.node_count(), 0, "the network graph should be empty at the beginning");
        let test_flood_id = 123456789;
        let test_initiator_id = client_id;
        let path_trace_1: Vec<(NodeId, PktNodeType)> = vec![
            (client_id, PktNodeType::Client),
            (1, PktNodeType::Drone),
            (2, PktNodeType::Drone),
            (200, PktNodeType::Server),
        ];
        let flood_response_1 = FloodResponse { flood_id: test_flood_id, path_trace: path_trace_1.clone() };
        let path_trace_2: Vec<(NodeId, PktNodeType)> = vec![
            (client_id, PktNodeType::Client),
            (3, PktNodeType::Drone),
            (4, PktNodeType::Drone),
            (200, PktNodeType::Server),
        ];
        let flood_response_2 = FloodResponse { flood_id: test_flood_id, path_trace: path_trace_2.clone() };
        let discovery_state = FloodDiscoveryState {
            initiator_id: test_initiator_id,
            start_time: Instant::now() - Duration::from_secs(3),
            received_responses: vec![flood_response_1, flood_response_2],
        };

        client.active_flood_discoveries.insert(test_flood_id, discovery_state);

        assert!(client.active_flood_discoveries.contains_key(&test_flood_id), "flood discovery should be initially active");

        client.check_flood_discoveries_timeouts();

        //- - - - verifying - - - -
        //1.flood discovery has been removed from active flood discoveries
        assert!(!client.active_flood_discoveries.contains_key(&test_flood_id), "flood discovery should be finalized and removed after the timeout");

        //2.the network graph has been updated with nodes and edges of both FloodResponses
        let expected_nodes_1 = vec![client_id, 1, 2, 200];
        for &id in &expected_nodes_1 {
            assert!(client.node_id_to_index.contains_key(&id), "node {} (from first path) should be present in the graph", id);
        }
        let expected_nodes_2 = vec![client_id, 3, 4, 200];
        for &id in &expected_nodes_2 {
            assert!(client.node_id_to_index.contains_key(&id), "node {} (from second path) should be present in the graph", id);
        }
        assert_eq!(client.network_graph.node_count(), 6, "there should be 6 unique nodes in the graph, after the elaboration");
        assert_eq!(client.node_id_to_index.len(), 6, "the map node_id_to_index should have 6 items");
        let expected_edges = vec![
            (client_id, 1), (1, 2), (2, 200),
            (client_id, 3), (3, 4), (4, 200),
        ];
        for (u, v) in &expected_edges {
            let u_idx = *client.node_id_to_index.get(u).unwrap();
            let v_idx = *client.node_id_to_index.get(v).unwrap();
            assert!(has_edge(&client.network_graph, u_idx, v_idx), "edge {} -> {} should exist", u, v);
            assert!(has_edge(&client.network_graph, v_idx, u_idx), "edge {} -> {} (inverse) should exist", v, u);
            assert_eq!(*client.network_graph.edge_weight(client.network_graph.find_edge(u_idx, v_idx).unwrap()).unwrap(), 0, "edge {} -> {} should have 0 weight", u, v);
        }
        assert_eq!(client.network_graph.edge_count(), 12, "there should be 12 edges in total (6 unique bidirectional) in the graph");
    }

    #[test]
    fn test_best_path_shortest_with_costs() {
        let client_id = 101;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![]);

        let nodes_info = vec![
            (client_id, PktNodeType::Client),
            (1, PktNodeType::Drone),
            (2, PktNodeType::Drone),
            (3, PktNodeType::Drone),
            (4, PktNodeType::Drone),
            (200, PktNodeType::Server),
        ];

        let mut node_indices = HashMap::new();
        for (id, node_type) in nodes_info {
            let node_idx = client.network_graph.add_node(NodeInfo { id, node_type });
            client.node_id_to_index.insert(id, node_idx);
            node_indices.insert(id, node_idx);
        }

        //adding edges to network_graph (first path)
        client.network_graph.add_edge(*node_indices.get(&client_id).unwrap(), *node_indices.get(&1).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&1).unwrap(), *node_indices.get(&client_id).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&1).unwrap(), *node_indices.get(&2).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&2).unwrap(), *node_indices.get(&1).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&2).unwrap(), *node_indices.get(&200).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&200).unwrap(), *node_indices.get(&2).unwrap(), 0);

        //adding edges to network_graph (second path)
        client.network_graph.add_edge(*node_indices.get(&client_id).unwrap(), *node_indices.get(&3).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&3).unwrap(), *node_indices.get(&client_id).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&3).unwrap(), *node_indices.get(&4).unwrap(), 10);
        client.network_graph.add_edge(*node_indices.get(&4).unwrap(), *node_indices.get(&3).unwrap(), 10);
        client.network_graph.add_edge(*node_indices.get(&4).unwrap(), *node_indices.get(&200).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&200).unwrap(), *node_indices.get(&4).unwrap(), 0);

        if let Some(shared_senders) = &client.shared_senders {
            let mut map = shared_senders.lock().unwrap();

            //first path
            map.insert((client_id, 1), unbounded().0);
            map.insert((1, client_id), unbounded().0);
            map.insert((1, 2), unbounded().0);
            map.insert((2, 1), unbounded().0);
            map.insert((2, 200), unbounded().0);
            map.insert((200, 2), unbounded().0);

            //second path
            map.insert((client_id, 3), unbounded().0);
            map.insert((3, client_id), unbounded().0);
            map.insert((3, 4), unbounded().0);
            map.insert((4, 3), unbounded().0);
            map.insert((4, 200), unbounded().0);
            map.insert((200, 4), unbounded().0);
        }

        //test case 1.1: verifying the shortest path with costs between 101 and 200
        let path_option = client.best_path(client_id, 200);
        assert!(path_option.is_some(), "should be found a path from 101 to 200");
        let expected_path = vec![client_id, 1, 2, 200];
        assert_eq!(path_option.unwrap(), expected_path, "the found path is not the shortest/has not the minimum cost");

        //test case 1.2: verifying the structure of the path (inizia con 'from' e finisce con 'to').
        let path = client.best_path(client_id, 200);
        assert!(path.is_some(), "the path shouldn't be None");
        let unwrapped_path = path.unwrap();
        assert_eq!(*unwrapped_path.first().unwrap(), client_id, "the path must start with the source node");
        assert_eq!(*unwrapped_path.last().unwrap(), 200, "the path must end with the destination node");
    }

    #[test]
    fn test_best_path_no_path_exists() {
        let client_id = 101;
        let drone1_id = 1;
        let server_id = 200;

        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![drone1_id]);

        let nodes_info = vec![
            (client_id, PktNodeType::Client),
            (drone1_id, PktNodeType::Drone),
            (server_id, PktNodeType::Server),
        ];

        let mut node_indices = HashMap::new();
        for (id, node_type) in nodes_info {
            let node_idx = client.network_graph.add_node(NodeInfo { id, node_type });
            client.node_id_to_index.insert(id, node_idx);
            node_indices.insert(id, node_idx);
        }

        client.network_graph.add_edge(*node_indices.get(&client_id).unwrap(), *node_indices.get(&drone1_id).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&drone1_id).unwrap(), *node_indices.get(&client_id).unwrap(), 0);

        let path = client.best_path(client_id, server_id);

        assert!(path.is_none(), "a path should not be found between disconnected nodes");

        let target_id_for_non_existent_source = 200;
        let path_source_not_in_graph = client.best_path(250, target_id_for_non_existent_source);
        assert!(path_source_not_in_graph.is_none(), "best_path() should return None is the source node is not in the graph");

        let source_id_for_non_existent_target = client_id;
        let path_target_not_in_graph = client.best_path(source_id_for_non_existent_target, 251);
        assert!(path_target_not_in_graph.is_none(), "best_path() should return None if the destination node is not in the graph");
    }

    #[test]
    fn test_best_path_source_node_not_in_graph() {
        let client_id = 101;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![]);
        let target_id = 200;
        let target_node_idx = client.network_graph.add_node(NodeInfo { id: target_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(target_id, target_node_idx);
        let path = client.best_path(235, target_id);
        assert!(path.is_none(), "best_path() should give None if the source node isn't in the graph");
    }

    #[test]
    fn test_best_path_destination_node_not_in_graph() {
        let client_id = 101;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![]);
        let source_id = client_id;
        let source_node_idx = client.network_graph.add_node(NodeInfo { id: source_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(source_id, source_node_idx);
        let path = client.best_path(source_id, 244);
        assert!(path.is_none(), "best_path() should give None if the destination node isn't in the graph");
    }

    #[test]
    fn test_flood_discovery_on_best_path_failure() {
        let client_id = 101;
        let server_id = 200;

        //- - - - preparing the environment for the test - - - -
        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![]);
        assert_eq!(client.network_graph.node_count(), 0, "network graph should be empty at the beginning");
        assert!(client.active_flood_discoveries.is_empty(), "no flood discovery active at the beginning");
        let login_command = format!("[Login]::{}", server_id);
        client.process_gui_command(client_id, login_command);
        assert_eq!(client.connected_server_id, Some(server_id), "client should be connected to server after the Login command");
        let message_command = format!("[MessageTo]::{}::Hello World", client_id);
        client.process_gui_command(client_id, message_command);

        //verifying that a new flood discovery has been activated
        assert!(!client.active_flood_discoveries.is_empty(), "a new flood discovery should have been activated because best_path() isn't able to find a path to the server");
        let mut flood_ids: Vec<u64> = client.active_flood_discoveries.keys().cloned().collect();
        assert_eq!(flood_ids.len(), 1, "there should be exactly one flood active discovery");
        let initiated_flood_id = flood_ids.remove(0);
        let flood_state = client.active_flood_discoveries.get(&initiated_flood_id).unwrap();
        assert_eq!(flood_state.initiator_id, client_id, "client itself (id: {}) should be the initializer of the flood discovery", client_id);
        assert_eq!(initiated_flood_id, 1, "flood_id should be 1 after the first flood discovery");
    }

    #[test]
    fn test_process_nack_error_in_routing_node_crash() {
        let client_id = 101;
        let drone1_id = 1;
        let drone2_id = 2;
        let server_id = 200;

        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) =
            setup_client(client_id, vec![drone1_id]);

        //- - - - preparing the environment for the test - - - -
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);

        let drone1_idx = client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone1_id, drone1_idx);

        let drone2_idx = client.network_graph.add_node(NodeInfo { id: drone2_id, node_type: PktNodeType::Drone }); //problematic node
        client.node_id_to_index.insert(drone2_id, drone2_idx);

        let server_idx = client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(server_id, server_idx);

        client.network_graph.add_edge(client_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone1_idx, client_idx, 0);
        client.network_graph.add_edge(drone1_idx, drone2_idx, 0);
        client.network_graph.add_edge(drone2_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone2_idx, server_idx, 0);
        client.network_graph.add_edge(server_idx, drone2_idx, 0);

        let session_id = 456;
        let nack_packet_id = 0;
        let problem_node_id = drone2_id;

        let original_rh = SourceRoutingHeader {
            hops: vec![client_id, drone1_id, problem_node_id, server_id],
            hop_index: 2,
        };

        let nack_type = NackType::ErrorInRouting(problem_node_id);
        let nack = Nack { fragment_index: nack_packet_id, nack_type: nack_type.clone() };
        let mut packet = Packet { pack_type: PacketType::Nack(nack.clone()), routing_header: original_rh.clone(), session_id };

        client.sent_messages.insert(session_id, SentMessageInfo {
            fragments: vec![],
            original_routing_header: original_rh.clone(),
            received_ack_indices: HashSet::new(),
            route_needs_recalculation: false,
        });
        client.route_cache.insert(server_id, vec![client_id, drone1_id, problem_node_id, server_id]);

        assert!(client.route_cache.contains_key(&server_id), "The route to the server should initially be in the cache");
        assert!(client.node_id_to_index.contains_key(&problem_node_id), "The problematic node should exist in the graph before the NACK");
        assert!(client.network_graph.node_weights().any(|ni| ni.id == problem_node_id), "The node should be present in the graph's weights before the NACK");
        assert!(client.packet_send.contains_key(&drone1_id), "The client should have a sender for drone1_id (direct neighbor)");
        assert!(!client.packet_send.contains_key(&problem_node_id), "The client should NOT have a sender for problem_node_id (not a direct neighbor)");

        client.process_nack(&nack, &mut packet);

        //- - - - verifying - - - -
        //1.that the problematic node has been removed from the graph and from map
        assert!(!client.node_id_to_index.contains_key(&problem_node_id), "The problematic node should be removed from the graph's index map");
        assert!(!client.network_graph.node_weights().any(|ni| ni.id == problem_node_id), "The node should not be present in the node weights of the graph after the NACK");

        //2.that the route for this session has been marked for recalculation
        assert!(client.sent_messages.get(&session_id).unwrap().route_needs_recalculation, "The route for the session should be marked for recalculation");

        //3.that the route_cache for the destination (server_id) has been invalidated
        assert!(!client.route_cache.contains_key(&server_id), "The cached route for the destination should be removed");

        //4.`packet_send` shouldn't be influenced because problem_node_id wasn't a direct neighbor of the client
        assert!(client.packet_send.contains_key(&drone1_id), "The client should still have a sender for drone1_id");
    }

    #[test]
    fn test_process_nack_error_in_routing_link_failure() {
        let client_id = 101;
        let drone1_id = 1;
        let drone2_id = 2;
        let drone3_id = 3;
        let server_id = 200;

        //- - - - setup of `shared_senders` to simulate that `drone2_id` is still active - - - -
        let (d1_tx_to_d2, _) = unbounded::<Packet>();
        let (d2_tx_to_d1, _) = unbounded::<Packet>();
        let (d2_tx_to_d3, _) = unbounded::<Packet>();
        let (d3_tx_to_d2, _) = unbounded::<Packet>();
        let (d3_tx_to_s, _) = unbounded::<Packet>();
        let (s_tx_to_d3, _) = unbounded::<Packet>();

        let mut shared_senders_map = HashMap::new();
        shared_senders_map.insert((drone1_id, drone2_id), d1_tx_to_d2.clone());
        shared_senders_map.insert((drone2_id, drone1_id), d2_tx_to_d1.clone());
        shared_senders_map.insert((drone2_id, drone3_id), d2_tx_to_d3.clone());
        shared_senders_map.insert((drone3_id, drone2_id), d3_tx_to_d2.clone());
        shared_senders_map.insert((drone3_id, server_id), d3_tx_to_s.clone());
        shared_senders_map.insert((server_id, drone3_id), s_tx_to_d3.clone());

        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) =
            setup_client_with_custom_shared_senders(client_id, vec![drone1_id], shared_senders_map);

        //- - - - preparing the graph - - - -
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone1_idx = client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone1_id, drone1_idx);
        let drone2_idx = client.network_graph.add_node(NodeInfo { id: drone2_id, node_type: PktNodeType::Drone }); // Nodo problematico
        client.node_id_to_index.insert(drone2_id, drone2_idx);
        let drone3_idx = client.network_graph.add_node(NodeInfo { id: drone3_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone3_id, drone3_idx);
        let server_idx = client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(server_id, server_idx);

        client.network_graph.add_edge(client_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone1_idx, client_idx, 0);
        client.network_graph.add_edge(drone1_idx, drone2_idx, 0);
        client.network_graph.add_edge(drone2_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone2_idx, drone3_idx, 0);
        client.network_graph.add_edge(drone3_idx, drone2_idx, 0);
        client.network_graph.add_edge(drone3_idx, server_idx, 0);
        client.network_graph.add_edge(server_idx, drone3_idx, 0);

        let session_id = 789;
        let nack_packet_id = 0;
        let problem_node_id = drone2_id;

        let original_rh = SourceRoutingHeader {
            hops: vec![client_id, drone1_id, problem_node_id, drone3_id, server_id],
            hop_index: 2,
        };

        let nack_type = NackType::ErrorInRouting(problem_node_id);
        let nack = Nack { fragment_index: nack_packet_id, nack_type: nack_type.clone() };
        let mut packet = Packet { pack_type: PacketType::Nack(nack.clone()), routing_header: original_rh.clone(), session_id };

        client.sent_messages.insert(session_id, SentMessageInfo {
            fragments: vec![],
            original_routing_header: original_rh.clone(),
            received_ack_indices: HashSet::new(),
            route_needs_recalculation: false,
        });
        client.route_cache.insert(server_id, vec![client_id, drone1_id, problem_node_id, drone3_id, server_id]);

        assert!(client.route_cache.contains_key(&server_id), "The route to the server should initially be in the cache");
        assert!(client.node_id_to_index.contains_key(&problem_node_id), "The problematic node should exist in the graph before the NACK");
        assert!(client.network_graph.node_weights().any(|ni| ni.id == problem_node_id), "The node should be present in the graph weights before the NACK");
        assert!(client.network_graph.contains_edge(drone1_idx, drone2_idx), "The link from drone1 to drone2 should initially exist");
        assert!(client.network_graph.contains_edge(drone2_idx, drone1_idx), "The link from drone2 to drone1 should initially exist");
        assert!(client.network_graph.contains_edge(drone2_idx, drone3_idx), "The link from drone2 to drone3 should initially exist (to confirm it stays alive)");
        assert!(client.network_graph.contains_edge(drone3_idx, drone2_idx), "The link from drone3 to drone2 should initially exist (to confirm it stays alive)");
        assert!(client.packet_send.contains_key(&drone1_id), "The client should have a sender for drone1_id (direct neighbor)");
        assert!(!client.packet_send.contains_key(&problem_node_id), "The client should NOT have a sender for problem_node_id (not a direct neighbor)");

        client.process_nack(&nack, &mut packet);

        //- - - - verifying - - - -
        //1.that the problematic node hasn't been removed from graph or from the map
        assert!(client.node_id_to_index.contains_key(&problem_node_id), "The problematic node should NOT be removed from the graph's index map");
        assert!(client.network_graph.node_weights().any(|ni| ni.id == problem_node_id), "The node SHOULD STILL be present in the graph's node weights after the NACK");

        //2.that the specific link (`drone1_id <-> drone2_id`) has been removed from graph
        let from_node_id = original_rh.hops[original_rh.hop_index.saturating_sub(1)];
        let from_idx = *client.node_id_to_index.get(&from_node_id).unwrap();
        let to_idx = *client.node_id_to_index.get(&problem_node_id).unwrap();
        assert!(!client.network_graph.contains_edge(from_idx, to_idx), "The link from drone1 to drone2 should be removed");
        assert!(!client.network_graph.contains_edge(to_idx, from_idx), "The link from drone2 to drone1 should be removed");

        //3.that other links that deal with `problem_node_id` are still present
        let drone3_idx = *client.node_id_to_index.get(&drone3_id).unwrap();
        assert!(client.network_graph.contains_edge(to_idx, drone3_idx), "The link from drone2 to drone3 should still exist");
        assert!(client.network_graph.contains_edge(drone3_idx, to_idx), "The link from drone3 to drone2 should still exist");

        //4.that the route for this session has been marked for recalculation
        assert!(client.sent_messages.get(&session_id).unwrap().route_needs_recalculation, "The route for the session should be marked for recalculation");

        //5.that the route_cache for destination (server_id) has been invalidated
        assert!(!client.route_cache.contains_key(&server_id), "The cached route for the destination should be removed");

        //6.`packet_send` shouldn't be influenced because problem_node_id wasn't a direct neighbor of the client
        assert!(client.packet_send.contains_key(&drone1_id), "The client should still have a sender for drone1_id");
    }

    #[test]
    fn test_process_nack_destination_is_drone() {
        let client_id = 101;
        let drone1_id = 1;
        let drone_destination_id = 50;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![drone1_id]);

        //- - - - preparing the environment for the test - - - -
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone1_idx = client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone1_id, drone1_idx);
        let drone_destination_idx = client.network_graph.add_node(NodeInfo { id: drone_destination_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone_destination_id, drone_destination_idx);
        client.network_graph.add_edge(client_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone1_idx, client_idx, 0);
        client.network_graph.add_edge(drone1_idx, drone_destination_idx, 0);
        client.network_graph.add_edge(drone_destination_idx, drone1_idx, 0);
        let session_id = 789;
        let nack_packet_id = 0;

        let original_rh = SourceRoutingHeader {
            hops: vec![client_id, drone1_id, drone_destination_id],
            hop_index: 2,
        };
        let nack_type = NackType::DestinationIsDrone;
        let nack = Nack { fragment_index: nack_packet_id, nack_type: nack_type.clone() };
        let mut packet = Packet { pack_type: PacketType::Nack(nack.clone()), routing_header: original_rh.clone(), session_id };
        client.sent_messages.insert(session_id, SentMessageInfo {
            fragments: vec![],
            original_routing_header: original_rh.clone(),
            received_ack_indices: HashSet::new(),
            route_needs_recalculation: false,
        });
        client.route_cache.insert(drone_destination_id, vec![client_id, drone1_id, drone_destination_id]);
        assert!(client.route_cache.contains_key(&drone_destination_id), "route towards destination drone should be initially in the cache");

        client.process_nack(&nack, &mut packet);

        //- - - - verifying - - - -
        //1.that the route for this session has been marked for the recalculation
        assert!(client.sent_messages.get(&session_id).unwrap().route_needs_recalculation, "route for the session should be marked for the recalculation");

        //2.that the route in cache for the destination (drone_destination_id) has been invalidated
        assert!(!client.route_cache.contains_key(&drone_destination_id), "route in cache for the destination of the drone should be removed");

        //3.that no nodes has been removed
        assert!(client.node_id_to_index.contains_key(&drone_destination_id), "destination node of the drone should be still in the graph");
    }

    #[test]
    fn test_process_nack_unexpected_recipient() {
        let client_id = 101;
        let drone1_id = 1;
        let drone_expected_recipient = 2;
        let drone_actual_recipient = 3;
        let server_id = 200;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![drone1_id, drone_actual_recipient]);

        //- - - - preparing the environment for the test - - - -
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone1_idx = client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone1_id, drone1_idx);
        let drone_expected_idx = client.network_graph.add_node(NodeInfo { id: drone_expected_recipient, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone_expected_recipient, drone_expected_idx);
        let drone_actual_idx = client.network_graph.add_node(NodeInfo { id: drone_actual_recipient, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone_actual_recipient, drone_actual_idx);
        let server_idx = client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(server_id, server_idx);

        client.network_graph.add_edge(client_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone1_idx, client_idx, 0);
        client.network_graph.add_edge(drone1_idx, drone_actual_idx, 0);
        client.network_graph.add_edge(drone_actual_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone_expected_idx, server_idx, 0);
        client.network_graph.add_edge(server_idx, drone_expected_idx, 0);

        let session_id = 910;
        let nack_packet_id = 0;
        let received_at_node_id = drone_actual_recipient;

        let nack_return_rh = SourceRoutingHeader {
            hops: vec![drone_actual_recipient, drone1_id, client_id],
            hop_index: 1,
        };

        let nack_type = NackType::UnexpectedRecipient(received_at_node_id);
        let nack = Nack { fragment_index: nack_packet_id, nack_type: nack_type.clone() };

        let mut packet = Packet { pack_type: PacketType::Nack(nack.clone()), routing_header: nack_return_rh.clone(), session_id };

        client.sent_messages.insert(session_id, SentMessageInfo {
            fragments: vec![],
            original_routing_header: SourceRoutingHeader {
                hops: vec![client_id, drone1_id, drone_expected_recipient, server_id],
                hop_index: 2,
            },
            received_ack_indices: HashSet::new(),
            route_needs_recalculation: false,
        });

        client.route_cache.insert(server_id, vec![client_id, drone1_id, drone_expected_recipient, server_id]);

        assert!(client.route_cache.contains_key(&server_id), "route towards server should be initially in cache");

        let problem_link_from_id = drone1_id;
        let problem_link_to_id = drone_actual_recipient;

        let problem_link_from_idx = *client.node_id_to_index.get(&problem_link_from_id).unwrap();
        let problem_link_to_idx = *client.node_id_to_index.get(&problem_link_to_id).unwrap();

        let initial_weight_forward = *client.network_graph.edge_weight(client.network_graph.find_edge(problem_link_from_idx, problem_link_to_idx).unwrap()).unwrap();
        let initial_weight_backward = *client.network_graph.edge_weight(client.network_graph.find_edge(problem_link_to_idx, problem_link_from_idx).unwrap()).unwrap();

        assert_eq!(initial_weight_forward, 0);
        assert_eq!(initial_weight_backward, 0);

        client.process_nack(&nack, &mut packet);

        //- - - - verifying - - - -
        //1.that the number of "drop" for the problematic link is incremented in both directions
        let new_weight_forward = *client.network_graph.edge_weight(client.network_graph.find_edge(problem_link_from_idx, problem_link_to_idx).unwrap()).unwrap();
        let new_weight_backward = *client.network_graph.edge_weight(client.network_graph.find_edge(problem_link_to_idx, problem_link_from_idx).unwrap()).unwrap();

        assert_eq!(new_weight_forward, initial_weight_forward.saturating_add(1), "weight of the forward link should be incremented");
        assert_eq!(new_weight_backward, initial_weight_backward.saturating_add(1), "weight of the backward link should be incremented");

        //2.that the route for this session has been marked for the recalculation
        assert!(client.sent_messages.get(&session_id).unwrap().route_needs_recalculation, "the route for the session should be marked for the recalculation");

        //3.that the route in cache for the destination (server_id) has been invalidated
        assert!(!client.route_cache.contains_key(&server_id), "the route in cache for server should be removed");
    }

    #[test]
    fn test_process_nack_dropped() {
        let client_id = 101;
        let drone1_id = 1;
        let drone_dropped_at_id = 2;
        let server_id = 200;
        let (mut client, _client_incoming_packet_tx, mut neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![drone1_id]);

        //- - - - preparing the environment for the test - - - -
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone1_idx = client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone1_id, drone1_idx);
        let drone_dropped_at_idx = client.network_graph.add_node(NodeInfo { id: drone_dropped_at_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone_dropped_at_id, drone_dropped_at_idx);
        let server_idx = client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(server_id, server_idx);
        client.network_graph.add_edge(client_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone1_idx, client_idx, 0);
        client.network_graph.add_edge(drone1_idx, drone_dropped_at_idx, 0);
        client.network_graph.add_edge(drone_dropped_at_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone_dropped_at_idx, server_idx, 0);
        client.network_graph.add_edge(server_idx, drone_dropped_at_idx, 0);
        let session_id = 789;
        let nack_fragment_index = 5;
        let nack_type = NackType::Dropped;
        let nack = Nack { fragment_index: nack_fragment_index, nack_type: nack_type.clone() };
        let original_packet_hops = vec![client_id, drone1_id, drone_dropped_at_id, server_id];
        let original_rh_for_resend = SourceRoutingHeader {
            hops: original_packet_hops.clone(),
            hop_index: 1,
        };
        let rh_nack_from_dropper = SourceRoutingHeader {
            hops: original_packet_hops.clone(),
            hop_index: 2,
        };
        let mut packet = Packet { pack_type: PacketType::Nack(nack.clone()), routing_header: rh_nack_from_dropper.clone(), session_id };
        let fragment_to_resend = Fragment {
            fragment_index: nack_fragment_index,
            total_n_fragments: 10,
            length: 50,
            data: [0xAA; 128],
        };

        client.sent_messages.insert(session_id, SentMessageInfo {
            fragments: vec![fragment_to_resend.clone()],
            original_routing_header: original_rh_for_resend.clone(),
            received_ack_indices: HashSet::new(),
            route_needs_recalculation: false,
        });

        let from_node_for_assert = rh_nack_from_dropper.hops[rh_nack_from_dropper.hop_index - 1];
        let dropped_at_node_for_assert = rh_nack_from_dropper.hops[rh_nack_from_dropper.hop_index];

        let from_node_idx = *client.node_id_to_index.get(&from_node_for_assert).unwrap();
        let dropped_at_node_idx = *client.node_id_to_index.get(&dropped_at_node_for_assert).unwrap();
        let initial_weight_forward = *client.network_graph.edge_weight(client.network_graph.find_edge(from_node_idx, dropped_at_node_idx).unwrap()).unwrap();
        let initial_weight_backward = *client.network_graph.edge_weight(client.network_graph.find_edge(dropped_at_node_idx, from_node_idx).unwrap()).unwrap();
        assert_eq!(initial_weight_forward, 0);
        assert_eq!(initial_weight_backward, 0);

        client.process_nack(&nack, &mut packet);

        //- - - - verifying - - - -
        //1.that the number of "drop" for the problematic link is incremented in both directions
        let new_weight_forward = *client.network_graph.edge_weight(client.network_graph.find_edge(from_node_idx, dropped_at_node_idx).unwrap()).unwrap();
        let new_weight_backward = *client.network_graph.edge_weight(client.network_graph.find_edge(dropped_at_node_idx, from_node_idx).unwrap()).unwrap();
        assert_eq!(new_weight_forward, initial_weight_forward.saturating_add(1), "weight of the forward link should be incremented");
        assert_eq!(new_weight_backward, initial_weight_backward.saturating_add(1), "weight of the backward link should be incremented");

        //2.that the fragment has been resent
        let expected_resend_target = original_rh_for_resend.hops[original_rh_for_resend.hop_index];
        let drone1_receiver = neighbor_outbound_receivers.get_mut(&expected_resend_target)
            .expect("client should have a sender towards drone1_id");
        let received_packet = drone1_receiver.recv_timeout(Duration::from_millis(100))
            .expect("client should have retransmitted the dropped fragment towards drone1_id");

        match received_packet.pack_type {
            PacketType::MsgFragment(frag) => {
                assert_eq!(frag.fragment_index, nack_fragment_index, "the index of the retransmitted fragment doesn't correspond");
                assert_eq!(received_packet.session_id, session_id, "session_id of the retransmitted packet doesn't correspond");
                assert_eq!(received_packet.routing_header.hops, original_rh_for_resend.hops, "hops of the routing header of the retransmitted packet don't correspond");
                assert_eq!(received_packet.routing_header.hop_index, original_rh_for_resend.hop_index, "hop_index of the routing header of the retransmitted packet doesn't correspond");
            },
            _ => panic!("expected MsgFragment for the retransmitted packet, but received {:?}", received_packet.pack_type),
        }
    }

    #[test]
    fn test_increment_drop_increases_weight() {
        let client_id = 101;
        let drone1_id = 1;
        let drone2_id = 2;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![drone1_id, drone2_id]);

        //- - - - preparing the environment for the test - - - -
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone1_idx = client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone1_id, drone1_idx);
        let drone2_idx = client.network_graph.add_node(NodeInfo { id: drone2_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone2_id, drone2_idx);
        let edge_idx_forward = client.network_graph.add_edge(drone1_idx, drone2_idx, 0);
        let edge_idx_backward = client.network_graph.add_edge(drone2_idx, drone1_idx, 0);
        let initial_weight_forward = *client.network_graph.edge_weight(edge_idx_forward).unwrap();
        let initial_weight_backward = *client.network_graph.edge_weight(edge_idx_backward).unwrap();

        //verifying the weights
        assert_eq!(initial_weight_forward, 0, "initial weight of the forward link should be 0");
        assert_eq!(initial_weight_backward, 0, "initial weight of the backward link should be 0");

        client.increment_drop(drone1_id, drone2_id);
        client.increment_drop(drone2_id, drone1_id);

        //verifying that the weight has been incremented correctly in both directions
        let new_weight_forward = *client.network_graph.edge_weight(edge_idx_forward).unwrap();
        let new_weight_backward = *client.network_graph.edge_weight(edge_idx_backward).unwrap();
        assert_eq!(new_weight_forward, initial_weight_forward.saturating_add(1), "weight of the forward link isn't incremented correctly");
        assert_eq!(new_weight_backward, initial_weight_backward.saturating_add(1), "weight of the backward link isn't incremented correctly");
    }

    #[test]
    fn test_remove_node_from_graph_removes_node_and_edges() {
        let client_id = 101;
        let drone_to_remove_id = 1;
        let drone_neighbor1_id = 2;
        let drone_neighbor2_id = 3;
        let server_id = 200;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers, _gui_input, _shortcut_tx) = setup_client(client_id, vec![]);

        //- - - - preparing environment for the test - - - -
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone_to_remove_idx = client.network_graph.add_node(NodeInfo { id: drone_to_remove_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone_to_remove_id, drone_to_remove_idx);
        let drone_neighbor1_idx = client.network_graph.add_node(NodeInfo { id: drone_neighbor1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone_neighbor1_id, drone_neighbor1_idx);
        let drone_neighbor2_idx = client.network_graph.add_node(NodeInfo { id: drone_neighbor2_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone_neighbor2_id, drone_neighbor2_idx);
        let server_idx = client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(server_id, server_idx);
        client.network_graph.add_edge(client_idx, drone_to_remove_idx, 0);
        client.network_graph.add_edge(drone_to_remove_idx, client_idx, 0);
        client.network_graph.add_edge(drone_to_remove_idx, drone_neighbor1_idx, 0);
        client.network_graph.add_edge(drone_neighbor1_idx, drone_to_remove_idx, 0);
        client.network_graph.add_edge(drone_to_remove_idx, drone_neighbor2_idx, 0);
        client.network_graph.add_edge(drone_neighbor2_idx, drone_to_remove_idx, 0);
        client.network_graph.add_edge(drone_to_remove_idx, server_idx, 0);
        client.network_graph.add_edge(server_idx, drone_to_remove_idx, 0);
        client.network_graph.add_edge(client_idx, drone_neighbor1_idx, 0);
        client.network_graph.add_edge(drone_neighbor1_idx, client_idx, 0);

        let initial_node_count = client.network_graph.node_count();
        let initial_edge_count = client.network_graph.edge_count();
        let expected_edges_removed = 8;

        //- - - - verifying - - - -
        assert!(client.node_id_to_index.contains_key(&drone_to_remove_id), "the node to be removed, should be initially present in node_id_to_index");
        assert!(client.network_graph.node_weights().any(|ni| ni.id == drone_to_remove_id), "the node to be removed, should be initially present in the initial graph");
        assert!(client.network_graph.contains_edge(client_idx, drone_to_remove_idx), "the link client->drone_to_remove should exist initially");
        assert!(client.network_graph.contains_edge(drone_to_remove_idx, drone_neighbor1_idx), "the link drone_to_remove->drone_neighbor1 should exist initially");

        client.remove_node_from_graph(drone_to_remove_id);

        //verifying that the node and all its edges has been removed
        assert!(!client.node_id_to_index.contains_key(&drone_to_remove_id), "the node shouldn't be in node_id_to_index after removing it");
        assert!(!client.network_graph.node_weights().any(|ni| ni.id == drone_to_remove_id), "the node shouldn't be in the graph after removing it");
        assert_eq!(client.network_graph.node_count(), initial_node_count - 1, "the number of nodes should be decremented by 1");
        assert_eq!(client.network_graph.edge_count(), initial_edge_count - expected_edges_removed, "the number of edges should be decremented correctly");

        //verifying that the specific edges, which involve the removed node, don't exist anymore
        assert!(!client.network_graph.contains_edge(client_idx, drone_to_remove_idx), "edge client->drone_to_remove shouldn't exist anymore");
        assert!(!client.network_graph.contains_edge(drone_to_remove_idx, client_idx), "edge drone_to_remove->client shouldn't exist anymore");
        assert!(!client.network_graph.contains_edge(drone_to_remove_idx, drone_neighbor1_idx), "edge drone_to_remove->drone_neighbor1 shouldn't exist anymore");
        assert!(!client.network_graph.contains_edge(drone_neighbor1_idx, drone_to_remove_idx), "edge drone_neighbor1->drone_to_remove shouldn't exist anymore");
        assert!(!client.network_graph.contains_edge(drone_to_remove_idx, drone_neighbor2_idx), "edge drone_to_remove->drone_neighbor2 shouldn't exist anymore");
        assert!(!client.network_graph.contains_edge(drone_neighbor2_idx, drone_to_remove_idx), "edge drone_neighbor2->drone_to_remove shouldn't exist anymore");
        assert!(!client.network_graph.contains_edge(drone_to_remove_idx, server_idx), "edge drone_to_remove->server shouldn't exist anymore");
        assert!(!client.network_graph.contains_edge(server_idx, drone_to_remove_idx), "edge server->drone_to_remove shouldn't exist anymore");

        //verifying that other edges aren't removed
        assert!(client.network_graph.contains_edge(client_idx, drone_neighbor1_idx), "edge client->drone_neighbor1 should still exist");
        assert!(client.network_graph.contains_edge(drone_neighbor1_idx, client_idx), "edge drone_neighbor1->client should still exist");
    }

    #[test]
    fn test_login_logout_updates_state_and_sends_messages() {
        let client_id = 101;
        let server_id = 200;
        let drone1_id = 1;
        let drone2_id = 2;

        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }

        let (client_to_d1_tx, d1_from_client_rx) = unbounded::<Packet>();
        let (d1_to_client_tx, client_from_d1_rx) = unbounded::<Packet>();
        let (d1_to_d2_tx, d2_from_d1_rx) = unbounded::<Packet>();
        let (d2_to_d1_tx, d1_from_d2_rx) = unbounded::<Packet>();
        let (d2_to_server_tx, server_from_d2_rx) = unbounded::<Packet>();
        let (server_to_d2_tx, d2_from_server_rx) = unbounded::<Packet>();

        let mut initial_shared_senders_map = HashMap::new();
        initial_shared_senders_map.insert((client_id, drone1_id), client_to_d1_tx.clone());
        initial_shared_senders_map.insert((drone1_id, client_id), d1_to_client_tx.clone());
        initial_shared_senders_map.insert((drone1_id, drone2_id), d1_to_d2_tx.clone());
        initial_shared_senders_map.insert((drone2_id, drone1_id), d2_to_d1_tx.clone());
        initial_shared_senders_map.insert((drone2_id, server_id), d2_to_server_tx.clone());
        initial_shared_senders_map.insert((server_id, drone2_id), server_to_d2_tx.clone());

        let (mut client, _client_incoming_packet_tx, mut neighbor_outbound_receivers, _gui_input, _shortcut_tx) =
            setup_client_with_custom_shared_senders(client_id, vec![drone1_id], initial_shared_senders_map);

        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone1_idx = client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone1_id, drone1_idx);
        let drone2_idx = client.network_graph.add_node(NodeInfo { id: drone2_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone2_id, drone2_idx);
        let server_idx = client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(server_id, server_idx);

        client.network_graph.add_edge(client_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone1_idx, client_idx, 0);
        client.network_graph.add_edge(drone1_idx, drone2_idx, 0);
        client.network_graph.add_edge(drone2_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone2_idx, server_idx, 0);
        client.network_graph.add_edge(server_idx, drone2_idx, 0);

        assert_eq!(client.connected_server_id, None, "client shouldn't be connected to a server initially");

        let login_command_string = format!("[Login]::{}", server_id);
        let initial_session_counter = *SESSION_COUNTER.lock().unwrap();
        client.process_gui_command(client_id, login_command_string);

        assert_eq!(client.connected_server_id, Some(server_id), "id of the connected server should be updated after the Login command");

        let expected_session_id_login = initial_session_counter + 1;
        let drone1_receiver = neighbor_outbound_receivers.get_mut(&drone1_id).unwrap();

        let expected_login_message_content = format!("[Login]::{}", server_id);
        let expected_login_fragments = (expected_login_message_content.as_bytes().len() + 128 - 1) / 128;
        assert_eq!(expected_login_fragments, 1, "the Login message should be 1 fragment long");

        let received_login_packet = drone1_receiver.recv_timeout(Duration::from_millis(100))
            .expect("should receive a fragment for the Login command");

        match received_login_packet.pack_type {
            PacketType::MsgFragment(fragment) => {
                assert_eq!(received_login_packet.session_id, expected_session_id_login, "wrong session_id for the Login fragment");
                assert_eq!(received_login_packet.routing_header.hops, vec![client_id, drone1_id, drone2_id, server_id], "wrong path hops for the Login fragment");
                assert_eq!(received_login_packet.routing_header.hop_index, 1, "hop_index for the Login fragment must be equal to 1");
                let reassembled_data = fragment.data[..fragment.length as usize].to_vec();
                let reassembled_message = String::from_utf8_lossy(&reassembled_data);
                assert_eq!(reassembled_message, expected_login_message_content, "content of the reassembled Login message doesn't correspond");
            },
            _ => panic!("expected MsgFragment for the Login, but received different packet type: {:?}", received_login_packet.pack_type),
        }

        assert_eq!(client.connected_server_id, Some(server_id), "client should be still connected before the Logout test");

        let logout_command_string = "[Logout]".to_string();
        let initial_session_counter_logout = *SESSION_COUNTER.lock().unwrap();
        client.process_gui_command(client_id, logout_command_string);

        assert_eq!(client.connected_server_id, None, "id of the connected server should be None after the Logout command");

        let expected_session_id_logout = initial_session_counter_logout + 1;
        let expected_logout_message_content = "[Logout]".to_string();
        let expected_logout_fragments = (expected_logout_message_content.as_bytes().len() + 128 - 1) / 128;
        assert_eq!(expected_logout_fragments, 1, "the Logout message should be 1 fragment long");

        let received_logout_packet = drone1_receiver.recv_timeout(Duration::from_millis(100))
            .expect("should receive a fragment packet for the Logout command");

        match received_logout_packet.pack_type {
            PacketType::MsgFragment(fragment) => {
                assert_eq!(received_logout_packet.session_id, expected_session_id_logout, "wrong session_id for the Logout fragment");
                assert_eq!(received_logout_packet.routing_header.hops, vec![client_id, drone1_id, drone2_id, server_id], "wrong hops path for the Logout fragment");
                assert_eq!(received_logout_packet.routing_header.hop_index, 1, "hop_index for the Logout fragment should be equal to 1");
                let reassembled_data = fragment.data[..fragment.length as usize].to_vec();
                let reassembled_message = String::from_utf8_lossy(&reassembled_data);
                assert_eq!(reassembled_message, expected_logout_message_content, "content of the reassembled Logout message doesn't correspond");
            },
            _ => panic!("expected MsgFragment for the Logout, but received different packet type: {:?}", received_logout_packet.pack_type),
        }

        assert_eq!(client.connected_server_id, None, "client should be disconnected for this part of the test");
        while drone1_receiver.try_recv().is_ok() {} // Svuota il canale
        let initial_session_counter_no_logout = *SESSION_COUNTER.lock().unwrap();
        client.process_gui_command(client_id, "[Logout]".to_string());
        assert_eq!(client.connected_server_id, None, "id of the connected server should remain None if client not logged in");
        assert!(drone1_receiver.try_recv().is_err(), "no packet should be sent when logging out while not connected");
        assert_eq!(*SESSION_COUNTER.lock().unwrap(), initial_session_counter_no_logout, "session counter shouldn't increment if the Logout command is ignored");
    }

    #[test]
    fn test_send_ack_sends_correct_packet_to_next_hop() {
        let client_id = 101;
        let drone1_id = 1;
        let drone2_id = 2;
        let server_id = 200;

        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }

        let (mut client, _client_incoming_packet_tx, mut neighbor_outbound_receivers, _gui_input, _shortcut_tx) =
            setup_client(client_id, vec![drone1_id]);

        let session_id = 12345;
        let fragment_index = 0;
        let total_fragments = 1;
        let fragment_data = [0u8; 128];
        let fragment_len = 10;

        let received_fragment = Fragment {
            fragment_index,
            total_n_fragments: total_fragments,
            length: fragment_len as u8,
            data: fragment_data,
        };

        let received_packet_routing_header = SourceRoutingHeader {
            hops: vec![server_id, drone2_id, drone1_id, client_id],
            hop_index: 3,
        };

        let mut received_packet = Packet {
            pack_type: PacketType::MsgFragment(received_fragment.clone()),
            routing_header: received_packet_routing_header.clone(),
            session_id,
        };

        client.send_ack(&mut received_packet, &received_fragment);

        let drone1_receiver = neighbor_outbound_receivers
            .get_mut(&drone1_id)
            .expect("The client should have a sender toward drone1_id");

        let sent_ack_packet = drone1_receiver
            .recv_timeout(Duration::from_millis(100))
            .expect("The ACK packet should have been sent to drone1_id");

        match sent_ack_packet.pack_type {
            PacketType::Ack(ack) => {
                assert_eq!(ack.fragment_index, fragment_index, "The ACK fragment index does not match");
                assert_eq!(sent_ack_packet.session_id, session_id, "The session_id of the ACK does not match");

                let mut expected_ack_hops = received_packet_routing_header.hops.clone();
                expected_ack_hops.reverse();
                assert_eq!(sent_ack_packet.routing_header.hops, expected_ack_hops, "The hops in the ACK's routing header do not match the reverse path");
                assert_eq!(sent_ack_packet.routing_header.hop_index, 1, "The hop_index in the ACK's routing header should be 1");
            }
            _ => panic!("An ACK packet was expected, but a different packet type was received: {:?}", sent_ack_packet.pack_type),
        }

        assert!(drone1_receiver.try_recv().is_err(), "Only one ACK packet was expected for the neighbor");
    }
}