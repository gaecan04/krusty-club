use std::collections::{HashMap, HashSet};
use std::time::Instant;
//use std::io::Write;
use std::process::Command;
use std::env::temp_dir;
use std::sync::Mutex;
//use std::path::PathBuf;
use once_cell::sync::Lazy;
use base64::{Engine, engine::general_purpose::STANDARD};
use image::load_from_memory;
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
//use wg_2024::packet::PacketType::MsgFragment;
//use wg_2024::drone::Drone;
use crossbeam_channel::{select_biased, Receiver, Sender};
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
pub struct NodeInfo {
    pub id : NodeId,
    pub node_type : NodeType,
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
}


#[derive(Debug, Clone)]
pub struct FloodDiscoveryState {
    pub initiator_id : NodeId,
    pub start_time : Instant,
    pub received_responses : Vec<FloodResponse>,
}


impl MyClient {
    pub(crate) fn new(
        id: NodeId,
        packet_recv: Receiver<Packet>,
        packet_send: HashMap<NodeId, Sender<Packet>>,
        sent_messages: HashMap<u64, SentMessageInfo>,
        connected_server_id: Option<NodeId>,
        seen_flood_ids : HashSet<(u64, NodeId)>,
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
        }
    }


    pub(crate) fn run(&mut self, gui_input: SharedGuiInput) {
        println!("Client {} starting run loop.", self.id);

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
                       println!("Packet received by client {} : {:?}", self.id, packet);
                       self.process_packet(packet);
                   }
               },
               default => {
                       std::thread::sleep(std::time::Duration::from_millis(1));
               },
           }
        }
    }


    fn check_flood_discoveries_timeouts(&mut self) {
        let now = Instant::now();
        let timeout_duration = std::time::Duration::from_millis(2000);
        let floods_to_finalize: Vec<u64> = self.active_flood_discoveries.iter().filter(|(_flood_id, state)| now.duration_since(state.start_time) > timeout_duration).map(|(_flood_id, state)| *_flood_id).collect();
        for flood_id in floods_to_finalize {
            println!("Client {} finalizing flood discovery for ID {} due to timeout.", self.id, flood_id);
            self.finalize_flood_discovery_topology(flood_id);
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
            PacketType::FloodRequest(request) => {
                //println!("Client {} received FloodRequest {}, ignoring as client should not propagate.", self.id, request.flood_id);
                self.process_flood_request(&request);
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
                        println!("Client {} received all ACKs for session {}. Message considered successfully sent.", self.id, packet.session_id);
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


    fn process_flood_request(&mut self, request: &FloodRequest) {
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
                            Ok(()) => println!("Client {} sent FloodResponse {} back to {} (prev hop).", self.id, request.flood_id, prev_hop_id),
                            Err(e) => eprintln!("Client {} failed to send FloodResponse {} back to {}: {}", self.id, request.flood_id, prev_hop_id, e),
                        }
                    } else {
                        eprintln!("Client {} error: no sender found for previous hop {}", self.id, prev_hop_id);
                    }
                } else {
                    eprintln!("Client {} cannot send FloodResponse back, path trace too short after adding self", self.id);
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
                        Ok(_) => println!("Client {} forwarded FloodRequest {} to neighbor {}.", self.id, request.flood_id, neighbor_id),
                        Err(e) => eprintln!("Client {} failed to forward FloodRequest {} to neighbor {}: {}", self.id, request.flood_id, neighbor_id, e),
                    }
                }
            }
        }
    }




    fn start_flood_discovery(&mut self) {
        println!("Client {} starting flood discovery.", self.id);
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

        println!("Client {} sending FloodRequest {} to all neighbors.", self.id, new_flood_id);
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
                println!("Message for session {} reassembled successfully.", session_id);
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
                self.process_received_high_level_message(message_string, session_id);
            }
        } else {
            eprintln!("Received fragment {} for session {} with offset {} and length {} which exceeds excepted message size {}. Ignoring fragment.", fragment.fragment_index, session_id, offset, fragment_len, state.data.len());
        }
    }


    //function in which the correctly reassembled high level messages are elaborated
    fn process_received_high_level_message(&mut self, message_string: String, session_id: u64) {
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
                    println!("Client {} received chat message from client {}. Message content: {}", self.id, sender_id, content);
                } else {
                    eprintln!("Client {} received MessageFrom with invalid sender_id: {}.", self.id, sender_id_str);
                }
            },
            ["[ClientListResponse]", list_str] => {
                //format: [ClientListResponse]::[client1_id, client2_id, ...]
                let cleaned = list_str.trim_matches(|c| c == '[' || c == ']').trim_end_matches(|c: char| c == '\0' || c == ' ' || c == '\n' || c == '\r' || c == '\t');
                let client_ids: Vec<NodeId> = cleaned.split(',').filter_map(|s| {
                    let cleaned : String = s.chars().filter(|&c| c != '\0' && c != ']').collect();
                    let trimmed = cleaned.trim();
                    trimmed.parse::<NodeId>().ok()
                }).collect();
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
                        //- - - - logic for showing the image - - - -
                        match STANDARD.decode(&base64_data) {
                            Ok(image_bytes) => {
                                println!("Client {} successfully decoded base64 data for media '{}'.", self.id, media_name);
                                //load_from_memory detects common image formats
                                match load_from_memory(&image_bytes) {
                                    Ok(img) => {
                                        println!("Client {} successfully loaded image data for media '{}'.", self.id, media_name);
                                        let mut temp_file_path = temp_dir();
                                        //guessing the extension format
                                        let extension = match img.color() {
                                            image::ColorType::Rgb8 => "png",
                                            image::ColorType::Rgba8 => "png",
                                            image::ColorType::L8 => "png",
                                            image::ColorType::La8 => "png",
                                            image::ColorType::Rgb16 => "png",
                                            image::ColorType::Rgba16 => "png",
                                            image::ColorType::L16 => "png",
                                            image::ColorType::La16 => "png",
                                            _ => "bin",
                                        };
                                        let cleaned_media_name = media_name.replace(|c : char| !c.is_ascii_alphanumeric() && c != ' ' && c != '-', "");
                                        let filename = format!("media_{}.{}", cleaned_media_name, extension);
                                        temp_file_path.push(filename);
                                        match img.save(&temp_file_path) {
                                            Ok(_) => {
                                                println!("Client {} successfully saved media '{}' to temporary file: {:?}", self.id, media_name, temp_file_path);
                                                let open_command = if cfg!(target_os = "windows") {
                                                    "start"
                                                } else if cfg!(target_os = "macos") {
                                                    "open"
                                                } else {
                                                    "xdg-open" //Linux
                                                };
                                                let path_str = temp_file_path.to_string_lossy().into_owned();
                                                match Command::new(open_command).arg(&path_str).spawn() {
                                                    Ok(_) => println!("Client {} successfully opened media '{}' with command '{} {}'.", self.id, media_name, open_command, path_str),
                                                    Err(e) => eprintln!("Client {} failed to open media '{}' using command '{} {}': {}", self.id, media_name, open_command, path_str, e),
                                                }
                                            },
                                            Err(e) => eprintln!("Client {} failed to save image data for media '{}' to file: {}", self.id, media_name, e),
                                        }
                                    },
                                    Err(e) => eprintln!("Client {} failed to load image from bytes for media '{}': {}", self.id, media_name, e),
                                }
                            },
                            Err(e) => eprintln!("Client {} failed to decode base64 data for media '{}': {}", self.id, media_name, e),
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
                    println!("Client {} received LOGIN ACK for session {}. Successfully logged in!", self.id, parsed_session_id);
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
        //ack_routing_header.hop_index = 1;

        let destination_node_id = ack_routing_header.hops.first().copied();
        let next_hop_for_ack_option = ack_routing_header.hops.get(1).copied();
        if let (Some(next_hop_for_ack), Some(_destination_node_id)) = (next_hop_for_ack_option, destination_node_id) {
            let ack_packet_to_send = Packet {
                pack_type: PacketType::Ack(Ack { fragment_index: fragment.fragment_index }),
                routing_header: ack_routing_header,
                session_id: packet.session_id,
            };
            if let Some(sender) = self.packet_send.get(&next_hop_for_ack) {
                match sender.send(ack_packet_to_send.clone()) {
                    Ok(()) => println!("Client {} sent ACK for session {} fragment {} to neighbor {}.", self.id, packet.session_id, fragment.fragment_index, next_hop_for_ack),
                    Err(e) => eprintln!("Client {} error sending ACK packet for session {} to neighbor {}: {}. Using ControllerShortcut.", self.id, packet.session_id, next_hop_for_ack, e),
                }
            } else {
                eprintln!("Client {} error: no sender found for ACK next hop {}. Attempting to use ControllerShortcut.", self.id, next_hop_for_ack);
            }
        } else {
            eprintln!("Client {} error: cannot send ACK, original routing header hops too short: {:?}. Cannot use ControllerShortcut.", self.id, packet.routing_header.hops);
        }
    }


    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        println!("Client {} processing NACK type {:?} for session {}.", self.id, nack.nack_type, packet.session_id);
        match &nack.nack_type {
            NackType::ErrorInRouting(problem_node_id) => {
                eprintln!("Client {} received ErrorInRouting NACK for session {} at node {}.", self.id, packet.session_id, problem_node_id);
                self.remove_node_from_graph(*problem_node_id);
                println!("Client {} removed node {} from graph due to ErrorInRouting.", self.id, problem_node_id);
                let sent_msg_info = self.sent_messages.get_mut(&packet.session_id);
                if let Some(info) = sent_msg_info {
                    info.route_needs_recalculation = true;
                    println!("Client {} marked route for session {} for recalculation due to ErrorInRouting.", self.id, packet.session_id);
                    if let Some(&dest_id) = info.original_routing_header.hops.last() {
                        self.route_cache.remove(&dest_id);
                        println!("Client {} invaliding cached route for {} due to ErrorInRouting NACK.", self.id, dest_id);
                    }
                } else {
                    eprintln!("Client {} received ErrorInRouting NACK for unknown session {}.", self.id, packet.session_id);
                }
            }
            NackType::DestinationIsDrone => {
                eprintln!("Client {} received DestinationIsDrone NACK for session {}.", self.id, packet.session_id);
                let sent_msg_info = self.sent_messages.get_mut(&packet.session_id);
                if let Some(info) = sent_msg_info {
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
                let nack_route = &packet.routing_header.hops;
                let nack_hop_index = packet.routing_header.hop_index;
                if nack_hop_index > 0 && nack_hop_index < nack_route.len() {
                    let problem_link_from = nack_route[nack_hop_index - 1];
                    let problem_link_to = *received_at_node_id;
                    println!("Client {} penalizing link {} -> {} due to UnexpectedRecipient.", self.id, problem_link_from, problem_link_to);
                    self.increment_drop(problem_link_from, problem_link_to);
                    self.increment_drop(problem_link_to, problem_link_from);
                } else {
                    eprintln!("Client {} received UnexpectedRecipient NACK with invalid routing header/hop index {:?}", self.id, packet.routing_header);
                }
                let sent_msg_info = self.sent_messages.get_mut(&packet.session_id);
                if let Some(info) = sent_msg_info {
                    info.route_needs_recalculation = true;
                    println!("Client {} marked route for session {} for recalculation due to UnexpectedRecipient.", self.id, packet.session_id);
                    if let Some(&dest_id) = info.original_routing_header.hops.last() {
                        self.route_cache.remove(&dest_id);
                        println!("Client {} invaliding cached route for {} due to UnexpectedRecipient NACK.", self.id, dest_id);
                    }
                } else {
                    eprintln!("Client {} received UnexpectedRecipient NACK for unknown session {}.", self.id, packet.session_id);
                }
            }
            NackType::Dropped => {
                eprintln!("Client {} received Dropped NACK for session {}, fragment {}.", self.id, packet.session_id, nack.fragment_index);
                let resend_info = self.sent_messages.get(&packet.session_id).and_then(|original_packet_info| {
                    original_packet_info.fragments.iter().find(|f| f.fragment_index == nack.fragment_index).map(|fragment_to_resend| {
                        (fragment_to_resend.clone(), original_packet_info.original_routing_header.clone())
                    })
                });
                if let Some((fragment_to_resend, original_header)) = resend_info {
                    let nack_route = &packet.routing_header.hops;
                    let nack_hop_index = packet.routing_header.hop_index;
                    if nack_hop_index > 0 && nack_hop_index < nack_route.len() {
                        let from_node = nack_route[nack_hop_index - 1];
                        let dropped_at_node = nack_route[nack_hop_index];
                        println!("Client {} penalizing link {} -> {} due to Dropped NACK.", self.id, from_node, dropped_at_node);
                        self.increment_drop(from_node, dropped_at_node);
                        self.increment_drop(dropped_at_node, from_node);
                    } else {
                        eprintln!("Client {} cannot determine dropping link: Original routing header hop_index is out of bounds for session {}.", self.id, packet.session_id);
                    }
                    let packet_to_resend = Packet {
                        pack_type: PacketType::MsgFragment(fragment_to_resend.clone()),
                        routing_header: original_header,
                        session_id: packet.session_id,
                    };
                    if packet_to_resend.routing_header.hops.len() > packet_to_resend.routing_header.hop_index {
                        let first_hop = packet_to_resend.routing_header.hops[packet_to_resend.routing_header.hop_index];
                        match self.send_to_neighbor(first_hop, packet_to_resend) {
                            Ok(_) => println!("Client {} resent fragment {} for session {} to {}.", self.id, fragment_to_resend.fragment_index, packet.session_id, first_hop),
                            Err(e) => eprintln!("Client {} failed to resend fragment {} for session {} to {}: {}", self.id, fragment_to_resend.fragment_index, packet.session_id, first_hop, e),
                        }
                    } else {
                        eprintln!("Client {} cannot resend fragment {}: Original routing header has no valid first hop for session {}", self.id, fragment_to_resend.fragment_index, packet.session_id);
                    }
                } else {
                    eprintln!("Client {} received dropped nack for session {}, fragment {} but could not find fragment in sent_messages", self.id, packet.session_id, nack.fragment_index); // [32, 34]
                }
            }
        }
    }


    fn create_topology(&mut self, flood_response: &FloodResponse) {
        println!("Client {} processing FloodResponse for flood_id {}.", self.id, flood_response.flood_id);
        let path = &flood_response.path_trace;
        if path.is_empty() {
            println!("Received empty path_trace in FloodResponse for flood_id {}.", flood_response.flood_id);
            return;
        }
        let (initiator_id, initiator_type) = path.first().cloned().unwrap();
        if let std::collections::hash_map::Entry::Vacant(e) = self.node_id_to_index.entry(initiator_id) {
            let node_index = self.network_graph.add_node(NodeInfo { id: initiator_id, node_type: initiator_type });
            e.insert(node_index);
            println!("Client {} added initiator node {} ({:?}) to graph.", self.id, initiator_id, initiator_type);
        }
        for i in 0..path.len() {
            let (current_node_id, current_node_type) = path[i];
            if let std::collections::hash_map::Entry::Vacant(e) = self.node_id_to_index.entry(current_node_id) {
                let node_index = self.network_graph.add_node(NodeInfo { id: current_node_id, node_type: current_node_type });
                e.insert(node_index);
                println!("Client {} added node {} ({:?}) to graph.", self.id, current_node_id, current_node_type);
            }
            /*
            let current_node_idx = *self.node_id_to_index.entry(current_node_id).or_insert_with(|| {
                println!("Client {} adding node {} ({:?}) to graph", self.id, current_node_id, current_node_type);
                self.network_graph.add_node(NodeInfo { id : current_node_id, node_type : current_node_type } )
            });
            */
            if i > 0 {
                let (prev_node_id, _) = path[i - 1];
                let (current_node_id, _) = path[i];
                if let (Some(&prev_node_idx), Some(&current_node_idx)) = (self.node_id_to_index.get(&prev_node_id), self.node_id_to_index.get(&current_node_id)) {
                    let edge_exists = self.network_graph.contains_edge(current_node_idx, prev_node_idx) || self.network_graph.contains_edge(prev_node_idx, current_node_idx);
                    if !edge_exists {
                        println!("Client {} adding edge: {} -> {}.", self.id, prev_node_id, current_node_id);
                        self.network_graph.add_edge(prev_node_idx, current_node_idx, 0);
                        self.network_graph.add_edge(current_node_idx, prev_node_idx, 0);
                    } else {
                        println!("Client {}: edge {} -> {} already exists.", self.id, prev_node_id, current_node_id);
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
        let high_level_message_info: Option<String> = match tokens.as_slice() {
            ["[Login]", server_id] => {
                if let Ok(parsed_server_id) = server_id.parse::<NodeId>() {
                    println!("Client {} processing LOGIN command for server {}.", self.id, parsed_server_id);
                    self.connected_server_id = Some(parsed_server_id);
                    Some(format!("[Login]::{}",parsed_server_id))
                } else {
                    eprintln!("Client {} received LOGIN command with invalid server {}.", self.id, server_id);
                    None
                }
            },
            ["[Logout]"] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing LOGOUT command via server {}.", self.id, mem_server_id);
                    Some("[Logout]".to_string())
                } else {
                    eprintln!("Client {} received LOGOUT command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[ClientListRequest]"] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing CLIENT LIST REQUEST command via server {}.", self.id, mem_server_id);
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
                    Some(format!("[MediaUpload]::{media_name}::{base64_data}"))
                } else {
                    eprintln!("Client {} received MEDIA UPLOAD command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[MediaDownloadRequest]", media_name] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing MEDIA DOWNLOAD REQUEST command for media '{}' via server {}.", self.id, media_name, mem_server_id);
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
                    Some("[MediaListRequest]".to_string())
                } else {
                    eprintln!("Client {} received MEDIA LIST REQUEST command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[MediaBroadcast]", media_name, base64_data] => {
                if let Some(mem_server_id) = self.connected_server_id {
                    println!("Client {} processing MEDIA BROADCAST command for media '{}' via server {}.", self.id, media_name, mem_server_id);
                    Some(format!("[MediaBroadcast]::{media_name}::{base64_data}"))
                } else {
                    eprintln!("Client {} received MEDIA BROADCAST command while not logged in. Ignoring.", self.id);
                    None
                }
            },
            ["[FloodRequired]",action] => {
                println!("Client {} received FLOOD REQUIRED command due to action: {}.", self.id, action);
                self.start_flood_discovery();
                None
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
                        println!("Client {} computed route to server {}: {:?}", self.id, id_to_send_to, path);
                        self.route_cache.insert(id_to_send_to, path.clone());
                        path
                    },
                    None => {
                        eprintln!("Client {} could not compute route to server {}. Starting flood discovery.", self.id, dest_id);
                        let active_flood = self.active_flood_discoveries.values().any(|state| {
                            state.initiator_id == self.id && state.start_time.elapsed() < std::time::Duration::from_secs(5)
                        });
                        if !active_flood {
                            self.start_flood_discovery();
                        }
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
                    println!("Client {} successfully sent Logout message. Disconnecting internally.", self.id);
                    self.connected_server_id = None;
                }
            } else {
                eprintln!("Client {} cannot send message: not connected to any server.", self.id);
            }
        }
    }

    fn best_path(&self, from: NodeId, to: NodeId) -> Option<Vec<NodeId>> {
        println!("Client {} calculating path from {} to {}.", self.id, from, to);
        if from == self.id {
            if let Some(cached_path) = self.route_cache.get(&to) {
                println!("Client {}: found path from {} to {}: {:?}", self.id, from, to, cached_path);
                return Some(cached_path.clone());
            }
        }
        let start_node_idx = self.node_id_to_index.get(&from).copied();
        let end_node_idx = self.node_id_to_index.get(&to).copied();
        match (start_node_idx, end_node_idx) {
            (Some(start_idx), Some(end_idx)) => {
                let path_result = petgraph::algo::astar(
                    &self.network_graph,
                    start_idx,
                    |n| n == end_idx, //goal condition: is node n the target node?
                    |e| *e.weight(),     //edge cost: use the weight of the edge (usize)
                    |_| 0                //heuristic: use 0 for all nodes (makes A* equivalent to Dijkstra)
                );
                match path_result {
                    Some((_cost, node_indices_path)) => {
                        let node_ids_path: Vec<NodeId> = node_indices_path.iter().filter_map(|&idx| self.network_graph.node_weight(idx).map(|node_info| node_info.id)).collect();
                        //println!("Client {} found path from {} to {}: {:?}", self.id, from, to, node_ids_path);
                        if node_ids_path.is_empty() || node_ids_path.first().copied() != Some(from) || node_ids_path.last().copied() != Some(to) {
                            eprintln!("Client {} computed an invalid path structure from {} to {}: {:?}", self.id, from, to, node_ids_path);
                            None
                        } else {
                            Some(node_ids_path)
                        }
                    },
                    None => {
                        println!("Client {} could not find a path from {} to {}.", self.id, from, to);
                        None
                    }
                }
            },
            _ => {
                eprintln!("Client {} cannot calculate path: source ({}) or destination ({}) node not found in graph.", self.id, from, to);
                None
            }
        }
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


    fn send_to_neighbor(&mut self, neighbor_id: NodeId, packet: Packet) -> Result<(), String> {
        if let Some(sender) = self.packet_send.get(&neighbor_id) {
            match sender.send(packet.clone()) {
                Ok(()) => Ok(()),
                Err(e) => Err(format!("Failed to send packet to neighbor {}: {}", neighbor_id, e)),
            }
        } else {
            Err(format!("Neighbor {} not found in map.", neighbor_id))
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
    use std::sync::Mutex;
    use once_cell::sync::Lazy;
    use std::time::{Instant, Duration};

    fn has_edge(graph: &StableGraph<NodeInfo, usize>, a: NodeIndex, b: NodeIndex) -> bool {
        graph.contains_edge(a, b) // Delega la chiamata al metodo corretto di petgraph
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
        const MAX_FRAGMENT_SIZE: usize = 128;

        for pkt in packets {
            if let PacketType::MsgFragment(fragment) = pkt.pack_type {
                let start_index = (fragment.fragment_index * MAX_FRAGMENT_SIZE as u64) as usize;
                let end_index = start_index + fragment.length as usize;

                if reassembled_data.len() < end_index {
                    reassembled_data.resize(end_index, 0);
                }
                reassembled_data[start_index..end_index].copy_from_slice(&fragment.data[..fragment.length as usize]);
            }
        }
        String::from_utf8_lossy(&reassembled_data).trim_end_matches('\0').to_string()
    }

    fn setup_client (client_id: NodeId, neighbor_ids: Vec<NodeId>) -> (MyClient, Sender<Packet>, HashMap<NodeId, Receiver<Packet>>) {
        let (_packet_recv_tx, packet_recv_rx) = unbounded::<Packet>();
        let mut packet_send_map = HashMap::new();
        let mut neighbor_receivers = HashMap::new();
        for &id in &neighbor_ids {
            let (tx, rx) = unbounded::<Packet>();
            packet_send_map.insert(id, tx);
            neighbor_receivers.insert(id, rx);
        }
        //let gui_input = SharedGuiInput::new(Default::default());
        let client = MyClient::new(
            client_id,
            packet_recv_rx,
            packet_send_map,
            HashMap::new(),
            None,
            HashSet::new(),
        );
        (client, _packet_recv_tx, neighbor_receivers)
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
        let (mut client, _client_incoming_packet_tx, neighbor_receivers) = setup_client(client_id, neighbor_ids.clone());
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
        let (mut client, client_incoming_packet_tx, neighbor_outbound_receivers) =
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
        let (mut client, client_incoming_packet_tx, neighbor_outbound_receivers) =
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
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![]);
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
        let (mut client, client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![]);
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
        let (mut client, _client_incoming_packet_tx, _neighbor_outbond_receivers) = setup_client(client_id, vec![]);
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
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![]);
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

        //first path: 101 -> 1 (cost 0) -> 2 (cost 0) -> 200 (cost 0)
        client.network_graph.add_edge(*node_indices.get(&client_id).unwrap(), *node_indices.get(&1).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&1).unwrap(), *node_indices.get(&client_id).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&1).unwrap(), *node_indices.get(&2).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&2).unwrap(), *node_indices.get(&1).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&2).unwrap(), *node_indices.get(&200).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&200).unwrap(), *node_indices.get(&2).unwrap(), 0);
        //second path: 101 -> 3 (cost 0) -> 4 (cost 10) -> 200 (cost 0)
        client.network_graph.add_edge(*node_indices.get(&client_id).unwrap(), *node_indices.get(&3).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&3).unwrap(), *node_indices.get(&client_id).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&3).unwrap(), *node_indices.get(&4).unwrap(), 10);
        client.network_graph.add_edge(*node_indices.get(&4).unwrap(), *node_indices.get(&3).unwrap(), 10);
        client.network_graph.add_edge(*node_indices.get(&4).unwrap(), *node_indices.get(&200).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&200).unwrap(), *node_indices.get(&4).unwrap(), 0);
        //additional path: 1 -> 4 (cost 1) -> 200 (cost 0)
        client.network_graph.add_edge(*node_indices.get(&1).unwrap(), *node_indices.get(&4).unwrap(), 1);
        client.network_graph.add_edge(*node_indices.get(&4).unwrap(), *node_indices.get(&1).unwrap(), 1);

        //test case 1.1: verify the shortest path with costs between 101 and 200.
        let path_option = client.best_path(client_id, 200);
        assert!(path_option.is_some(), "should be found a path from 101 to 200");
        let expected_path = vec![client_id, 1, 2, 200];
        assert_eq!(path_option.unwrap(), expected_path, "the found path is not the shortest/has not the minimum cost");

        //test case 1.2: verify the structure of the path (starts with 'from' and ends with 'to').
        let path = client.best_path(client_id, 200);
        assert!(path.is_some(), "the path shouldn't be None");
        let unwrapped_path = path.unwrap();
        assert_eq!(*unwrapped_path.first().unwrap(), client_id, "the path must start with the source node");
        assert_eq!(*unwrapped_path.last().unwrap(), 200, "the path must end with the destination node");

        //test case 1.3: verify another shortest path (e.g. from 1 to 200).
        let path_1_to_200_option = client.best_path(1, 200);
        assert!(path_1_to_200_option.is_some(), "should be found a path from 1 to 200");
        /*
        let expected_path_1_to_200 : Vec<NodeId> = vec![];
        assert_eq!(path_1_to_200_option.unwrap(), expected_path_1_to_200, "the found path from 1 to 200 is not the shortest/has not the minimum cost");
         */
    }


    #[test]
    fn test_best_path_no_path_exists() {
        let client_id = 101;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![]);
        let nodes_info = vec![
            (client_id, PktNodeType::Client),
            (1, PktNodeType::Drone),
            (200, PktNodeType::Server),
            (201, PktNodeType::Server),
        ];
        let mut node_indices = HashMap::new();
        for (id, node_type) in nodes_info {
            let node_idx = client.network_graph.add_node(NodeInfo { id, node_type });
            client.node_id_to_index.insert(id, node_idx);
            node_indices.insert(id, node_idx);
        }

        client.network_graph.add_edge(*node_indices.get(&client_id).unwrap(), *node_indices.get(&1).unwrap(), 0);
        client.network_graph.add_edge(*node_indices.get(&1).unwrap(), *node_indices.get(&client_id).unwrap(), 0);

        let path = client.best_path(client_id, 200);
        assert!(path.is_none(), "it shouldn't be found a path between disconnected nodes");
    }


    #[test]
    fn test_best_path_source_node_not_in_graph() {
        let client_id = 101;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![]);
        let target_id = 200;
        let target_node_idx = client.network_graph.add_node(NodeInfo { id: target_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(target_id, target_node_idx);
        let path = client.best_path(235, target_id);
        assert!(path.is_none(), "best_path() should give None if the source node isn't in the graph");
    }


    #[test]
    fn test_best_path_destination_node_not_in_graph() {
        let client_id = 101;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![]);
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
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![]);
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
    fn test_high_level_message_handling() {
        let client_id = 101;
        let server_id = 200;
        let drone1_id = 1;
        let drone2_id = 2;

        //- - - - preparing the environment for the test - - - -
        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }
        let (mut client, _client_incoming_packet_tx, mut neighbor_outbound_receivers) =
            setup_client(client_id, vec![drone1_id]);
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
        client.connected_server_id = Some(server_id);


        //test 1: fragmenting and sending with appropriate routing_header
        let original_message_long = "hello, this is a test message which has to be fragmented because it's longer than 128 byte. It needs more fragments to be sent through the network. Let's make it long enough for 2 fragments, maybe 3. More text, more fragments! This part makes it sufficiently long for the test";
        let full_high_level_message_content = format!("[MessageTo]::{}::{}", client_id, original_message_long);
        let message_command_send = full_high_level_message_content.clone();
        let initial_session_counter_send = *SESSION_COUNTER.lock().unwrap();
        client.process_gui_command(client_id, message_command_send.clone());
        let expected_session_id_send = initial_session_counter_send + 1;
        let drone1_receiver = neighbor_outbound_receivers.remove(&drone1_id)
            .expect("receiver for drone1 shouldn't be empty");
        let mut received_sent_fragments = Vec::new();
        const FRAGMENT_SIZE: usize = 128;
        let expected_total_fragments_send = (full_high_level_message_content.as_bytes().len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        for _ in 0..expected_total_fragments_send {
            let packet = drone1_receiver.recv_timeout(Duration::from_millis(100))
                .expect("should receive a fragment packet");
            received_sent_fragments.push(packet);
        }
        assert_eq!(received_sent_fragments.len(), expected_total_fragments_send, "not all fragments has been sent");
        for (i, packet) in received_sent_fragments.iter().enumerate() {
            assert_eq!(packet.session_id, expected_session_id_send, "wrong session_id for the sent fragment");
            assert_eq!(packet.routing_header.hop_index, 1, "hop index should start from 1 for the sender");
            assert_eq!(packet.routing_header.hops, vec![client_id, drone1_id, drone2_id, server_id], "wrong hops");
            match &packet.pack_type {
                PacketType::MsgFragment(fragment) => {
                    assert_eq!(fragment.fragment_index, i as u64, "fragment indice not corresponding");
                    assert_eq!(fragment.total_n_fragments, expected_total_fragments_send as u64, "total number of fragments not corresponding");
                    let start = i * FRAGMENT_SIZE;
                    let end = (start + FRAGMENT_SIZE).min(full_high_level_message_content.as_bytes().len());
                    let expected_len = (end - start) as u8;
                    assert_eq!(fragment.length, expected_len, "length of the fragment not corresponding");
                    let expected_data_slice = &full_high_level_message_content.as_bytes()[start..end];
                    assert_eq!(&fragment.data[..expected_len as usize], expected_data_slice, "data of the fragment not corresponding");
                },
                _ => panic!("expected MsgFragment, received different packet type: {:?}", packet.pack_type),
            }
        }

        //test 2: reassembling (normal order)
        let receiving_client_id_normal = 102;
        let sender_id_normal = client_id;
        let (mut receiver_client_normal, receiver_incoming_tx_normal, _dummy_receivers_normal) =
            setup_client(receiving_client_id_normal, vec![]);
        let normal_reassembly_message = "this is a message for the reassembling in normal order. It is sufficiently long to guarantee more than one fragment. Please make this test work or I'll quit engineering";
        let reassembly_session_id_normal = expected_session_id_send + 1;
        let reassembly_total_fragments_normal = (normal_reassembly_message.as_bytes().len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        let message_bytes_normal = normal_reassembly_message.as_bytes();
        for i in 0..reassembly_total_fragments_normal {
            let start = i * FRAGMENT_SIZE;
            let end = (start + FRAGMENT_SIZE).min(message_bytes_normal.len());
            let fragment_len = (end - start) as u8;
            let mut data_arr = [0u8; FRAGMENT_SIZE];
            data_arr[..fragment_len as usize].copy_from_slice(&message_bytes_normal[start..end]);
            let fragment = Fragment {
                fragment_index: i as u64,
                total_n_fragments: reassembly_total_fragments_normal as u64,
                length: fragment_len,
                data: data_arr,
            };
            let packet = Packet {
                pack_type: PacketType::MsgFragment(fragment),
                routing_header: SourceRoutingHeader { hops: vec![sender_id_normal, receiving_client_id_normal], hop_index: 0 },
                session_id: reassembly_session_id_normal,
            };
            receiver_incoming_tx_normal.send(packet.clone()).unwrap();
            receiver_client_normal.process_packet(packet.clone());
        }
        assert!(!receiver_client_normal.received_messages.contains_key(&reassembly_session_id_normal), "the map of received messages should be empty after complete reassembling (normal order)");

        //test 3: handling duplicate fragments and Fuori Sequenza
        let ooo_client_id = 103;
        let ooo_sender_id = client_id;
        let (mut ooo_client, ooo_incoming_tx, _dummy_receivers2) =
            setup_client(ooo_client_id, vec![]);
        let ooo_message = "this is a message to test fuori sequenza and duplicated fragments. It should be long enough to have more than one fragment. Please let this test work otherwise I must quit engineering";
        let ooo_session_id = reassembly_session_id_normal + 1;
        let ooo_total_fragments = (ooo_message.as_bytes().len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        let ooo_message_bytes = ooo_message.as_bytes();
        let mut fragments_to_send: Vec<Packet> = Vec::new();
        for i in 0..ooo_total_fragments {
            let start = i * FRAGMENT_SIZE;
            let end = (start + FRAGMENT_SIZE).min(ooo_message_bytes.len());
            let fragment_len = (end - start) as u8;
            let mut data_arr = [0u8; FRAGMENT_SIZE];
            data_arr[..fragment_len as usize].copy_from_slice(&ooo_message_bytes[start..end]);
            let fragment = Fragment {
                fragment_index: i as u64,
                total_n_fragments: ooo_total_fragments as u64,
                length: fragment_len,
                data: data_arr,
            };
            fragments_to_send.push(Packet {
                pack_type: PacketType::MsgFragment(fragment),
                routing_header: SourceRoutingHeader { hops: vec![ooo_sender_id, ooo_client_id], hop_index: 0 },
                session_id: ooo_session_id,
            });
        }
        assert!(ooo_total_fragments >= 2, "the message is too short for the fuori sequenza test, it needs at least 2 fragments");
        if ooo_total_fragments > 1 {
            ooo_incoming_tx.send(fragments_to_send[ooo_total_fragments - 1].clone()).unwrap();
            ooo_client.process_packet(fragments_to_send[ooo_total_fragments - 1].clone());
            assert!(ooo_client.received_messages.contains_key(&ooo_session_id), "the map should contain the session after the first fragment");
            assert_eq!(ooo_client.received_messages.get(&ooo_session_id).unwrap().received_indices.len(), 1, "it should have 1 received fragment");
        }
        for i in 0..ooo_total_fragments.saturating_sub(1) {
            if ooo_total_fragments > 1 && i == ooo_total_fragments - 1 {
                continue;
            }
            ooo_incoming_tx.send(fragments_to_send[i].clone()).unwrap();
            ooo_client.process_packet(fragments_to_send[i].clone());
        }
        assert!(!ooo_client.received_messages.contains_key(&ooo_session_id), "the map should be empty after the complete reassembling");
    }

    #[test]
    fn test_send_ack_and_client_ack_handling() {
        let client_id_sender = 101;
        let server_id = 200;
        let drone1_id = 1;
        let drone2_id = 2;

        //- - - - preparing the environment for the test - - - -
        let (mut sender_client, sender_incoming_packet_tx, mut sender_outbound_receivers) =
            setup_client(client_id_sender, vec![drone1_id]);
        let client_idx = sender_client.network_graph.add_node(NodeInfo { id: client_id_sender, node_type: PktNodeType::Client });
        sender_client.node_id_to_index.insert(client_id_sender, client_idx);
        let drone1_idx = sender_client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        sender_client.node_id_to_index.insert(drone1_id, drone1_idx);
        let drone2_idx = sender_client.network_graph.add_node(NodeInfo { id: drone2_id, node_type: PktNodeType::Drone });
        sender_client.node_id_to_index.insert(drone2_id, drone2_idx);
        let server_idx = sender_client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        sender_client.node_id_to_index.insert(server_id, server_idx);
        sender_client.network_graph.add_edge(client_idx, drone1_idx, 0);
        sender_client.network_graph.add_edge(drone1_idx, client_idx, 0);
        sender_client.network_graph.add_edge(drone1_idx, drone2_idx, 0);
        sender_client.network_graph.add_edge(drone2_idx, drone1_idx, 0);
        sender_client.network_graph.add_edge(drone2_idx, server_idx, 0);
        sender_client.network_graph.add_edge(server_idx, drone2_idx, 0);
        sender_client.connected_server_id = Some(server_id);

        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }

        //1.simulation sending a multi-fragmented message from client to receiver
        let original_message = "This is a test message, which should be fragmented to correctly test the handling of ACKs. It has to be sufficiently long to generate more fragments to let the test work.";
        let message_command = format!("[MessageTo]::{}::{}", server_id, original_message);
        sender_client.process_gui_command(server_id, message_command.clone());
        let sent_session_id = *SESSION_COUNTER.lock().unwrap();
        //verifying that the fragments are generated and memorized in `sent_messages`
        assert!(sender_client.sent_messages.contains_key(&sent_session_id), "the sender client should have memorized the information of the sent message");
        let sent_message_info = sender_client.sent_messages.get(&sent_session_id).unwrap();
        let total_fragments_expected = sent_message_info.fragments.len() as u64;
        assert!(total_fragments_expected > 1, "the message must be fragmented for this test (expected > 1 fragments)");
        assert_eq!(sent_message_info.received_ack_indices.len(), 0, "initially, no ACK should be received from sender client");

        let drone1_receiver_from_sender = sender_outbound_receivers.remove(&drone1_id)
            .expect("sender client should have an outbound channel towards drone1");
        let mut sent_fragments_from_sender: Vec<Packet> = Vec::new();
        for _ in 0..total_fragments_expected {
            sent_fragments_from_sender.push(drone1_receiver_from_sender.recv_timeout(Duration::from_millis(100)).expect("not all fragments received by the sender client"));
        }
        
        //2.simulation of the server (simulated by `mock_server_client`) which will receive the fragments and it'll send ACK
        let (mut mock_server_client, _mock_server_incoming_tx, mut mock_server_outbound_receivers) =
            setup_client(server_id, vec![drone2_id]);
        let drone2_receiver_from_mock_server = mock_server_outbound_receivers.remove(&drone2_id)
            .expect("mock server should have an outbound channel towards drone2");
        let mut ack_packets_to_send_back_to_sender: Vec<Packet> = Vec::new();
        for i in 0..total_fragments_expected {
            let original_fragment_packet = sent_fragments_from_sender[i as usize].clone();
            let mut packet_at_server = original_fragment_packet.clone();
            packet_at_server.routing_header.hop_index = packet_at_server.routing_header.hops.len();
            mock_server_client.process_packet(packet_at_server);
            //catch the ACK packet sent from mock server to drone2
            let ack_packet_from_mock_server_to_drone2 = drone2_receiver_from_mock_server.recv_timeout(Duration::from_millis(100))
                .expect(&format!("mock server should send ACK for the fragment {}", i));

            //- - - - verifying generated ACK packet - - - -
            match ack_packet_from_mock_server_to_drone2.clone().pack_type {
                PacketType::Ack(ack) => {
                    assert_eq!(ack.fragment_index, i, "the index of the ACK fragment doesn't correspond");
                    let expected_reversed_hops: Vec<NodeId> = original_fragment_packet.routing_header.hops
                        .iter()
                        .rev()
                        .copied()
                        .collect();
                    assert_eq!(ack_packet_from_mock_server_to_drone2.routing_header.hops, expected_reversed_hops, "hops of the ACK should be the inverse path of the original message");
                },
                _ => panic!("expected ACK packet, but received different packet type: {:?}", ack_packet_from_mock_server_to_drone2.pack_type),
            }
            ack_packets_to_send_back_to_sender.push(ack_packet_from_mock_server_to_drone2);
        }
        
        //3.simulation of the sender client which receives ACK and updates its state
        for ack_packet_original_from_mock_server in ack_packets_to_send_back_to_sender {
            let mut ack_packet_at_sender_client = ack_packet_original_from_mock_server.clone();
            ack_packet_at_sender_client.routing_header.hop_index = ack_packet_at_sender_client.routing_header.hops.len();

            sender_incoming_packet_tx.send(ack_packet_at_sender_client.clone()).unwrap();
            sender_client.process_packet(ack_packet_at_sender_client.clone());
        }

        //- - - - verifying the final state of the messages sent by the sender client - - - -
        let final_sent_message_info = sender_client.sent_messages.get(&sent_session_id).unwrap();
        assert_eq!(final_sent_message_info.received_ack_indices.len() as u64, total_fragments_expected, "all the fragments should be marked as ACKed");
    }

    #[test]
    fn test_process_nack_error_in_routing() {
        let client_id = 101;
        let drone1_id = 1;
        let drone2_id = 2;
        let server_id = 200;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![drone1_id]);

        //- - - - preparing the environment for the test - - - -
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone1_idx = client.network_graph.add_node(NodeInfo { id: drone1_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone1_id, drone1_idx);
        let drone2_idx = client.network_graph.add_node(NodeInfo { id: drone2_id, node_type: PktNodeType::Drone }); //crashed node
        client.node_id_to_index.insert(drone2_id, drone2_idx);
        let server_idx = client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(server_id, server_idx);
        client.network_graph.add_edge(client_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone1_idx, client_idx, 0);
        client.network_graph.add_edge(drone1_idx, drone2_idx, 0); //link of crashed drone
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
        assert!(client.route_cache.contains_key(&server_id), "route towards server should be initially in cache");
        assert!(client.node_id_to_index.contains_key(&problem_node_id), "the problematic node should exist in the graph before the NACK");

        client.process_nack(&nack, &mut packet); // [5]

        //- - - - verifying - - - -
        //1.that the problematic node is removed from graph
        assert!(!client.node_id_to_index.contains_key(&problem_node_id), "the problematic node should be removed from graph");
        assert!(!client.network_graph.node_weights().any(|ni| ni.id == problem_node_id), "the node shouldn't be present in the weights of the graph");

        //2.that the route for this session has been marked for the recalculation
        assert!(client.sent_messages.get(&session_id).unwrap().route_needs_recalculation, "route for the session should be marked for the recalculation");

        //3.that the route in cache for the destination (server_id) is invalidated
        assert!(!client.route_cache.contains_key(&server_id), "route in cache for the destination should be removed");
    }

    #[test]
    fn test_process_nack_destination_is_drone() {
        let client_id = 101;
        let drone1_id = 1;
        let drone_destination_id = 50;
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![drone1_id]);

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
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![drone1_id, drone_actual_recipient]);

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
        client.network_graph.add_edge(drone1_idx, drone_actual_idx, 0); //penalized drone
        client.network_graph.add_edge(drone_actual_idx, drone1_idx, 0);
        client.network_graph.add_edge(drone_expected_idx, server_idx, 0);
        client.network_graph.add_edge(server_idx, drone_expected_idx, 0);
        let session_id = 910;
        let nack_packet_id = 0;
        let received_at_node_id = drone_actual_recipient;

        let original_rh = SourceRoutingHeader {
            hops: vec![client_id, drone1_id, drone_expected_recipient, server_id],
            hop_index: 2,
        };
        let nack_type = NackType::UnexpectedRecipient(received_at_node_id);
        let nack = Nack { fragment_index: nack_packet_id, nack_type: nack_type.clone() };
        let mut packet = Packet { pack_type: PacketType::Nack(nack.clone()), routing_header: original_rh.clone(), session_id };
        client.sent_messages.insert(session_id, SentMessageInfo {
            fragments: vec![],
            original_routing_header: original_rh.clone(),
            received_ack_indices: HashSet::new(),
            route_needs_recalculation: false,
        });

        client.route_cache.insert(server_id, vec![client_id, drone1_id, drone_expected_recipient, server_id]);
        assert!(client.route_cache.contains_key(&server_id), "route towards server should be initially in cache");

        let problem_link_from_idx = *client.node_id_to_index.get(&drone1_id).unwrap();
        let problem_link_to_idx = *client.node_id_to_index.get(&drone_actual_recipient).unwrap();
        let initial_weight_forward = *client.network_graph.edge_weight(client.network_graph.find_edge(problem_link_from_idx, problem_link_to_idx).unwrap()).unwrap();
        let initial_weight_backward = *client.network_graph.edge_weight(client.network_graph.find_edge(problem_link_to_idx, problem_link_from_idx).unwrap()).unwrap();
        assert_eq!(initial_weight_forward, 0);
        assert_eq!(initial_weight_backward, 0);

        client.process_nack(&nack, &mut packet); // [9]

        //- - - - verifying - - - -
        //1.that the number of "drop" for the problematic link is incremented in both directions
        let new_weight_forward = *client.network_graph.edge_weight(client.network_graph.find_edge(problem_link_from_idx, problem_link_to_idx).unwrap()).unwrap();
        let new_weight_backward = *client.network_graph.edge_weight(client.network_graph.find_edge(problem_link_to_idx, problem_link_from_idx).unwrap()).unwrap();
        assert_eq!(new_weight_forward, initial_weight_forward.saturating_add(1), "weight of the forward link should be incremented");
        assert_eq!(new_weight_backward, initial_weight_backward.saturating_add(1), "weight of the backward link should be incremented");

        //2.thet the route for this session has been marked for the recalculation
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
        let (mut client, _client_incoming_packet_tx, mut neighbor_outbound_receivers) = setup_client(client_id, vec![drone1_id]);

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
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![drone1_id, drone2_id]);

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
        let (mut client, _client_incoming_packet_tx, _neighbor_outbound_receivers) = setup_client(client_id, vec![]);

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

        //- - - - preparing the environment for the test - - - -
        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }
        let (mut client, _client_incoming_packet_tx, mut neighbor_outbound_receivers) =
            setup_client(client_id, vec![drone1_id]);
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

        // - - - - testing Login command - - - -
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

        //- - - - testing Logout command - - - -
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

        //- - - - testing Logout command while not logged in - - - -
        assert_eq!(client.connected_server_id, None, "client should be disconnected for this part of the test");
        while drone1_receiver.try_recv().is_ok() {}
        let initial_session_counter_no_logout = *SESSION_COUNTER.lock().unwrap();
        client.process_gui_command(client_id, "[Logout]".to_string());
        assert_eq!(client.connected_server_id, None, "id of the connected server should remain None if client not logged in");
        assert!(drone1_receiver.try_recv().is_err(), "no packet should be sent when logging out while not connected");
        assert_eq!(*SESSION_COUNTER.lock().unwrap(), initial_session_counter_no_logout, "session counter shouldn't increment if the Logout command is ignored");
    }

    #[test]
    fn test_client_gui_commands_to_high_level_messages() {
        let client_id = 101;
        let drone_id = 1;
        let server_id = 200;
        let neighbor_ids = vec![drone_id];

        //- - - - preparing the environment for the test - - - -
        let (mut client, _client_incoming_packet_tx, mut neighbor_outbound_receivers) =
            setup_client(client_id, neighbor_ids.clone());
        {
            let mut counter = SESSION_COUNTER.lock().unwrap();
            *counter = 0;
        }
        let client_idx = client.network_graph.add_node(NodeInfo { id: client_id, node_type: PktNodeType::Client });
        client.node_id_to_index.insert(client_id, client_idx);
        let drone_idx = client.network_graph.add_node(NodeInfo { id: drone_id, node_type: PktNodeType::Drone });
        client.node_id_to_index.insert(drone_id, drone_idx);
        let server_idx = client.network_graph.add_node(NodeInfo { id: server_id, node_type: PktNodeType::Server });
        client.node_id_to_index.insert(server_id, server_idx);
        client.network_graph.add_edge(client_idx, drone_idx, 0);
        client.network_graph.add_edge(drone_idx, client_idx, 0);
        client.network_graph.add_edge(drone_idx, server_idx, 0);
        client.network_graph.add_edge(server_idx, drone_idx, 0);
        client.connected_server_id = Some(server_id);
        let drone_receiver = neighbor_outbound_receivers.remove(&drone_id)
            .expect("receiving channel for neighboring drone not found");
        const FRAGMENT_SIZE: usize = 128;

        //- - - - test 1: [MediaUpload] - - - -
        let media_name_upload = "test_image.png";
        let base64_data_upload = "SGVsbG8gV29ybGQgZnJvbSBJbWFnZSBVcGxvYWQh"; // "Hello World from Image Upload!" in Base64
        let command_upload = format!("[MediaUpload]::{media_name_upload}::{base64_data_upload}");
        let expected_high_level_upload = format!("[MediaUpload]::{media_name_upload}::{base64_data_upload}");
        let initial_session_counter_upload = *SESSION_COUNTER.lock().unwrap();

        client.process_gui_command(client_id, command_upload.clone());

        let expected_session_id_upload = initial_session_counter_upload + 1;
        let mut received_packets_upload = Vec::new();
        let expected_total_fragments_upload = (expected_high_level_upload.as_bytes().len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        for _ in 0..expected_total_fragments_upload {
            received_packets_upload.push(drone_receiver.recv_timeout(Duration::from_millis(100))
                .expect(&format!("expecting to receive a fragment for MediaUpload, fragment {}", received_packets_upload.len())));
        }
        let reassembled_upload = reassemble_fragments(received_packets_upload.clone());
        assert_eq!(reassembled_upload, expected_high_level_upload, "no reassembling for the MediaUpload message");
        assert_eq!(*SESSION_COUNTER.lock().unwrap(), expected_session_id_upload, "session counter not incremented correctly for MediaUpload");
        if let Some(pkt) = received_packets_upload.first() {
            assert_eq!(pkt.routing_header.hops, vec![client_id, drone_id, server_id], "wrong hops for MediaUpload");
            assert_eq!(pkt.routing_header.hop_index, 1, "wrong hop_index for MediaUpload");
        } else {
            panic!("no packet received for MediaUpload.");
        }

        //- - - - test 2: [MediaDownloadRequest] - - - -
        let media_name_download = "requested_media.jpg";
        let command_download = format!("[MediaDownloadRequest]::{media_name_download}");
        let expected_high_level_download = format!("[MediaDownloadRequest]::{media_name_download}");
        let initial_session_counter_download = *SESSION_COUNTER.lock().unwrap();

        client.process_gui_command(client_id, command_download.clone());

        let expected_session_id_download = initial_session_counter_download + 1;
        let mut received_packets_download = Vec::new();
        let expected_total_fragments_download = (expected_high_level_download.as_bytes().len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        for _ in 0..expected_total_fragments_download {
            received_packets_download.push(drone_receiver.recv_timeout(Duration::from_millis(100))
                .expect(&format!("expecting to receive a fragment for MediaDownloadRequest, fragment {}", received_packets_download.len())));
        }
        let reassembled_download = reassemble_fragments(received_packets_download.clone());
        assert_eq!(reassembled_download, expected_high_level_download, "missing message reassembling for MediaDownloadRequest");
        assert_eq!(*SESSION_COUNTER.lock().unwrap(), expected_session_id_download, "session counter not incremented correctly for MediaDownloadRequest");
        if let Some(pkt) = received_packets_download.first() {
            assert_eq!(pkt.routing_header.hops, vec![client_id, drone_id, server_id], "wrong hops for MediaDownloadRequest");
            assert_eq!(pkt.routing_header.hop_index, 1, "wrong hop_index for MediaDownloadRequest");
        } else {
            panic!("no packet received for MediaDownloadRequest.");
        }

        //- - - - test 3: [MediaListRequest] - - - -
        let command_list = "[MediaListRequest]".to_string();
        let expected_high_level_list = "[MediaListRequest]".to_string();
        let initial_session_counter_list = *SESSION_COUNTER.lock().unwrap();

        client.process_gui_command(client_id, command_list.clone());

        let expected_session_id_list = initial_session_counter_list + 1;
        let mut received_packets_list = Vec::new();
        let expected_total_fragments_list = (expected_high_level_list.as_bytes().len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        for _ in 0..expected_total_fragments_list {
            received_packets_list.push(drone_receiver.recv_timeout(Duration::from_millis(100))
                .expect(&format!("expecting to receive a fragment for MediaListRequest, fragment {}", received_packets_list.len())));
        }
        let reassembled_list = reassemble_fragments(received_packets_list.clone());
        assert_eq!(reassembled_list, expected_high_level_list, "missing reassembly of the message for MediaListRequest");
        assert_eq!(*SESSION_COUNTER.lock().unwrap(), expected_session_id_list, "session counter not incremented correctly for MediaListRequest");
        if let Some(pkt) = received_packets_list.first() {
            assert_eq!(pkt.routing_header.hops, vec![client_id, drone_id, server_id], "wrong hops for MediaListRequest");
            assert_eq!(pkt.routing_header.hop_index, 1, "wrong hop_index for MediaListRequest");
        } else {
            panic!("no packet received for MediaListRequest");
        }

        //- - - - test 4: [MediaBroadcast] - - - -
        let media_name_broadcast = "broadcast_video.mp4";
        let base64_data_broadcast = "SGVsbG8gQnJvYWRjYXN0IFdvcmxkIQ=="; // "Hello Broadcast World!" in Base64
        let command_broadcast = format!("[MediaBroadcast]::{media_name_broadcast}::{base64_data_broadcast}");
        let expected_high_level_broadcast = format!("[MediaBroadcast]::{media_name_broadcast}::{base64_data_broadcast}");
        let initial_session_counter_broadcast = *SESSION_COUNTER.lock().unwrap();

        client.process_gui_command(client_id, command_broadcast.clone());

        let expected_session_id_broadcast = initial_session_counter_broadcast + 1;
        let mut received_packets_broadcast = Vec::new();
        let expected_total_fragments_broadcast = (expected_high_level_broadcast.as_bytes().len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        for _ in 0..expected_total_fragments_broadcast {
            received_packets_broadcast.push(drone_receiver.recv_timeout(Duration::from_millis(100))
                .expect(&format!("expecting to receive a fragment for MediaBroadcast, fragment {}", received_packets_broadcast.len())));
        }
        let reassembled_broadcast = reassemble_fragments(received_packets_broadcast.clone());
        assert_eq!(reassembled_broadcast, expected_high_level_broadcast, "missing reassembly of the message for MediaBroadcast");
        assert_eq!(*SESSION_COUNTER.lock().unwrap(), expected_session_id_broadcast, "session counter not incremented correctly for MediaBroadcast");
        if let Some(pkt) = received_packets_broadcast.first() {
            assert_eq!(pkt.routing_header.hops, vec![client_id, drone_id, server_id], "wrong hops for MediaBroadcast");
            assert_eq!(pkt.routing_header.hop_index, 1, "wrong hop_index for MediaBroadcast");
        } else {
            panic!("no packet received for MediaBroadcast.");
        }

        //- - - - test 5: [HistoryRequest] - - - -
        let target_client_history = 102;
        let command_history = format!("[HistoryRequest]::{client_id}::{target_client_history}");
        let expected_high_level_history = format!("[HistoryRequest]::{client_id}::{target_client_history}");
        let initial_session_counter_history = *SESSION_COUNTER.lock().unwrap();

        client.process_gui_command(client_id, command_history.clone());

        let expected_session_id_history = initial_session_counter_history + 1;
        let mut received_packets_history = Vec::new();
        let expected_total_fragments_history = (expected_high_level_history.as_bytes().len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        for _ in 0..expected_total_fragments_history {
            received_packets_history.push(drone_receiver.recv_timeout(Duration::from_millis(100))
                .expect(&format!("expecting to receive a fragment for HistoryRequest, fragment {}", received_packets_history.len())));
        }
        let reassembled_history = reassemble_fragments(received_packets_history.clone());
        assert_eq!(reassembled_history, expected_high_level_history, "missing reassembling of the message for HistoryRequest");
        assert_eq!(*SESSION_COUNTER.lock().unwrap(), expected_session_id_history, "session counter not incremented correctly for HistoryRequest");
        if let Some(pkt) = received_packets_history.first() {
            assert_eq!(pkt.routing_header.hops, vec![client_id, drone_id, server_id], "wrong hops for HistoryRequest");
            assert_eq!(pkt.routing_header.hop_index, 1, "wrong hop_index for HistoryRequest");
        } else {
            panic!("no packet received for HistoryRequest");
        }

        assert!(client.packet_send.contains_key(&drone_id), "map packet_send of the client should still contain id of the drone");
    }
}