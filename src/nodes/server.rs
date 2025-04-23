/*
COSE DA FARE :
- le varie funzioni di interazione con i client: con i vari codici da mettere nei frammenti
- implementare sto grafo dimmerda
- fare simulazioni

*/

use std::collections::HashMap;
use crossbeam_channel::{Receiver, RecvError, Sender};
use eframe::egui::accesskit::Node;
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use log::{info, error, warn, debug};


#[derive(Debug, Clone)]
pub struct server {
    pub id: u8, // Server ID
    pub received_fragments: HashMap<(u64, NodeId), Vec<Option<[u8; 128]>>>, // Maps (session_id, src_id) to fragment data
    pub fragment_lengths: HashMap<(u64, NodeId), u8>, // Maps (session_id, src_id) to length of the last fragment
    pub packet_sender: HashMap<NodeId, Sender<Packet>>, // Hashmap containing each sender channel to the neighbors (channels to send packets to clients)
    pub packet_receiver: Receiver<Packet>, // Channel to receive packets from clients/drones
    pub registered_clients: HashMap<NodeId,Vec<NodeId>>,
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
            registered_clients: HashMap::new(),
            //recovery_in_progress: HashMap::new(),
            //drop_counts: HashMap::new(),
        }
    }

    /// Run the server to process incoming packets and handle fragment assembly
    pub fn run(&mut self) {
        // Initialize logger (logging setup for your program)
        if let Err(e) = env_logger::builder().try_init() {
            eprintln!("Failed to initialize logger: {}", e);
        }

        info!("Server {} started running.", self.id);

        loop {
            match self.packet_receiver.recv() {
                Ok(mut packet) => {
                    match &packet.pack_type {
                        PacketType::MsgFragment(fragment) => {
                            self.send_ack(&mut packet, &fragment);
                            self.handle_fragment(packet.session_id, fragment, packet.routing_header);
                        }
                        PacketType::Nack(nack) => {
                            self.handle_nack(packet.session_id, nack, packet.routing_header);
                        }
                        PacketType::Ack(ack) => {
                            // Process ACKs, needed for simulation controller to know when to print the message
                            self.handle_ack(packet.session_id, ack, packet.routing_header);
                        }
                        PacketType::FloodRequest(flood_request) => {
                            // Process flood requests from clients trying to discover the network
                            // Server should send back a flood response
                            self.handle_flood_request(packet.session_id, flood_request, packet.routing_header);
                        }
                        PacketType::FloodResponse(flood_response) => {
                            // Process flood responses containing network information
                            self.handle_flood_response(packet.session_id, flood_response, packet.routing_header);
                        }
                        _ => {
                            warn!("Server {} received unexpected packet type.", self.id);
                        }
                    }
                }
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

        // Check if the fragment is already received
        if entry[fragment.fragment_index as usize].is_some() {
            warn!("Duplicate fragment {} received for session {:?}", fragment.fragment_index, key);
            return;
        }

        // Store the fragment data
        entry[fragment.fragment_index as usize] = Some(fragment.data);

        // Update the last fragment's length if applicable
        if fragment.fragment_index == fragment.total_n_fragments - 1 {
            self.fragment_lengths.insert(key, fragment.length);
            info!("Last fragment received for session {:?} with length {}.", key, fragment.length);
        }

        // Check if all fragments have been received
        if entry.iter().all(Option::is_some) {
            // All fragments received, handle complete message
            self.handle_complete_message(key, routing_header.clone());
        }
    }
    /*fn handle_complete_message(&mut self, key: (u64, NodeId), routing_header: SourceRoutingHeader) {
        let fragments = self.received_fragments.remove(&key).unwrap();
        let total_length = fragments.len() * 128 - 128 + self.fragment_lengths.remove(&key).unwrap_or(128) as usize;

        // Reassemble the message
        let mut message = Vec::with_capacity(total_length);
        if fragments.iter().any(|f| f.is_none()) {
            error!("Missing fragments detected for session {:?}", key);
            return; // Handle incomplete fragments gracefully
        }
        for fragment in fragments {
            message.extend_from_slice(&fragment.unwrap());
        }
        message.truncate(total_length);
        info!("Server reassembled message for session {:?}: {:?}", key, message);


        //chat server implementation:
        let message_string = String::from_utf8_lossy(&message).to_string();
        let session_id: u64 = key.0;
        let client_id = key.1;

        let tokens: Vec<&str> = message_string.trim().splitn(3, ':').collect();

        match tokens.as_slice() {
            ["registration_to_chat"] => {
                self.registered_clients
                    .entry(session_id as NodeId)
                    .or_default()
                    .push(client_id);
                info!("Client {} registered to chat in session {}", client_id, session_id);
            }
            ["client_list?"] => {
                if let Some(sender) = self.packet_sender.get(&client_id) {
                    let clients = self
                        .registered_clients
                        .get(&(session_id as NodeId))
                        .cloned()
                        .unwrap_or_default();
                    let response = format!("client_list!: {:?}", clients);
                    self.send_chat_message(session_id as u64, client_id, response, routing_header);
                }
            }
            ["message_for?", target_id_str, msg] => {
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    if (self.registered_clients
                        .get(&(session_id as NodeId))
                        .map_or(false, |list| list.contains(&target_id)))
                    {
                        let response = format!("message_from!:{}:{}", client_id, msg);
                        self.send_chat_message(session_id as u64, target_id, response, routing_header);
                    } else {
                        let response = "error_wrong_client_id!".to_string();
                        self.send_chat_message(session_id as u64, client_id, response, routing_header);
                    }
                }
            }
            _ => {
                warn!("Unrecognized message: {}", message_string);
            }
        }

    }*/


    fn handle_complete_message(&mut self, key: (u64, NodeId), routing_header: SourceRoutingHeader) {
        let fragments = self.received_fragments.remove(&key).unwrap();
        let total_length = fragments.len() * 128 - 128 + self.fragment_lengths.remove(&key).unwrap_or(128) as usize;

        let mut message = Vec::with_capacity(total_length);
        for fragment in fragments {
            message.extend_from_slice(&fragment.unwrap());
        }
        message.truncate(total_length);

        let message_string = String::from_utf8_lossy(&message).to_string();
        let session_id: u64 = key.0;
        let client_id = key.1;

        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();

        match tokens.as_slice() {
            ["[Login]"] => {
                self.registered_clients
                    .entry(session_id as NodeId)
                    .or_default()
                    .push(client_id);
                info!("Client {} registered to chat in session {}", client_id, session_id);
            }
            ["[ClientListRequest]"] => {
                if let Some(sender) = self.packet_sender.get(&client_id) {
                    let clients = self
                        .registered_clients
                        .get(&(session_id as NodeId))
                        .cloned()
                        .unwrap_or_default();
                    let response = format!("[ClientListResponse]::{:?}", clients);
                    self.send_chat_message(session_id, client_id, response, routing_header);
                }
            }
            ["[ChatRequest]", target_id_str] => {
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    let success = self.registered_clients
                        .get(&(session_id as NodeId))
                        .map_or(false, |list| list.contains(&target_id));
                    let response = format!("[ChatStart]::{}", success);
                    self.send_chat_message(session_id, client_id, response, routing_header);
                }
            }
            ["[MessageTo]", target_id_str, msg] => {
                if let Ok(target_id) = target_id_str.parse::<NodeId>() {
                    if self.registered_clients
                        .get(&(session_id as NodeId))
                        .map_or(false, |list| list.contains(&target_id))
                    {
                        let response = format!("[MessageFrom]::{}::{}", client_id, msg);
                        self.send_chat_message(session_id, target_id, response, routing_header);
                    } else {
                        self.send_chat_message(session_id, client_id, "error_wrong_client_id!".to_string(), routing_header);
                    }
                }
            }
            ["[ChatFinish]"] => {
                if let Some(clients) = self.registered_clients.get_mut(&(session_id as NodeId)) {
                    clients.retain(|&id| id != client_id);
                    info!("Client {} finished chat in session {}", client_id, session_id);
                }
            }
            ["[Logout]"] => {
                if let Some(clients) = self.registered_clients.get_mut(&(session_id as NodeId)) {
                    clients.retain(|&id| id != client_id);
                    info!("Client {} logged out from session {}", client_id, session_id);
                }
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
                // Nacktype: ErrorInRouting ---> floodRequest --> find crashed drone --> remove from graph --> calculate path to dijstra and keep sending
                //put a limit of 5 dropped fragments, at the 6th the server will flood the network sending a floodrequest
                // to the drone to whom it is connected.

                //Modus Operandi client/server:
                // prima operazione generale: floodRequest.
                /* Riceverò delle flood_response e da queste mi costruisco il grafo.
                    Una volta costruito il grafo ad ogni nack::Dropped che ricevo vado a modificare il peso di tutti i
                    collegamenti relativi a quel drone. Gli altri nack implicano che c'è qualche problema del tipo crashed Drone.
                    Come gestisco? Mando floodRequest, riceverò floodResponse che mi indicheranno il drone problematico, carpita
                    questa informazione lo vado a rimuovere dal grafo.
                 */
                //quando ricevo un nack::ErrorInRouting ... faccio un flooding normale (creo una flood_request normale),
                //poi riceverò flood_response e agisco sul grafo come al solito.

                // grafo fatto con pet_graph.

                warn!("Received Nack::Dropped, modifying the costs in the graph")

            }
            _ => {
                    warn!("Received ErrorInRouting/DestinationIsDrone/UnexpectedRecipient NACK type, sending flood request");
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
    fn send_ack(&mut self, packet: &mut Packet, fragment: &Fragment) {
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
        info!("Received Flood request for session {} with flood id {}", session_id, flood_request.flood_id);

        let flood_response= PacketType::FloodResponse(
            FloodResponse {
                flood_id: session_id,
                path_trace: vec![(self.id,NodeType::Server)],
            }
        );
        let packet_flood_response= Packet{
            routing_header: SourceRoutingHeader{
                hop_index: 0,
                hops: source_routing_header.hops.iter().rev().copied().collect(),
            },
            session_id,
            pack_type: flood_response,
        };
        self.packet_sender.iter().for_each(|(_, sender)| {
            sender.try_send(packet_flood_response).unwrap()
        })

    }

    //handle_flood_response is still to be checked properly and tested.
    fn handle_flood_response(&mut self, session_id: u64, flood_response: &FloodResponse, routing_header: SourceRoutingHeader) {
        info!("Received FloodResponse for flood_id {} in session {}", flood_response.flood_id, session_id);
        //obiettivo: fare tutta quella roba strana con il grafo.

        // Extract the path from the response
        let path = flood_response.path_trace
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<NodeId>>();

        info!("New path discovered: {:?}", path);
        //integrare tutti i collegamenti e nodi del path dentro il grado

        // TODO: Use this path for future communications with this client
        // You might want to store this path in a new field in your server struct

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

        // Reverse route from original to find target
        let mut hops = original_header.hops.clone();
        hops.reverse();
        hops.push(target_id); // may need more logic here later

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

}

#[cfg(test)]
mod tests {
    use super::*; // Assuming the server module and related types are in the parent module
    use crossbeam_channel::unbounded;
    use wg_2024::packet::{Packet, PacketType, Fragment, Ack};
    use wg_2024::network::{SourceRoutingHeader, NodeId};

    #[test]
    fn test_handle_nack_dropped_fragment() {
        // Create channels for sending/receiving packets
        let (server_tx, server_rx) = unbounded();
        let (packet_tx, packet_rx) = unbounded();

        // Create server and insert test data
        let mut srv = server::new(1, server_tx, packet_rx);

        // Session and routing setup
        let session_id: u64 = 44;
        let src_node: NodeId = 10;
        let dst_node: NodeId = 20;
        let routing_header = SourceRoutingHeader {
            hop_index: 0,
            hops: vec![src_node, dst_node],
        };

        // Create and store a fragment in the server's state for testing
        let mut data = [0u8; 128];
        for i in 0..128 {
            data[i] = i as u8;
        }

        // Create fragment structures
        let fragment0 = Fragment {
            fragment_index: 0,
            total_n_fragments: 2,
            length: 128,
            data,
        };

        let fragment1 = Fragment {
            fragment_index: 1,
            total_n_fragments: 2,
            length: 64,
            data,
        };

        // Manually insert fragments into server state
        let key = (session_id, src_node);
        srv.received_fragments.insert(key, vec![Some(data), Some(data)]);
        srv.fragment_lengths.insert(key, 64);

        // Create a NACK for fragment 0 with Dropped type
        let nack = Nack {
            fragment_index: 0,
            nack_type: NackType::Dropped,
        };

        // Packet from destination to source (reversed routing)
        let nack_packet = Packet {
            pack_type: PacketType::Nack(nack),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: vec![dst_node, src_node], // Reversed route
            },
            session_id,
        };

        // Send the NACK to the server
        packet_tx.send(nack_packet).unwrap();

        // Process the NACK
        if let Ok(packet) = packet_rx.recv() {
            match &packet.pack_type {
                PacketType::Nack(nack) => {
                    srv.handle_nack(packet.session_id, nack, packet.routing_header);
                },
                _ => panic!("Expected a NACK packet"),
            }
        }

        // Check the server's response - should resend the fragment with a new route
        let responses: Vec<Packet> = server_rx.try_iter().collect();
        assert!(!responses.is_empty(), "Server should have sent a response to the NACK");

        // Find resent fragments
        let resent_fragments: Vec<&Packet> = responses.iter()
            .filter(|p| {
                if let PacketType::MsgFragment(fragment) = &p.pack_type {
                    fragment.fragment_index == 0 && p.session_id == session_id
                } else {
                    false
                }
            })
            .collect();

        assert!(!resent_fragments.is_empty(), "Server should have resent the fragment");
    }

}
