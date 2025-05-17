use std::collections::{HashMap, HashSet};
use std::time::Instant;
//use std::io::Write;
use std::process::Command;
use std::env::temp_dir;
//use std::path::PathBuf;
use rand::random;
use base64;
use image::load_from_memory;
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
//use wg_2024::packet::PacketType::MsgFragment;
//use wg_2024::drone::Drone;
use crossbeam_channel::{select_biased, Receiver, Sender};
use crate::simulation_controller::gui_input_queue::{pop_all_gui_messages, push_gui_message, SharedGuiInput};


#[derive(Debug, Clone)]
pub struct ReceivedMessageState {
    pub data : Vec<u8>,
    pub total_fragments : u64,
    pub received_indices : HashSet<u64>,
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
    pub sim_contr_recv: Receiver<DroneCommand>,
    pub sim_contr_send : Sender<DroneEvent>,
    pub sent_messages: HashMap<u64, SentMessageInfo>,
    pub received_messages : HashMap<u64, ReceivedMessageState>, //to determine when the message is complete (it has all the fragments)
    pub network_graph : StableGraph<NodeInfo, usize>, //graph to memorize info about nodes
    pub node_id_to_index : HashMap<NodeId, NodeIndex>, //mapping from node_id to inner indices of the graph
    pub active_flood_discoveries: HashMap<u64, FloodDiscoveryState>, //structure to take track of flood_request/response
    pub gui_input : SharedGuiInput,
    //pub finalized_flood_discoveries : bool,
}

#[derive(Debug, Clone)]
struct FloodDiscoveryState {
    initiator_id : NodeId,
    start_time : Instant,
    received_responses : Vec<FloodResponse>,
    //is_finalized : bool
}

impl MyClient {
    pub fn new(id: NodeId, sim_contr_recv: Receiver<DroneCommand>, sim_contr_send: Sender<DroneEvent>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, sent_messages: HashMap<u64, SentMessageInfo>, gui_input: SharedGuiInput) -> Self {
        Self {
            id,
            sim_contr_recv,
            sim_contr_send,
            packet_recv,
            packet_send,
            sent_messages,
            received_messages: HashMap::new(),
            network_graph: StableGraph::new(),
            node_id_to_index: HashMap::new(),
            active_flood_discoveries: HashMap::new(),
            gui_input,
            //finalized_flood_discoveries : HashSet::new(),
        }
    }

    fn run(&mut self) {
        let mut seen_flood_ids = HashSet::new();
        println!("Client {} starting run loop", self.id);
        if self.network_graph.node_count() == 0 {
            println!("Client {} network graph is empty, starting flood discovery", self.id);
            self.start_flood_discovery()
        }
        loop {
            let gui_messages = pop_all_gui_messages(&self.gui_input, self.id);
            for (server_id, msg_string) in gui_messages {
                self.process_gui_command(server_id, msg_string);
            }
            self.check_flood_discoveries_timeouts();
            select_biased! {
                recv(self.sim_contr_recv) -> command => {
                    if let Ok(command) = command {
                        println!("Command received by client {} : {:?}", self.id, command);
                        match command {
                            DroneCommand::AddSender(node_id, sender) => {
                                println!("Client {} received AddSender command for node {}", self.id, node_id);
                                self.packet_send.insert(node_id, sender);
                                println!("Client {} starting flood discovery after AddSender", self.id);
                                self.start_flood_discovery();
                            },
                            DroneCommand::RemoveSender(node_id) => {
                                println!("Client {} received RemoveSender command for node {}", self.id, node_id);
                                self.packet_send.remove(&node_id);
                                self.remove_node_from_graph(node_id);
                                println!("Client {} starting flood discovery after RemoveSender", self.id);
                                self.start_flood_discovery();
                            },
                            DroneCommand::Crash | DroneCommand::SetPacketDropRate(_) => {
                                eprintln!("Client {} received unexpected command: {:?}", self.id, command);
                            }
                        }
                    } else {
                        println!("Simulation Controller channel closed for client {}. Exiting run loop.", self.id);
                        break;
                    }
                },
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet by client {} ...", self.id);
                    if let Ok(packet) = packet {
                        println!("Packet received by drone {} : {:?}",self.id, packet);
                        self.process_packet(packet, &mut seen_flood_ids);
                    }
                },
                default => {
                    //it avoids too much CPU consuming
                    std::thread::sleep(std::time::Duration::from_millis(1));
                },
            }
        }
    }

    fn check_flood_discoveries_timeouts(&mut self) {
        let now = Instant::now();
        let timeout_duration = std::time::Duration::from_millis(5);
        let floods_to_finalize: Vec<u64> = self.active_flood_discoveries.iter().filter(|(_flood_id, state)| now.duration_since(state.start_time) > timeout_duration).map(|(_flood_id, state)| *_flood_id).collect();
        for flood_id in floods_to_finalize {
            println!("Client {} finalizing flood discovery for ID {} due to timeout", self.id, flood_id);
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
            eprintln!("Client {} tried to finalize unknown flood discovery ID {}", self.id, flood_id);
        }
    }

    fn process_packet(&mut self, mut packet: Packet, seen_flood_ids: &mut HashSet<u64>) {
        let packet_type = packet.pack_type.clone();
        match packet_type {
            PacketType::FloodRequest(request) => { //CONTROLLA SE IL CLIENT PUò PROPAGARE FLOODREQUESTS
                //println!("Client {} received FloodRequest {}, ignoring as client should not propagate.", self.id, request.flood_id);
                self.process_flood_request(&request, seen_flood_ids, packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                println!("Client {} received FloodResponse for flood_id {}", self.id, response.flood_id);
                if let Some(discovery_state) = self.active_flood_discoveries.get_mut(&response.flood_id) {
                    discovery_state.received_responses.push(response.clone());
                    println!("Client {} collected {} responses for flood ID {}.", self.id, discovery_state.received_responses.len(), response.flood_id);
                } else {
                    eprintln!("Client {} received FloodResponse for unknown or inactive flood ID {}. Processing path anyway.", self.id, response.flood_id);
                    //self.create_topology(&response);
                }
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut packet, &fragment);
                self.reassemble_packet(&fragment, &mut packet);
            },
            PacketType::Ack(ack) => {
                println!("Client {} received ACK for session {}, fragment {}", self.id, packet.session_id, ack.fragment_index);
                if let Some(sent_msg_info) = self.sent_messages.get_mut(&packet.session_id) {
                    sent_msg_info.received_ack_indices.insert(ack.fragment_index);
                    println!("Client {} marked fragment {} of session {} as ACKed. Received {}/{} ACKs", self.id, ack.fragment_index, packet.session_id, sent_msg_info.received_ack_indices.len(), sent_msg_info.fragments.len());
                    /*
                    if sent_msg_info.received_ack_indices-len() == sent_msg_info.fragments.len() {
                        println!("Client {} received all ACKs for session {}. Message considered successfully sent.", self.id, packet.session_id);
                        //self.sent_messages.remove(&packet.session_id);
                    }
                     */
                } else {
                    eprintln!("Client {} received ACK for unknown session {}", self.id, packet.session_id);
                }
            },
            PacketType::Nack(nack) => {
                self.process_nack(&nack, &mut packet);
            }
        }
    }

    fn process_flood_request(&mut self, request: &FloodRequest, seen_flood_ids: &mut HashSet<u64>, header: SourceRoutingHeader) {
        let mut updated_request = request.clone();
        updated_request.path_trace.push((self.id, NodeType::Client));
        println!("Client {} processed FloodRequest {}. Path trace now: {:?}", self.id, request.flood_id, updated_request.path_trace);
        let flood_response = FloodResponse {
            flood_id: request.flood_id,
            path_trace: updated_request.path_trace,
        };
        let mut response_routing_header = header.clone();
        response_routing_header.hops.reverse();
        response_routing_header.hop_index = 1;
        let reversed_hops = response_routing_header.hops.clone();
        let final_destination_id = flood_response.path_trace.first().map(|(id, _)| *id);
        if response_routing_header.hops.len() > 1 {
            let next_hop = response_routing_header.hops[response_routing_header.hop_index];
            let response_packet_to_send = Packet {
                pack_type: PacketType::FloodResponse(flood_response.clone()),
                routing_header: response_routing_header.clone(),
                session_id: request.flood_id,
            };
            if let Some(sender) = self.packet_send.get(&next_hop) {
                match sender.send(response_packet_to_send.clone()) {
                    Ok(()) => {
                        println!("Client {} sent FloodResponse {} back to {}", self.id, request.flood_id, next_hop);
                    }
                    Err(e) => {
                        eprintln!("Client {} failed to send FloodResponse {} back to {}: {}", self.id, request.flood_id, next_hop, e);
                        if let Some(dest_id) = final_destination_id {
                            let shortcut_header = SourceRoutingHeader {
                                hop_index: 0,
                                hops: vec![dest_id],
                            };
                            let shortcut_packet = Packet {
                                pack_type: PacketType::FloodResponse(flood_response.clone()),
                                routing_header: shortcut_header,
                                session_id: request.flood_id,
                            };
                            match self.sim_contr_send.send(DroneEvent::ControllerShortcut(shortcut_packet)) {
                                Ok(()) => println!("Client {} sent FloodResponse {} via ControllerShortcut to destination {}.", self.id, request.flood_id, dest_id),
                                Err(e_sc) => eprintln!("Client {} FAILED to send FloodResponse {} via ControllerShortcut to {}: {}", self.id, request.flood_id, dest_id, e_sc),
                            }
                        } else {
                            eprintln!("Client {} failed to send FloodResponse {} via ControllerShortcut: could not determine final destination.", self.id, request.flood_id);
                        }
                    }
                }
            } else {
                eprintln!("Client {} error: no sender found for FloodResponse next hop {}. Attempting ControllerShortcut", self.id, next_hop);
                if let Some(dest_id) = final_destination_id {
                    let shortcut_header = SourceRoutingHeader {
                        hop_index: 0,
                        hops: vec![dest_id],
                    };
                    let shortcut_packet = Packet {
                        pack_type: PacketType::FloodResponse(flood_response.clone()),
                        routing_header: shortcut_header,
                        session_id: request.flood_id,
                    };
                    match self.sim_contr_send.send(DroneEvent::ControllerShortcut(shortcut_packet)) {
                        Ok(()) => println!("Client {} sent FloodResponse {} via ControllerShortcut (neighbor not found).", self.id, request.flood_id),
                        Err(e_sc) => eprintln!("Client {} FAILED to send FloodResponse via ControllerShortcut (neighbor not found) for flood_id {}: {}", self.id, request.flood_id, e_sc),
                    }
                } else {
                    eprintln!("Client {} error: cannot send FloodResponse, inverted routing header hops too short {:?}. Cannot use normal path.", self.id, reversed_hops);
                    if let Some(dest_id) = final_destination_id {
                        eprintln!("Client {} attempting ControllerShortcut due to short reversed hops.", self.id);
                        let shortcut_header = SourceRoutingHeader {
                            hop_index: 0,
                            hops: vec![dest_id],
                        };

                        let shortcut_packet = Packet {
                            pack_type: PacketType::FloodResponse(flood_response.clone()),
                            routing_header: shortcut_header,
                            session_id: request.flood_id,
                        };

                        match self.sim_contr_send.send(DroneEvent::ControllerShortcut(shortcut_packet)) { 
                            Ok(()) => println!("Client {} sent FloodResponse {} via ControllerShortcut (short hops).", self.id, request.flood_id),
                            Err(e_sc) => eprintln!("Client {} FAILED to send FloodResponse via ControllerShortcut (short hops) for flood_id {}: {}", self.id, request.flood_id, e_sc),
                        }
                    } else {
                        eprintln!("Client {} failed to send FloodResponse {} via ControllerShortcut: could not determine final destination (short hops case).", self.id, request.flood_id);
                    }
                }
            }
        }
        seen_flood_ids.insert(request.flood_id);
    }

    fn start_flood_discovery(&mut self) {
        println!("Client {} starting flood discovery", self.id);
        //1.generating unique flood_id 
        let new_flood_id = random::<u64>();
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

        println!("Client {} sending FloodRequest {} to all neighbors", self.id, new_flood_id);
        //4.sending flood_request to all neighbors
        for (&neighbor_id, sender) in &self.packet_send {
            match sender.send(flood_packet.clone()) {
                Ok(_) => println!("Client {} sent FloodRequest to neighbor {}", self.id, neighbor_id),
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
            eprintln!("Client {} error: Received packet with empty hops in routing header for session {}", self.id, session_id);
            0
        });
        let key = session_id;
        let fragment_len = fragment.length as usize;
        let offset = (fragment.fragment_index * 128) as usize;
        let state = self.received_messages.entry(key).or_insert_with(|| {
            println!("Client {} starting reassembly for session {} from src {}", self.id, session_id, src_id);
            ReceivedMessageState {
                data: vec![0u8; (fragment.total_n_fragments * 128) as usize],
                total_fragments: fragment.total_n_fragments,
                received_indices: HashSet::new(),
            }
        });
        if state.received_indices.contains(&fragment.fragment_index) {
            println!("Received duplicate fragment {} for session {}. Ignoring", fragment.fragment_index, session_id);
            return;
        }

        if offset + fragment_len <= state.data.len() {
            state.data[offset..offset + fragment_len].copy_from_slice(&fragment.data[..fragment_len]);

            state.received_indices.insert(fragment.fragment_index);
            println!("Client {} received fragment {} for session {}. Received {}/{} fragments.", self.id, fragment.fragment_index, session_id, state.received_indices.len(), state.total_fragments);
            if state.received_indices.len() as u64 == state.total_fragments {
                println!("Message for session {} reassembled successfully", session_id);
                let full_message_data = state.data.clone();
                self.received_messages.remove(&key);

                let message_string = String::from_utf8_lossy(&full_message_data).to_string();
                self.process_received_high_level_message(message_string, src_id, session_id);
            }
        } else {
            eprintln!("Received fragment {} for session {} with offset {} and length {} which exceeds excepted message size {}. Ignoring fragment", fragment.fragment_index, session_id, offset, fragment_len, state.data.len());
        }
    }

    //function in which the correctly reassembled high level messages are elaborated
    fn process_received_high_level_message(&mut self, message_string: String, source_id: NodeId, session_id: u64) {
        println!("Client {} processing high-level message for session {} from source {}: {}", self.id, session_id, source_id, message_string);
        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();
        if tokens.is_empty() {
            eprintln!("Client {} received empty high-level message for session {}", self.id, session_id);
            return;
        }

        match tokens.as_slice() {
            ["[MessageFrom]", sender_id_str, content_str] => {
                // format: [MessageFrom]::sender_id::message_content 
                if let Ok(sender_id) = sender_id_str.parse::<NodeId>() {
                    let content = *content_str;
                    println!("Client {} RECEIVED CHAT MESSAGE from client {}: {}", self.id, sender_id, content);
                } else {
                    eprintln!("Client {} received MessageFrom with invalid sender_id: {}", self.id, sender_id_str);
                }
            },
            ["[ClientListResponse]", list_str] => {
                //format: [ClientListResponse]::[client1_id, client2_id, ...]
                let client_ids: Vec<NodeId> = list_str.trim_start_matches('[').trim_end_matches(']').split(',').filter(|s| !s.trim().is_empty()).filter_map(|s| s.trim().parse::<NodeId>().ok()).collect();
                println!("Client {} RECEIVED CLIENT LIST: {:?}", self.id, client_ids);
            },
            ["[ChatStart]", status_str] => {
                //format: [ChatStart]::bool
                match status_str.to_lowercase().as_str() {
                    "true" => {
                        println!("Client {} CHAT REQUEST ACCEPTED. Chat started.", self.id);
                    },
                    "false" => {
                        println!("Client {} CHAT REQUEST DENIED.", self.id);
                    },
                    _ => eprintln!("Client {} received ChatStart with invalid boolean value: {}", self.id, status_str),
                }
            },
            ["[ChatFinish]"] => {
                //format: [ChatFinish]
                println!("Client {} CHAT TERMINATED.", self.id);
            },
            ["[ChatRequest]", peer_id_str] => {
                //format: [ChatRequest]::{peer_id}
                if let Ok(requester_id) = peer_id_str.parse::<NodeId>() {
                    println!("Client {} RECEIVED INCOMING CHAT REQUEST from client {}.", self.id, requester_id);
                } else {
                    eprintln!("Client {} received ChatRequest with invalid requester_id: {}", self.id, peer_id_str);
                }
            },
            ["[HistoryResponse]", content_str] => {
                //format: [HistoryResponse]::message_content
                let history_content = *content_str;
                println!("Client {} RECEIVED CHAT HISTORY: {}", self.id, history_content);
            },
            ["[MediaUploadAck]", media] => {
                //format: [MediaUploadAck]::media_name
                if tokens.len() >= 2 {
                    let media_name = media;
                    println!("Client {} received MediaUploadAck for media '{}'", self.id, media_name);
                    push_gui_message(&self.gui_input, source_id, self.id, format!("Upload media {} confirmed", media_name));
                } else {
                    eprintln!("Client {} received invalid MediaUploadAck format: {}", self.id, message_string);
                }
            },
            ["[MediaDownloadResponse]", media, base64] => {
                if tokens.len() >= 3 {
                    let media_name = media;
                    let base64_data = base64;
                    if media_name.to_string() == "ERROR" && base64_data.to_string() == "NotFound" {
                        println!("Client {} received MediaDownloadResponse: Media not found.", self.id);
                        push_gui_message(&self.gui_input, source_id, self.id, "Media requested not found".to_string());
                    } else {
                        println!("Client {} received MediaDownloadResponse for media '{}', data: ...", self.id, media_name);
                        //- - - - logic for showing the image - - - -
                        match base64::decode(&base64_data) {
                            Ok(image_bytes) => {
                                println!("Client {} successfully decoded base64 data for media '{}'", self.id, media_name);
                                //load_from_memory detects common image formats
                                match load_from_memory(&image_bytes) {
                                    Ok(img) => {
                                        println!("Client {} successfully loaded image data for media '{}'", self.id, media_name);
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
                                        let cleaned_media_name = media_name.replace(|c : char| !c.is_ascii_alphanumeric() && c != '_' && c != '-', "_");
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
                                                    Ok(_) => println!("Client {} successfully opened media '{}' with command '{} {}'", self.id, media_name, open_command, path_str),
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
            [type_tag, ..] => {
                eprintln!("Client {} received unrecognized high-level message type: {}", self.id, type_tag);
            },
            [] => {
                eprintln!("Client {} received empty high-level message for session {}", self.id, session_id);
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
                    Ok(()) => println!("Client {} sent ACK for session {} fragment {} to neighbor {}", self.id, packet.session_id, fragment.fragment_index, next_hop_for_ack),
                    Err(e) => {
                        eprintln!("Client {} error sending ACK packet for session {} to neighbor {}: {}. Using ControllerShortcut.", self.id, packet.session_id, next_hop_for_ack, e);
                        let mut shortcut_header = ack_packet_to_send.routing_header.clone();
                        shortcut_header.hop_index = 0;
                        shortcut_header.hops = vec![destination_node_id.unwrap()];
                        let shortcut_packet = Packet {
                            pack_type: ack_packet_to_send.pack_type,
                            routing_header: shortcut_header,
                            session_id: ack_packet_to_send.session_id,
                        };
                        match self.sim_contr_send.send(DroneEvent::ControllerShortcut(shortcut_packet)) {
                            Ok(()) => println!("Client {} sent ACK for session {} fragment {} via ControllerShortcut.", self.id, packet.session_id, fragment.fragment_index),
                            Err(e_sc) => eprintln!("Client {} FAILED to send ACK via ControllerShortcut for session {}: {}", self.id, packet.session_id, e_sc),
                        }
                    }
                }
            } else {
                eprintln!("Client {} error: no sender found for ACK next hop {}. Attempting to use ControllerShortcut.", self.id, next_hop_for_ack);
                let mut shortcut_header = ack_packet_to_send.routing_header.clone();
                shortcut_header.hop_index = 0;
                shortcut_header.hops = vec![destination_node_id.unwrap()];
                let shortcut_packet = Packet {
                    pack_type: ack_packet_to_send.pack_type,
                    routing_header: shortcut_header,
                    session_id: ack_packet_to_send.session_id,
                };
                match self.sim_contr_send.send(DroneEvent::ControllerShortcut(shortcut_packet)) {
                    Ok(()) => println!("Client {} sent ACK for session {} fragment {} via ControllerShortcut (neighbor not found).", self.id, packet.session_id, fragment.fragment_index),
                    Err(e_sc) => eprintln!("Client {} FAILED to send ACK via ControllerShortcut (neighbor not found) for session {}: {}", self.id, packet.session_id, e_sc),
                }
            }
        } else {
            eprintln!("Client {} error: cannot send ACK, original routing header hops too short: {:?}. Cannot use ControllerShortcut.", self.id, packet.routing_header.hops);
        }
    }

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        println!("Client {} processing NACK type {:?} for session {}", self.id, nack.nack_type, packet.session_id);
        match &nack.nack_type {
            NackType::ErrorInRouting(problem_node_id) => {
                eprintln!("Client {} received Nack::ErrorInRouting for session {} at node {}", self.id, packet.session_id, problem_node_id);
                self.remove_node_from_graph(*problem_node_id);
                println!("Client {} removed node {} from graph due to ErrorInRouting", self.id, problem_node_id);
                let sent_msg_info = self.sent_messages.get_mut(&packet.session_id);
                if let Some(info) = sent_msg_info {
                    info.route_needs_recalculation = true;
                    println!("Client {} marked route for session {} for recalculation due to ErrorInRouting.", self.id, packet.session_id);
                } else {
                    eprintln!("Client {} received ErrorInRouting NACK for unknown session {}", self.id, packet.session_id);
                }
            }
            NackType::DestinationIsDrone => {
                eprintln!("Client {} received Nack::DestinationIsDrone for session {}. Route calculation error?", self.id, packet.session_id);
                let sent_msg_info = self.sent_messages.get_mut(&packet.session_id);
                if let Some(info) = sent_msg_info {
                    info.route_needs_recalculation = true;
                    println!("Client {} marked route for session {} for recalculation due to DestinationIsDrone.", self.id, packet.session_id);
                } else {
                    eprintln!("Client {} received DestinationIsDrone NACK for unknown session {}", self.id, packet.session_id);
                }
                //self.start_flood_discovery();
            }
            NackType::UnexpectedRecipient(received_at_node_id) => {
                eprintln!("Client {} received Nack::UnexpectedRecipient for session {}: packet arrived at node {} but expected a different one. Route mismatch or topology error?", self.id, packet.session_id, received_at_node_id);
                let nack_route = &packet.routing_header.hops;
                let nack_hop_index = packet.routing_header.hop_index;
                if nack_hop_index > 0 && nack_hop_index < nack_route.len() {
                    let problem_link_from = nack_route[nack_hop_index - 1];
                    let problem_link_to = nack_route[nack_hop_index];
                    println!("Client {} penalizing link {} -> {} due to UnexpectedRecipient", self.id, problem_link_from, problem_link_to);
                    self.increment_drop(problem_link_from, problem_link_to);
                    self.increment_drop(problem_link_to, problem_link_from);
                } else {
                    eprintln!("Client {} received UnexpectedRecipient NACK with invalid routing header/hop index {:?}", self.id, packet.routing_header);
                }
                let sent_msg_info = self.sent_messages.get_mut(&packet.session_id);
                if let Some(info) = sent_msg_info {
                    info.route_needs_recalculation = true;
                    println!("Client {} marked route for session {} for recalculation due to UnexpectedRecipient.", self.id, packet.session_id);
                } else {
                    eprintln!("Client {} received UnexpectedRecipient NACK for unknown session {}", self.id, packet.session_id);
                }
            }
            NackType::Dropped => {
                eprintln!("Client {} received Nack::Dropped for session {}, fragment {}", self.id, packet.session_id, nack.fragment_index);
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
                        println!("Client {} penalizing link {} -> {} due to Dropped NACK", self.id, from_node, dropped_at_node);
                        self.increment_drop(from_node, dropped_at_node);
                        self.increment_drop(dropped_at_node, from_node);
                    } else {
                        eprintln!("Client {} cannot determine dropping link: Original routing header hop_index is out of bounds for session {}", self.id, packet.session_id);
                    }
                    let packet_to_resend = Packet {
                        pack_type: PacketType::MsgFragment(fragment_to_resend.clone()),
                        routing_header: original_header,
                        session_id: packet.session_id,
                    };
                    if packet_to_resend.routing_header.hops.len() > packet_to_resend.routing_header.hop_index {
                        let first_hop = packet_to_resend.routing_header.hops[packet_to_resend.routing_header.hop_index];
                        match self.send_to_neighbor(first_hop, packet_to_resend) {
                            Ok(_) => println!("Client {} resent fragment {} for session {} to {}", self.id, fragment_to_resend.fragment_index, packet.session_id, first_hop),
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
        println!("Client {} processing FloodResponse for flood_id {}", self.id, flood_response.flood_id);
        let path = &flood_response.path_trace;
        if path.is_empty() {
            println!("Received empty path_trace in FloodResponse for flood_id {}", flood_response.flood_id);
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
                if let Some(&current_node_idx) = self.node_id_to_index.get(&current_node_id) {
                    if let Some(&prev_node_idx) = self.node_id_to_index.get(&prev_node_id) {
                        let edge_exists = self.network_graph.contains_edge(current_node_idx, prev_node_idx) || self.network_graph.contains_edge(prev_node_idx, current_node_idx);
                        if !edge_exists {
                            println!("Client {} adding edge: {} -> {}", self.id, prev_node_id, current_node_id);
                            self.network_graph.add_edge(prev_node_idx, current_node_idx, 0);
                            self.network_graph.add_edge(current_node_idx, prev_node_idx, 0);
                        } else {
                            println!("Client {} edge {} -> {} already exists.", self.id, prev_node_id, current_node_id);
                        }
                    } else {
                        eprintln!("Client {} error: previous node id {} not found in node_id_to_index map while processing path trace", self.id, prev_node_id);
                    }
                } else {
                    eprintln!("Client {} error: current node id {} not found in node_id_to_index map while processing path trace", self.id, current_node_id);
                }
            }
        }
    }

    fn process_gui_command(&mut self, destination_server_id: NodeId, command_string: String) {
        println!("Client {} processing GUI command '{}' for server {}", self.id, command_string, destination_server_id);
        let tokens: Vec<&str> = command_string.trim().split("::").collect();
        let command_type = tokens.first().unwrap_or(&"");
        let high_level_message_info: Option<(String, NodeId)> = match *command_type {
            "[Login]" => {
                println!("Client {} processing Login command", self.id);
                Some(("[Login]".to_string(), destination_server_id))
            },
            "[Logout]" => {
                println!("Client {} processing Logout command", self.id);
                Some(("[Logout]".to_string(), destination_server_id))
            },
            "[ClientListRequest]" => {
                println!("Client {} processing ClientListRequest command", self.id);
                Some(("[ClientListRequest]".to_string(), destination_server_id))
            },
            "[MessageTo]" => {
                if tokens.len() >= 3 {
                    let target_id_str = tokens[3];
                    let message_content = tokens[4];
                    if let Ok(target_client_id) = target_id_str.parse::<NodeId>() {
                        println!("Client {} processing MessageTo command for client {} with content {}", self.id, target_client_id, message_content);
                        Some((format!("[MessageTo]::{target_client_id}::{message_content}"), destination_server_id))
                    } else {
                        eprintln!("Client {} received MessageTo command with invalid target_id: {}", self.id, target_id_str);
                        None
                    }
                } else {
                    eprintln!("Client {} received invalid MessageTo command format: {}", self.id, command_string);
                    None
                }
            },
            "[ChatRequest]" => {
                if tokens.len() >= 2 {
                    let peer_id_str = tokens[3];
                    if let Ok(peer_id) = peer_id_str.parse::<NodeId>() {
                        println!("Client {} processing ChatRequest command for peer {}", self.id, peer_id);
                        Some((format!("[ChatRequest]::{peer_id}"), destination_server_id))
                    } else {
                        eprintln!("Client {} received ChatRequest command with invalid peer_id: {}", self.id, peer_id_str);
                        None
                    }
                } else {
                    eprintln!("Client {} received invalid ChatRequest command format: {}", self.id, command_string);
                    None
                }
            },
            "[ChatFinish]" => {
                if tokens.len() >= 2 {
                    let peer_id_str = tokens[3];
                    if let Ok(peer_id) = peer_id_str.parse::<NodeId>() {
                        println!("Client {} processing ChatFinished command for peer {}", self.id, peer_id);
                        Some((format!("[ChatFinish]::{peer_id}"), destination_server_id))
                    } else {
                        eprintln!("Client {} received ChatFinish command with invalid peer_id: {}", self.id, peer_id_str);
                        None
                    }
                } else {
                    eprintln!("Client {} received invalid ChatFinish command format: {}", self.id, command_string);
                    None
                }
            },
            "[MediaUpload]" => {
                if tokens.len() >= 3 {
                    let media_name = tokens[1];
                    let base64_data = tokens[42];
                    println!("Client {} processing MediaUpload command for media '{}'", self.id, media_name);
                    Some((format!("[MediaUpload]::{media_name}::{base64_data}"), destination_server_id))
                } else {
                    eprintln!("Client {} received invalid MediaUpload command format: {}", self.id, command_string);
                    None
                }
            },
            "[MediaDownloadRequest]" => {
                if tokens.len() >= 2 {
                    let media_name = tokens[1];
                    println!("Client {} processing MediaDownloadRequest command for media '{}'", self.id, media_name);
                    Some((format!("[MediaDownloadRequest]::{media_name}"), destination_server_id))
                } else {
                    eprintln!("Client {} received invalid MediaDownloadRequest command format: {}", self.id, command_string);
                    None
                }
            },
            _ => {
                eprintln!("Client {} received unrecognized GUI command: {}", self.id, command_string);
                None
            }
        };
        //- - - - route evaluation towards destination server - - - -
        if let Some((high_level_message_content, target_destination_id)) = high_level_message_info {
            let route_option = self.best_path(self.id, destination_server_id);
            let route = match route_option {
                Some(path) => {
                    println!("Client {} computed route to server {}: {:?}", self.id, destination_server_id, path);
                    path
                },
                None => {
                    eprintln!("Client {} could not compute route to server {}. Starting flood discovery.", self.id, destination_server_id);
                    self.start_flood_discovery();
                    return;
                }
            };
            let routing_header = SourceRoutingHeader {
                hops: route.clone(),
                hop_index: 1,
            };
            let message_data_bytes = high_level_message_content.into_bytes();
            const FRAGMENT_SIZE: usize = 128;
            let total_fragments = (message_data_bytes.len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
            let mut fragments = Vec::new();
            let session_id = random::<u64>();
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
            println!("Client {} stored message info for session {}", self.id, session_id);
            if routing_header.hops.len() > routing_header.hop_index {
                let first_hop = routing_header.hops[routing_header.hop_index];
                println!("Client {} sending message fragments for session {} starting with hop {}", self.id, session_id, first_hop);
                for fragment in fragments {
                    let packet = Packet {
                        pack_type: PacketType::MsgFragment(fragment.clone()),
                        routing_header: routing_header.clone(),
                        session_id,
                    };
                    match self.send_to_neighbor(first_hop, packet) {
                        Ok(()) => println!("Client {} sent fragment {} for session {} to  {}", self.id, fragment.fragment_index, session_id, first_hop),
                        Err(e) => eprintln!("Client {} Failed to send fragment {} for session {} to {}: {}", self.id, fragment.fragment_index, session_id, first_hop, e),
                    }
                }
            } else {
                eprintln!("Client {} has no valid first hop in computed route {:?} to send the message to!", self.id, route);
                self.start_flood_discovery();
            }
        }
    }

    fn best_path(&self, from: NodeId, to: NodeId) -> Option<Vec<NodeId>> {
        println!("Client {} calculating path from {} to {}", self.id, from, to);
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
                        println!("Client {} found path from {} to {}: {:?}", self.id, from, to, node_ids_path);
                        if node_ids_path.is_empty() || node_ids_path.first().copied() != Some(from) || node_ids_path.last().copied() != Some(to) {
                            eprintln!("Client {} computed an invalid path structure from {} to {}: {:?}", self.id, from, to, node_ids_path);
                            None
                        } else {
                            Some(node_ids_path)
                        }
                    },
                    None => {
                        println!("Client {} could not find a path from {} to {}", self.id, from, to);
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
        println!("Client {} incrementing drop count for link {} -> {}", self.id, from, to);
        let from_node_idx = self.node_id_to_index.get(&from).copied();
        let to_node_idx = self.node_id_to_index.get(&to).copied();
        if let (Some(from_idx), Some(to_idx)) = (from_node_idx, to_node_idx) {
            if let Some(edge_index) = self.network_graph.find_edge(from_idx, to_idx) {
                let current_weight = *self.network_graph.edge_weight(edge_index).unwrap();
                *self.network_graph.edge_weight_mut(edge_index).unwrap() = current_weight.saturating_add(1);
                println!("Client {} incremented weight for link {} -> {} to {}", self.id, from, to, current_weight.saturating_add(1));
            } else {
                println!("Client {} cannot increment drop: link {} -> {} not found in graph", self.id, from, to);
            }
        } else {
            println!("Client {} cannot increment drop: one or both nodes ({}, {}) not found in node_id_to_index map", self.id, from, to);
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

    fn send_chat_message(&mut self, destination_server_id: NodeId, target_client_id: NodeId, message_text: String) {
        println!("Client {} preparing to send chat message to client {} via server {}", self.id, target_client_id, destination_server_id);
        //- - - - route computation - - - -
        let route_option = self.best_path(self.id, destination_server_id);
        let route = match route_option {
            Some(path) => {
                println!("Client {} computed route: {:?}", self.id, path);
                path
            },
            None => {
                eprintln!("Client {} could not compute route to server {}", self.id, destination_server_id);
                return;
            }
        };

        let routing_header = SourceRoutingHeader {
            hops: route.clone(),
            hop_index: 1,
        };

        //- - - - creation of high level message - - - -
        let high_level_message_content = format!("[message_for]::{client_id}::{message}", client_id = target_client_id, message = message_text); //DA SISTEMARE, FATTO DA IA
        let message_data_bytes = high_level_message_content.into_bytes();

        //- - - - message fragmentation - - - -
        const FRAGMENT_SIZE: usize = 128;
        let total_fragments = (message_data_bytes.len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        let mut fragments: Vec<Fragment> = Vec::new();
        let session_id = random::<u64>();
        for i in 0..total_fragments {
            let start = i * FRAGMENT_SIZE;
            let end = std::cmp::min(start + FRAGMENT_SIZE, message_data_bytes.len());
            let len = end - start;
            let mut data_array = [0u8; 128];
            data_array[..len].copy_from_slice(&message_data_bytes[start..end]);

            fragments.push(Fragment {
                fragment_index: i as u64,
                total_n_fragments: total_fragments as u64,
                length: len as u8,
                data: data_array,
            });
        }
        println!("Client {} fragmented message into {} fragments for session {}", self.id, total_fragments, session_id);

        self.sent_messages.insert(session_id, SentMessageInfo {
            fragments: fragments.clone(),
            original_routing_header: routing_header.clone(),
            received_ack_indices: HashSet::new(),
            route_needs_recalculation: false,
        });

        //- - - - sending fragmented message - - - -
        if routing_header.hops.len() > routing_header.hop_index {
            let first_hop = routing_header.hops[routing_header.hop_index];
            for fragment in fragments {
                let packet = Packet {
                    pack_type: PacketType::MsgFragment(fragment.clone()),
                    routing_header: routing_header.clone(),
                    session_id,
                };
                match self.send_to_neighbor(first_hop, packet) {
                    Ok(()) => println!("Client {} sent fragment {} for session {} to {}", self.id, fragment.fragment_index, session_id, first_hop),
                    Err(e) => eprintln!("Client {} Failed to send fragment {} for session {} to {}: {}", self.id, fragment.fragment_index, session_id, first_hop, e),
                }
            }
        } else {
            eprintln!("Client {} has no valid first hop in computed route {:?} to send the message to!", self.id, route);
        }
    }

    fn get_node_info(&self, node_id: NodeId) -> Option<&NodeInfo> {
        self.node_id_to_index.get(&node_id).and_then(|&idx| self.network_graph.node_weight(idx))
    }

    fn send_to_neighbor(&mut self, neighbor_id: NodeId, packet: Packet) -> Result<(), String> {
        if let Some(sender) = self.packet_send.get(&neighbor_id) {
            match sender.send(packet) {
                Ok(()) => Ok(()),
                Err(e) => Err(format!("Failed to send packet to neighbor {}: {}", neighbor_id, e)),
            }
        } else {
            Err(format!("Neighbor {} not found in packet_send map.", neighbor_id))
        }
    }
}