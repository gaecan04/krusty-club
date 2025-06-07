    use std::collections::{HashMap, HashSet};
    use std::time::Instant;
    //use std::io::Write;
    use std::process::Command;
    use std::env::temp_dir;
    //use std::path::PathBuf;
    use rand::random;
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
    use once_cell::sync::Lazy;
    use std::sync::Mutex;
    use std::collections::VecDeque;


    static SESSION_IDS : Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(0));

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
        pub sent_messages: HashMap<u64, SentMessageInfo>,
        pub received_messages : HashMap<u64, ReceivedMessageState>, //to determine when the message is complete (it has all the fragments)
        pub network_graph : StableGraph<NodeInfo, usize>, //graph to memorize info about nodes
        pub node_id_to_index : HashMap<NodeId, NodeIndex>, //mapping from node_id to inner indices of the graph
        pub active_flood_discoveries: HashMap<u64, FloodDiscoveryState>, //structure to take track of flood_request/response
        pub connected_server_id : Option<NodeId>,
        pub seen_flood_ids : HashSet<(u64, NodeId)>,
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
            }
        }

        fn get_next_session_id(&self) -> u64 {
            let mut counter = SESSION_IDS.lock().unwrap();
            let current_id = *counter;
            *counter += 1;
            current_id
        }

        pub(crate) fn run(&mut self, gui_input: SharedGuiInput) {
            let mut seen_flood_ids = HashSet::new();

            println!("Client {} starting run loop", self.id);

            if self.network_graph.node_count() == 0 {
                println!("Client {} network graph is empty, starting flood discovery", self.id);
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
                            self.process_packet(packet, &mut seen_flood_ids);
                        } else {
                            println!("No packet received or channel closed.");
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

        ///SISTEMA FUNZIONE (vedi anche active_flood_discoveries)
        fn process_packet(&mut self, mut packet: Packet, seen_flood_ids: &mut HashSet<u64>) {
            let packet_type = packet.pack_type.clone();
            match packet_type {
                PacketType::FloodRequest(request) => { //CONTROLLA SE IL CLIENT PUò PROPAGARE FLOODREQUESTS
                    //println!("Client {} received FloodRequest {}, ignoring as client should not propagate.", self.id, request.flood_id);
                    self.process_flood_request(&request, packet.routing_header.clone());
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
                        if sent_msg_info.received_ack_indices.len() == sent_msg_info.fragments.len() {
                            println!("Client {} received all ACKs for session {}. Message considered successfully sent.", self.id, packet.session_id);
                            //self.sent_messages.remove(&packet.session_id);
                        }
                    } else {
                        eprintln!("Client {} received ACK for unknown session {}", self.id, packet.session_id);
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
                                session_id : self.get_next_session_id(),
                            };
                            match sender_channel.send(response_packet_to_send) {
                                Ok(()) => println!("Client {} sent FloodResponse {} back to {} (prev hop)", self.id, request.flood_id, prev_hop_id),
                                Err(e) => eprintln!("Client {} failed to send FloodResponse {} back to {}: {}", self.id, request.flood_id, prev_hop_id, e),
                            }
                        } else {
                            eprintln!("Client {} error: no sender found for previous hop {}", self.id, prev_hop_id);
                        }
                    } else {
                        eprintln!("Client {} cannot send FloodResponse back, path trace too short after adding self", self.id);
                    }
                } else {
                    eprintln!("Client {} error: could not find sender channel for previous hop", self.id);
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
                            session_id : self.get_next_session_id(),
                        };
                        match sender_channel.send(packet_to_forward) {
                            Ok(_) => println!("Client {} forwarded FloodRequest {} to neighbor {}", self.id, request.flood_id, neighbor_id),
                            Err(e) => eprintln!("Client {} failed to forward FloodRequest {} to neighbor {}: {}", self.id, request.flood_id, neighbor_id, e),
                        }
                    }
                }
            }
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
                session_id: self.get_next_session_id(),
            };

            println!("Client {} sending FloodRequest {} to all neighbors", self.id, new_flood_id);
            //4.sending flood_request to all neighbors
            for (&neighbor_id, sender) in &self.packet_send {
                match sender.send(flood_packet.clone()) {
                    Ok(_) =>  println!("Client {} sent FloodRequest to neighbor {}", self.id, neighbor_id),
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
                    } else {
                        eprintln!("Client {} received invalid MediaUploadAck format: {}", self.id, message_string);
                    }
                },
                ["[MediaDownloadResponse]", media, base64] => {
                    if tokens.len() >= 3 {
                        let media_name = media;
                        let base64_data = base64;
                        if media_name.to_string() == "ERROR" && base64_data.to_string() == "NotFound" {
                            println!("Client {} received MediaDownloadResponse: Media not found.", self.id)                    } else {
                            println!("Client {} received MediaDownloadResponse for media '{}', data: ...", self.id, media_name);
                            //- - - - logic for showing the image - - - -
                            match STANDARD.decode(&base64_data) {
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
                ["[MediaListResponse]", list_str] => {
                    println!("Client {} RECEIVED MEDIA LIST: {}", self.id, list_str);
                    let media_list : Vec<String> = list_str.split(',').filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
                    println!("Available media files: {:?}", media_list);
                },
                ["[LoginAck]", session_id] => {
                    //format: [LoginAck]::session_id
                    if let Ok(parsed_session_id) = session_id.parse::<u64>() {
                        println!("Client {} received LoginAck for session {}. Successfully logged in!", self.id, parsed_session_id);
                    } else {
                        eprintln!("Client {} received LoginAck with invalid session ID: {}", self.id, session_id);
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

        fn process_gui_command(&mut self, dest_id: NodeId, command_string: String) {
            println!("Client {} processing GUI command '{}' for {}", self.id, command_string, dest_id);
            let tokens: Vec<&str> = command_string.trim().split("::").collect();
            let command_type_str = tokens.get(0).unwrap_or(&"");
            let high_level_message_info: Option<String> = match tokens.as_slice() {
                ["[Login]", server_id] => {
                    if let Ok(parsed_server_id) = server_id.parse::<NodeId>() {
                        println!("Client {} processing Login command for server {}", self.id, parsed_server_id);
                        self.connected_server_id = Some(parsed_server_id);
                        // For real message: return Some("[Login]".to_string())
                        Some(format!("[Login]::{}",parsed_server_id))
                    } else {
                        eprintln!("Client {} received Login command with invalid server {}", self.id, server_id);
                        None
                    }
                },
                ["[Logout]"] => {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing Logout command via server {}", self.id, mem_server_id);
                        Some("[Logout]".to_string())
                    } else {
                        eprintln!("Client {} received Logout command while not logged in. Ignoring", self.id);
                        None
                    }
                },
                ["[ClientListRequest]"] => {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing ClientListRequest command via server {}", self.id, mem_server_id);
                        Some("[ClientListRequest]".to_string())
                    } else {
                        eprintln!("Client {} received ClientListRequest command while not logged in. Ignoring", self.id);
                        None
                    }
                },
                ["[MessageTo]", target_id, message_content] => {
                    if let Ok(target_client_id) = target_id.parse::<NodeId>() {
                        if let Some(mem_server_id) = self.connected_server_id {
                            println!("Client {} processing MessageTo command for client {} via server {} with content {}", self.id, target_client_id, mem_server_id, message_content);
                            Some(format!("[MessageTo]::{target_client_id}::{message_content}"))
                        } else {
                            eprintln!("Client {} received MessageTo command while not logged in. Ignoring", self.id);
                            None
                        }
                    } else {
                        eprintln!("Client {} received MessageTo command with invalid target_id: {}", self.id, target_id);
                        None
                    }
                },
                ["[ChatRequest]", peer_id] => {
                    if let Ok(_peer_id) = peer_id.parse::<NodeId>() {
                        if let Some(mem_server_id) = self.connected_server_id {
                            println!("Client {} processing ChatRequest command for peer {} via server {}", self.id, _peer_id, mem_server_id);
                            Some(format!("[ChatRequest]::{_peer_id}"))
                        } else {
                            eprintln!("Client {} received ChatRequest command while not logged in. Ignoring", self.id);
                            None
                        }
                    } else {
                        eprintln!("Client {} received ChatRequest command with invalid peer_id: {}", self.id, peer_id);
                        None
                    }
                },
                ["[ChatFinish]", peer_id] => {
                    if let Ok(_peer_id) = peer_id.parse::<NodeId>() {
                        if let Some(mem_server_id) = self.connected_server_id {
                            println!("Client {} processing ChatFinished command for peer {} via server {}", self.id, _peer_id, mem_server_id);
                            Some(format!("[ChatFinish]::{_peer_id}"))
                        } else {
                            eprintln!("Client {} received ChatFinish command while not logged in. Ignoring", self.id);
                            None
                        }
                    } else {
                        eprintln!("Client {} received ChatFinish command with invalid peer_id: {}", self.id, peer_id);
                        None
                    }
                },
                ["[MediaUpload]", media_name, base64_data] => {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing MediaUpload command for media '{}' via server {}", self.id, media_name, mem_server_id);
                        Some(format!("[MediaUpload]::{media_name}::{base64_data}"))
                    } else {
                        eprintln!("Client {} received MediaUpload command while not logged in. Ignoring", self.id);
                        None
                    }
                },
                ["[MediaDownloadRequest]", media_name] => {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing MediaDownloadRequest command for media '{}' via server {}", self.id, media_name, mem_server_id);
                        Some(format!("[MediaDownloadRequest::{media_name}"))
                    } else {
                        eprintln!("Client {} received MediaDownloadRequest command while not logged in. Ignoring", self.id);
                        None
                    }
                },
                ["[HistoryRequest]", client_id, target_id] => {
                    if let (Some(_client_id), Some(_target_id)) = (client_id.parse::<NodeId>().ok(), target_id.parse::<NodeId>().ok()) {
                        if let Some(mem_server_id) = self.connected_server_id {
                            println!("Client {} processing HistoryRequest command for history between {} and {} via server {}", self.id, _client_id, _target_id, mem_server_id);
                            Some(format!("[HistoryRequest]::{_client_id}::{_target_id}"))
                        } else {
                            eprintln!("Client {} received HistoryRequest command while not logged in. Ignoring", self.id);
                            None
                        }
                    } else {
                        eprintln!("Client {} received HistoryRequest command with invalid client/target id: {}", self.id, command_string);
                        None
                    }
                },
                ["[MediaListRequest]"] => {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing MediaListRequest command via server {}", self.id, mem_server_id);
                        Some("[MediaListRequest]".to_string())
                    } else {
                        eprintln!("Client {} received MediaListRequest command while not logged in. Ignoring", self.id);
                        None
                    }
                },
                ["[MediaBroadcast]", media_name, base64_data] => {
                    if let Some(mem_server_id) = self.connected_server_id {
                        println!("Client {} processing MediaBroadcast command for media '{}' via server {}", self.id, media_name, mem_server_id);
                        Some(format!("[MediaBroadcast]::{media_name}::{base64_data}"))
                    } else {
                        eprintln!("Client {} received MediaBroadcast command while not logged in. Ignoring", self.id);
                        None
                    }
                },

                ["[FloodRequired]",action] => {
                    println!("Client {} received FloodRequired command due to action: {}", self.id, action);
                    self.start_flood_discovery();
                    None
                },
                _ => {
                    eprintln!("Client {} received unrecognized GUI command: {}", self.id, command_string);
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
                            path
                        },
                        None => {
                            eprintln!("Client {} could not compute route to server {}. Starting flood discovery.", self.id, dest_id);
                            self.start_flood_discovery();
                            return;
                        }
                    };
                    let routing_header = SourceRoutingHeader {
                        hops: route.clone(),
                        hop_index: 1,
                    };
                    println!("♥ BEST PATH IS : {:?}",routing_header.hops);

                    let message_data_bytes = high_level_message_content.into_bytes();
                    const FRAGMENT_SIZE: usize = 128;
                    let total_fragments = (message_data_bytes.len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
                    let mut fragments = Vec::new();
                    let session_id = self.get_next_session_id();
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
                    if command_type_str == &"[Logout]" {
                        println!("Client {} successfully sent Logout message. Disconnecting internally.", self.id);
                        self.connected_server_id = None;
                    }
                } else {
                    eprintln!("Client {} cannot send high-level message, no server connected.", self.id);
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
                if let Some(edge_index) = self.network_graph.find_edge(to_idx, from_idx) {
                    let current_weight = *self.network_graph.edge_weight(edge_index).unwrap();
                    *self.network_graph.edge_weight_mut(edge_index).unwrap() = current_weight.saturating_add(1);
                    println!("Client {} incremented weight for link {} -> {} to {}", self.id, to, from, current_weight.saturating_add(1));
                } else {
                    println!("Client {} cannot increment drop: link {} -> {} not found in graph", self.id, to, from);
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

        fn get_node_info(&self, node_id: NodeId) -> Option<&NodeInfo> {
            self.node_id_to_index.get(&node_id).and_then(|&idx| self.network_graph.node_weight(idx))
        }

        fn send_to_neighbor(&mut self, neighbor_id: NodeId, packet: Packet) -> Result<(), String> {
            if let Some(sender) = self.packet_send.get(&neighbor_id) {
                match sender.send(packet.clone()) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(format!("Failed to send packet to neighbor {}: {}", neighbor_id, e)),
                }
            } else {
                Err(format!("Neighbor {} not found in packet_send map.", neighbor_id))
            }
        }
    }

    /*
    #[cfg(test)]
    mod tests {
        use super::*;
        use crossbeam_channel::{unbounded, Receiver, Sender};
        use std::collections::HashMap;
        use wg_2024::packet::{Fragment, Packet, PacketType, Nack, NackType, FloodRequest, FloodResponse, NodeType as PktNodeType};
        use wg_2024::controller::{DroneCommand, DroneEvent};
        use wg_2024::network::{NodeId, SourceRoutingHeader};
        use crate::simulation_controller::gui_input_queue::{new_gui_input_queue, SharedGuiInput};
        use petgraph::stable_graph::{StableGraph};
        use std::time::Duration;

        //helper function to create MyClient in mock channels
        fn setup_client(client_id: NodeId) -> (MyClient, Sender<DroneCommand>, Receiver<DroneEvent>, Sender<Packet>, HashMap<NodeId, Receiver<Packet>>, SharedGuiInput) {
            let (sim_contr_send_tx, sim_contr_send_rx) = unbounded::<DroneEvent>(); //C -> SC
            let (sim_contr_recv_tx, sim_contr_recv_rx) = unbounded::<DroneCommand>(); //SC -> C
            let (packet_recv_tx, packet_recv_rx) = unbounded::<Packet>(); // net -> C
            let mut packet_send_map = HashMap::new();
            let mut neighbor_receivers = HashMap::new();
            let nghb1_id = 10;
            let nghb2_id = 20;
            let (nghb1_send_tx, nghb1_recv_rx) = unbounded::<Packet>();
            packet_send_map.insert(nghb1_id, nghb1_send_tx);
            neighbor_receivers.insert(nghb1_id, nghb1_recv_rx);
            let (nghb2_send_tx, nghb2_recv_rx) = unbounded::<Packet>();
            packet_send_map.insert(nghb2_id, nghb2_send_tx);
            neighbor_receivers.insert(nghb2_id, nghb2_recv_rx);
            let gui_input = new_gui_input_queue();

            let client = MyClient::new(
                client_id,
                packet_recv_rx,
                packet_send_map,
                HashMap::new(),
                None,
            );

            (client, sim_contr_recv_tx, sim_contr_send_rx, packet_recv_tx, neighbor_receivers, gui_input)
        }

        //- - - - unit tests - - - -

        /*
        //unit test: [Login] token
        #[test]
        fn test_process_gui_command_login() {
            let client_id = 1;
            let server_id = 100;
            let (mut client, _sim_contr_tx, _sim_contr_rx, _packet_tx, _neighbor_rx, gui_input) = setup_client(client_id);

            push_gui_message(&gui_input, client_id, server_id, "[Login]".to_string());

            let gui_messages = pop_all_gui_messages(&client.gui_input, client_id);
            for (dest_server_id, msg_string) in gui_messages {
                //mock best_path function for this test
                let original_best_path = client.best_path;
                client.best_path = |src, dest| {
                    if src == client_id && dest == server_id {
                        Some(vec![client_id, 10, 50, server_id])
                    } else {
                        None
                    }
                };
                client.process_gui_command(dest_server_id, msg_string);
                //ripristina funzione originale o ridefinisci per ogni test
            }
            //verifica l'output: un pacchetto MsgFragment dovrebbe essere inviato
            //non possiamo leggere direttamente dai senders in packet_send_map qui
            //questo test unitario isolato è difficile perché process_gui_command chiama send logic
            //è meglio fare un test di integrazione per questo scenario
            //
            //alternativa per test unitario: verifica solo il parsing e la preparazione del messaggio
            //modifica process_gui_command per restituire il messaggio di alto livello preparato
            //
            //dato che process_gui_command chiama direttamente la logica di invio, un test unitario puro è difficile
            //spostiamo questo allo scenario di integrazione
        }
        */

        /*
        //unit test: handling receiving MsgFragment and reassembling
        #[test]
        fn test_reassemble_packet() {
            let client_id = 1;
            let (mut client, _sim_contr_tx, _sim_contr_rx, _packet_tx, _neighbor_rx, _gui_input) = setup_client(client_id);

            let session_id = 123;
            let source_id = 2; //the other client

            let fragment1 = Fragment { fragment_index : 0, total_n_fragments : 3, length : 5, data : [1, 2, 3, 4, 5, 0, ..; 128] };
            let fragment2 = Fragment { fragment_index : 1, total_n_fragments : 3, length : 5, data : [6, 7, 8, 9, 10, 0, ..; 128] };
            let fragment3 = Fragment { fragment_index : 2, total_n_fragments : 3, length : 5, data : [11, 12, 13, 14, 15, 0, ..; 128] };

            let rh = SourceRoutingHeader { hops : vec![], hop_index : 0 };

            let mut packet1 = Packet { pack_type : PacketType::MsgFragment(fragment1.clone()), routing_header : rh.clone(), session_id };
            let mut packet2 = Packet { pack_type :  PacketType::MsgFragment(fragment2.clone()), routing_header : rh.clone(), session_id };
            let mut packet3 = Packet { pack_type :   PacketType::MsgFragment(fragment3.clone()), routing_header : rh.clone(), session_id };

            client.reassemble_packet(&fragment1, &mut packet1);
            assert!(client.received_messages.contains_key(&session_id));
            let state = client.received_messages.get(&session_id).unwrap();
            assert_eq!(state.received_indices.len(), 1);
            assert_eq!(state.data.len(), (fragment1.total_n_fragments * fragment1.length as u64) as usize);
            assert_eq!(&state.data[0..fragment1.length as usize], &fragment1.data[0..fragment1.length as usize]);

            client.reassemble_packet(&fragment2, &mut packet2);
            let state = client.received_messages.get(&session_id).unwrap();
            assert_eq!(state.received_indices.len(), 2);
            assert_eq!(&state.data.[fragment1.length as usize..(fragment1.length +fragment2.length) as usize], &fragment2.data[0..fragment2.length as usize]);

            //mock process_received_high_level_message
            let mut high_level_message_processed = false;
            let original_process = client.process_received_high_level_message;
            client.process_received_high_level_message = |_, msg_string, stc_id, sess_id| {
                println!("Mocked processing high-level message: '{}' from {} (session {})", msg_string, source_id, sess_id);
                high_level_message_processed = true;
                //possibile aggiunta asserzioni messaggio ri-assemblato
                let expected_message = "AABBCCDD".to_string();
                //in questo test unitario, i byte sono {1..15} quindi il messaggio decodificato sarebbe "0102030405060708090a0b0c0d0e0f" in hex
                //dobbiamo testare che 'data' contenga i byte corretti e che 'process_received_high_level_message' venga chiamata
                let expected_bytes : Vec<u8> = vec![20-34];
                //testiamo che la funzione mock venga chiamata. il test sul contenuto decodificato dipende dalla logica di decodifica del client
            };

            client.reassemble_packet(&fragment3, &mut packet3);
            assert!(!client.received_messages.contains_key(&session_id));
            //verifica che la funzione di processing del messaggio di livello si stata chiamata
            //se la mock non ha effetti esterni, si può solo dedurre che sarebbe stata chiamata se non fosse mockata

            //ripristina funzione originale o ridefinisci per ogni test
        }
        */

        //unit test: handling NACK (ErrorInRouting)
        #[test]
        fn test_process_nack_error_in_routing() {
            let client_id = 1;
            let (mut client, _sim_contr_tx, _sim_contr_rx, _packet_tx, _neighbor_rx, _gui_input) = setup_client(client_id);

            let session_id = 456;
            let nack_packet_id = 0;
            let bad_node_id = 99;
            let nack_type = NackType::ErrorInRouting(bad_node_id);
            let nack = Nack { fragment_index : nack_packet_id, nack_type : nack_type.clone() };
            let rh = SourceRoutingHeader { hops: vec![client_id, 10, bad_node_id, 50, 100], hop_index: 2 };

            let mut packet = Packet { pack_type : PacketType::Nack(nack.clone()), routing_header : rh.clone(), session_id };

            //simulation: client sent message with this session_id and route
            client.sent_messages.insert(session_id, SentMessageInfo {
                fragments : vec![],
                original_routing_header : rh.clone(),
                received_ack_indices : HashSet::new(),
                route_needs_recalculation : false,
            });

            client.process_nack(&nack, &mut packet);

            //verify that route for this session has been marked for recalculation
            assert!(client.sent_messages.get(&session_id).unwrap().route_needs_recalculation);
            println!("Client {} marked route for session {} for recalculation due to ErrorInRouting.", client_id, session_id);

            //verify that flood discovery hasn't been initialized automatically from process_nack
        }

        //unit test: handling ANCK (Dropped)
        #[test]
        fn test_process_nack_dropped() {
            let client_id = 1;
            let (mut client, _sim_contr_tx, _sim_contr_rx, _packet_tx, _neighbor_rx, _gui_input) = setup_client(client_id);

            let session_id = 789;
            let nack_packet_id = 5;
            let nack_type = NackType::Dropped;
            let nack = Nack { fragment_index : nack_packet_id, nack_type };
            //we have to penalize the problematic link
            //mock raph
            client.network_graph = StableGraph::new();
            let client_idx = client.network_graph.add_node(NodeInfo { id : client_id, node_type : PktNodeType::Client });
            client.node_id_to_index.insert(client_id, client_idx);
            let drone10_idx = client.network_graph.add_node(NodeInfo { id : 10, node_type : PktNodeType::Drone });
            client.node_id_to_index.insert(10, drone10_idx);
            let drone5_idx = client.network_graph.add_node(NodeInfo { id : 5, node_type : PktNodeType::Drone });
            client.node_id_to_index.insert(5, drone5_idx);
            let edge_idx = client.network_graph.add_edge(client_idx, drone10_idx, 1);

            let rh = SourceRoutingHeader { hops : vec![10, client_id], hop_index : 1 };
            let mut packet = Packet { pack_type : PacketType::Nack(nack.clone()), routing_header : rh.clone(), session_id };

            let initial_weight = client.network_graph.edge_weight(edge_idx).unwrap().clone();

            client.process_nack(&nack, &mut packet);

            //verify that the weight changed
            let new_weight = client.network_graph.edge_weight(edge_idx).unwrap();
            println!("Initial weight: {}, New weight: {}", initial_weight, new_weight);
            assert!(*new_weight > initial_weight);
        }

        //unit test: creation and updating topology from FloodResponse
        #[test]
        fn test_create_topology() {
            let client_id = 1;
            let (mut client, _sim_contr_tx, _sim_contr_rx, _packet_tx, _neighbor_rx, _gui_input) = setup_client(client_id);

            //simulation FloodResponse tracking a path
            let path_trace: Vec<(NodeId, PktNodeType)> = vec![
                (10, PktNodeType::Drone),
                (20, PktNodeType::Drone),
                (30, PktNodeType::Drone),
                (100, PktNodeType::Drone),
            ];
            let flood_response = FloodResponse { flood_id : 99, path_trace : path_trace.clone()
            };
     */