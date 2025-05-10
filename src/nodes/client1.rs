use std::collections::{HashMap, HashSet};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::{/*FloodRequest, FloodResponse,*/ MsgFragment};
use wg_2024::drone::Drone;
//use crate::droneK::drone::MyDrone;
use crossbeam_channel::{select, Receiver, Sender};
use std::{fs, thread};
use petgraph::graph::{Graph, Node, NodeIndex};
use petgraph::Undirected;
use petgraph::stable_graph::StableGraph;
use petgraph::algo::dijkstra;
use petgraph::visit::EdgeRef;
use wg_2024::controller::DroneCommand::SetPacketDropRate;
/*

WORKING ON:
1.definizione della struct MyClient per gestire correttamente la mappa node_id_to_index con NodeId come chiave e il grafo network_graph con pesi sugli archi.
2.definizione della struct NodeInfo per rimuovere il derive Hash.
3.funzione create_topology per popolare il grafo con pesi iniziali sugli archi.
4.implementazione di un metodo best_path per calcolare le rotte utilizzando l'algoritmo di Dijkstra di petgraph.
5.implementazione di metodi per aggiornare dinamicamente il grafo in risposta ai NACK (increment_drop e remove_node_from_graph).
6.integrazione della chiamata a best_path nella logica di invio dei messaggi di alto livello (come nell'esempio send_chat_message).
7.modifica della funzione process_nack per invocare gli aggiornamenti del grafo.

*/


/*

**********AGGIUNGERE LE FUNZIONI PER MANDARE I PACCHETTI***********

 */



#[derive(Debug, Clone)]
pub struct ReceivedMessageState {
    pub data : Vec<u8>,
    pub total_fragments : u64,
    pub received_indeces : HashSet<u64>,
}

#[derive(Debug, Clone)]
pub struct SentMessageInfo {
    pub fragments : Vec<Fragment>,
    pub original_routing_header : SourceRoutingHeader,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeInfo {
    id : NodeId,
    node_type : NodeType,
    //possibile aggiunta di info (tipo PDR)
}

#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_recv: Receiver<DroneCommand>,
    pub sent_messages: HashMap<u64, SentMessageInfo>,
    pub received_messages : HashMap<u64, ReceivedMessageState>, //per determinare quando il messaggio è completo (ha tutti i frammenti)
    network_graph : StableGraph<NodeInfo, usize>, //grafo per memorizzare info sui nodi
    node_id_to_index : HashMap<NodeId, NodeIndex>, //mappatura da nodeid a indice interno del grafo
    active_flood_discoveries: HashMap<u64, FloodDiscoveryState>, //struttura per tracciare floodrequest/response
}

#[derive(Debug, Clone)]
struct FloodDiscoveryState {
    initiator_id : NodeId,
    //possibile aggiunta di info sui nodi in attesa o timer
}

impl MyClient{
    pub fn new(id: NodeId, sim_contr_recv: Receiver<DroneCommand>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, sent_messages:HashMap<u64 , SentMessageInfo>) -> Self {
        Self {
            id,
            sim_contr_recv,
            packet_recv,
            packet_send,
            sent_messages,
            received_messages : HashMap::new(),
            network_graph: StableGraph::new(),
            node_id_to_index: HashMap::new(),
            active_flood_discoveries: HashMap::new(),
        }
    }

    fn run (&mut self){
        let mut seen_flood_ids = HashSet::new();
        println!("Client {} starting run loop", self.id);
        //implementa logica per quando avviare flood discovery (e.g. all'avvio o quando una rotta fallisce)
        loop{
            select! {
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet by client {} ...", self.id);
                    if let Ok(packet) = packet {
                        println!("Packet received by drone {} : {:?}",self.id, packet);
                        self.process_packet(packet, &mut seen_flood_ids);
                    }
                },
                recv(self.sim_contr_recv) -> command => {
                    if let Ok(command) = command {
                        println!("Command received by client {} : {:?}", self.id, command);
                        //implementa gestione comandi dal SC (crash, addsender, removesender, setpacketdroprate)
                        match command {
                            DroneCommand::AddSender(node_id, sender) => {
                                println!("Client {} received AddSender command for node {}", self.id, node_id);
                                self.packet_send.insert(node_id, sender);
                                //riavvia flooddiscovery dato che è stato aggiunto un vicino
                            },
                            DroneCommand::RemoveSender(node_id) => {
                                println!("Client {} received RemoveSender command for node {}", self.id, node_id);
                                self.packet_send.remove(&node_id);
                                self.remove_node_from_graph(node_id);
                            },
                            DroneCommand::Crash | DroneCommand::SetPacketDropRate(_) => {
                                eprintln!("Client {} received unexpected command: {:?}", self.id, command);
                            }
                        }
                    }
                }
                //aggiungere qui select branch per gui_input o altri eventi per invio messaggi
            }
        }
    }

    fn process_packet (&mut self, mut packet: Packet, seen_flood_ids: &mut HashSet<u64>) {
        let packet_type = packet.pack_type.clone();
        match packet_type {
            PacketType::FloodRequest(request) => { //CONTROLLA SE IL CLIENT PUò PROPAGARE FLOODREQUESTS
                //println!("Client {} received FloodRequest {}, ignoring as client should not propagate.", self.id, request.flood_id);
                self.process_flood_request(&request, seen_flood_ids , packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                self.create_topology(&response);
                //implementa logica per tracciare floodresponses e finalizzare discovery per un determinato flood_id
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut packet , &fragment);
                self.reassemble_packet(&fragment , &mut packet);
            },
            PacketType::Ack(ack) => {
                //implementa logica per gestire ack
                //quando ricevi ACK, segna come ricevuto in self.sent_messages
                println!("Client {} received ACK for session {}, fragment {}", self.id, packet.session_id, ack.fragment_index);
                if let Some(sent_msg_info) =  self.sent_messages.get_mut(&packet.session_id) {
                    //segna come ricevuto in self.sent_messages e rimuovilo dalla lista di quelli in trasmissione
                    //possibile necessità di modificare SentMessageInfo
                }
            },
            PacketType::Nack(nack ) => {
                //implementa logica per gestire nack
                self.process_nack(&nack , &mut packet);
            }
        }
    }

    /*
    i messaggi che vengono mandati vengono divisi in un vettore di frammenti associato al suo id e conservati in un hashmap
    quando recuperiamo il messaggio cerchiamo l'id e troviamo il fragment index
     */

    fn process_flood_request(&mut self, request: &FloodRequest, seen_flood_ids: &mut HashSet<u64>, header: SourceRoutingHeader){
        let mut updated_request = request.clone();
        updated_request.path_trace.push((self.id, NodeType::Client));
        println!("Client {} processed FloodRequest {}. Path trace now: {:?}", self.id, request.flood_id, updated_request.path_trace);
        let flood_response = FloodResponse {
            flood_id : request.flood_id,
            path_trace : updated_request.path_trace,
        };
        let mut response_routing_header = header.clone();
        response_routing_header.hops.reverse();
        response_routing_header.hop_index = 1;
        let reversed_hops = response_routing_header.hops.clone();
        if response_routing_header.hops.len() > 1 {
            let next_hop = response_routing_header.hops[response_routing_header.hop_index];
            if let Some(sender) = self.packet_send.get(&next_hop) {
                let response_packet = Packet {
                    pack_type : PacketType::FloodResponse(flood_response),
                    routing_header : response_routing_header,
                    session_id : request.flood_id,
                };
                match sender.send(response_packet) {
                    Ok(()) => {
                        println!("Client {} sent FloodResponse {} back to {}", self.id, request.flood_id, next_hop);
                    }
                    Err(e) => {
                        eprintln!("Client {} failed to send FloodResponse {} back to {}: {}", self.id, request.flood_id, next_hop, e);
                    }
                }
            } else {
                eprintln!("Client {} error: no sender found for FloodResponse next hop {}", self.id, next_hop);
            }
        } else {
            eprintln!("Client {} error: cannot send FloodResponse, inverted routing header hops too short {:?}", self.id, reversed_hops);
        }

        //rivedi completamente logica (PROTOCOLLO)(?)
        seen_flood_ids.insert(request.flood_id);

    }

    fn start_flood_discovery(&mut self) {
        println!("Client {} starting flood discovery", self.id);
        //1.genera flood_id unico
        let new_flood_id = rand::random::<u64>();
        //2.crea floodrequest
        let flood_request = FloodRequest {
            flood_id : new_flood_id,
            initiator_id : self.id,
            path_trace : vec![(self.id, NodeType::Client)],
        };
        //3.inivia floodrequest ai ngbh
        let flood_packet = Packet {
            pack_type : PacketType::FloodRequest(flood_request),
            routing_header : SourceRoutingHeader {
                hop_index : 0,
                hops : vec![],
            },
            session_id : new_flood_id,
        };

        println!("Client {} sending FloodRequest {} to all neighbors", self.id, new_flood_id);
        //4.invia floodrequest a tutti i ngbh
        for (&neighbor_id, sender) in &self.packet_send {
            match sender.send(flood_packet.clone()) {
                Ok(_) => println!("Client {} sent FloodRequest to neighbor {}",  self.id, neighbor_id),
                Err(e) => eprintln!("Client {} failed to send FloodRequest to neighbor {}: {}",  self.id, neighbor_id, e),
            }
        }
        //implementa come mantenere traccia dei flood_id attivi e delle risposte attese/ricevute
        self.active_flood_discoveries.insert(new_flood_id, FloodDiscoveryState{ initiator_id : self.id });
    }

    fn reassemble_packet (&mut self, fragment: &Fragment, packet : &mut Packet){
        let session_id = packet.session_id;
        let src_id = packet.routing_header.hops.first().copied().unwrap_or_else(|| {
            eprintln!("Client {} error: Received packet with empty hops in routing header for session {}", self.id, session_id);
            0 //possibile logica errata
        });
        let key = session_id;
        let fragment_len = fragment.length as usize;
        let offset = (fragment.fragment_index * 128) as usize;
        let state = self.received_messages.entry(key).or_insert_with(|| {
            println!("Client {} starting reassembly for session {} from src {}", self.id, session_id, src_id);
            ReceivedMessageState {
                data : vec![0u8; (fragment.total_n_fragments * 128) as usize],
                total_fragments : fragment.total_n_fragments,
                received_indeces : HashSet::new(),
            }
        });
        if state.received_indeces.contains(&fragment.fragment_index) {
            println!("Received duplicate fragment {} for session {}. Ignoring", fragment.fragment_index, session_id);
            return;
        }

        let fragment_data_slice = &fragment.data[..fragment_len];

        if offset + fragment_len <= state.data.len() {
            state.data[offset..offset + fragment_len].copy_from_slice(&fragment.data[..fragment_len]);
            //let target_slice = &mut state.data[offset..offset + fragment_len];
            //target_slice.copy_from_slice(fragment_data_slice);

            state.received_indeces.insert(fragment.fragment_index);
            println!("Client {} received fragment {} for session {}. Received {}/{} fragments.", self.id, fragment.fragment_index, session_id, state.received_indeces.len(), state.total_fragments);
            if state.received_indeces.len() as u64 == state.total_fragments {
                println!("Message for sesssion {} reassembled successfully", session_id);
                let full_message_data = state.data.clone();
                self.received_messages.remove(&key);
                //implementa logica per elaborare messaggio riassemblato (state.data) come messaggio di alto livello
                let message_string = String::from_utf8_lossy(&full_message_data);
                println!("Client {} reassembled message content: {:?}", self.id, message_string);
            }
        } else {
            eprintln!("Received fragment {} for session {} with offset {} and length {} which exceeds excepted message size {}. Ignoring fragment", fragment.fragment_index, session_id, offset, fragment_len, state.data.len());
        }
    }

    fn send_ack (&mut self, packet: &mut Packet , fragment: &Fragment) {
        let mut ack_routing_header = packet.routing_header.clone();
        ack_routing_header.hops.reverse();
        ack_routing_header.hop_index = 1;

        let mut ack_packet = Packet {
            pack_type: PacketType::Ack(Ack { fragment_index : fragment.fragment_index }),
            routing_header: ack_routing_header,
            session_id: packet.session_id,
        };

        if ack_packet.routing_header.hops.len() > 1 {
            let next_hop_for_ack = ack_packet.routing_header.hops[ack_packet.routing_header.hop_index];
            if let Some(sender) = self.packet_send.get(&next_hop_for_ack) {
                match sender.send(ack_packet) {
                    Ok(()) => println!("Client {} sent ACK for session {} fragment {} to neighbor {}", self.id, packet.session_id, fragment.fragment_index, next_hop_for_ack),
                    Err(e) => eprintln!("Client {} error sending ACK packet for session {}: {:?}", self.id, packet.session_id, e),
                }
            } else {
                eprintln!("Client {} error: no sender found for ACK next hop {}", self.id, next_hop_for_ack);
            }
        } else {
            eprintln!("Client {} error: cannot send ACK, original routing header hops too short: {:?}", self.id, packet.routing_header.hops);
        }
    }

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        println!("Client {} processing NACK type {:?} for session {}", self.id, nack.nack_type, packet.session_id);
        match &nack.nack_type {
            NackType::ErrorInRouting(problem_node_id) => {
                eprintln!("Client {} received Nack::ErrorInRouting for session {} at node {}", self.id, packet.session_id, problem_node_id);
                //implementa logica per informare il modulo routing che il nodo "problem_node_id" o il link verso di esso sono inaffidabili
                //possibile dover ricalcolare la rotta
                self.remove_node_from_graph(*problem_node_id);
                println!("Client {} removed node {} from graph due to ErrorInRouting", self.id, problem_node_id);
                //implementa invalidazione rotta usata per questa sessione o avviare ricalcolo
            }
            NackType::DestinationIsDrone => {
                eprintln!("Client {} received Nack::DestinationIsDrone for session {}. Route calculation error?", self.id, packet.session_id);
                //implemeta logica per segnalare che la rotta calcolata potrebbe essere errata
                //implementa invalidazione rotta usata
            }
            NackType::UnexpectedRecipient(received_at_node_id) => {
                eprintln!("Client {} received Nack::UnexpectedRecipient for session {}: packet arrived at node {} but expected a different one. Route mismatch or topology error?", self.id, packet.session_id, received_at_node_id);
                //implementa logica per segnalare disallineamento della rotta o errore di topologia
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
            }
            NackType::Dropped=>{
                eprintln!("Client {} received Nack::Dropped for session {}, fragment {}", self.id, packet.session_id, nack.fragment_index);
                if let Some(original_packet_info) = self.sent_messages.get(&packet.session_id) { //original_packet_info dovrebbe contenere i frammenti E l'header di routing originale
                    if let Some(fragment_to_resend) = original_packet_info.fragments.iter().find(|f| f.fragment_index == nack.fragment_index) {
                        let original_header = &original_packet_info.original_routing_header;
                        let nack_route = &packet.routing_header.hops;
                        let nack_hop_index = packet.routing_header.hop_index;
                        if nack_hop_index > 0 && nack_hop_index < nack_route.len() {
                            let from_node = nack_route[nack_hop_index - 1];
                            let dropped_at_node = nack_route[nack_hop_index];
                            println!("Client {} penalizing link {} -> {} due to Dropped NACK", self.id, from_node, dropped_at_node);
                            self.increment_drop(from_node, dropped_at_node);
                            self.increment_drop(dropped_at_node, from_node);
                            //implementa politica di ritrasmissione
                            /*let packet_to_resend = Packet {
                                pack_type : PacketType::MsgFragment(fragment_to_resend.clone()),
                                routing_header : original_header.clone(),
                                session_id : packet.session_id,
                            };
                            let first_hop = original_header.hops[original_header.hop_index];
                            match self.send_to_neighbor(first_hop, packet_to_resend) {
                                Ok(_) => println!("Client {} resent fragment {} for session {} to {}", self.id, fragment_to_resend.fragment_index, packet.session_id, first_hop),
                                Err(e) => eprintln!("Client {} failed to resend fragment {} for session {} to {}: {}", self.id, fragment_to_resend.fragment_index, packet.session_id, first_hop, e),
                            }*/
                        } else {
                            eprintln!("Client {} cannot determine dropping link: Original routing header hop_index is out of bounds for session {}", self.id, packet.session_id); // [59, 60]
                        }
                    } else {
                        eprintln!("Client {} received dropped nack for session {}, fragment {} but could not find fragment in sent_messages", self.id, packet.session_id, nack.fragment_index); // [32, 34]
                    }
                } else {
                    eprintln!("Client {} received dropped nack for session {}, but original packet info not found in sent_messages", self.id, packet.session_id); // [32, 34]
                }
            }
        }
    }

    fn create_topology(&mut self, flood_response: &FloodResponse){
        println!("Client {} processing FloodResponse for flood_id {}", self.id, flood_response.flood_id);
        let path = &flood_response.path_trace;
        if path.is_empty() {
            println!("Received empty path_trace in FloodResponse for flood_id {}", flood_response.flood_id);
            return;
        }
        for i in 0..path.len() {
            let (current_node_id, current_node_type) = path[i];
            let current_node_idx = *self.node_id_to_index.entry(current_node_id).or_insert_with(|| {
                println!("Client {} adding node {} ({:?}) to graph", self.id, current_node_id, current_node_type);
                self.network_graph.add_node(NodeInfo { id : current_node_id, node_type : current_node_type } )
            });
            if i > 0 {
                let (prev_node_id, prev_node_type) = path[i - 1];
                if let Some(&prev_node_idx) = self.node_id_to_index.get(&prev_node_id) {
                    let edge_exists_fwd = self.network_graph.contains_edge(prev_node_idx, current_node_idx);
                    let edge_exists_bwd = self.network_graph.contains_edge(current_node_idx, prev_node_idx);
                    if !edge_exists_fwd {
                        println!("Client {} adding edge: {} -> {}", self.id, prev_node_id, current_node_id);
                        self.network_graph.add_edge(prev_node_idx, current_node_idx, 0);
                    }
                    if !edge_exists_bwd {
                        println!("Client {} adding edge: {} -> {}", self.id, current_node_id, prev_node_id);
                        self.network_graph.add_edge(current_node_idx, prev_node_idx, 0);
                    }
                    else {
                        eprintln!("Client {} error: previous node id {} not found in node_id_to_index map while processing path trace", self.id, prev_node_id);
                    }
                }
            }
        }
        //raccogli tutte FloodResponse (per flood_id), dopo finalizza topologia per quel ciclo
    }

    fn best_path(&self, source: NodeId, target: NodeId) -> Option<Vec<NodeId>> {
        let &source_idx = self.node_id_to_index.get(&source)?;
        let &target_idx = self.node_id_to_index.get(&target)?;
        let predecessors = dijkstra(&self.network_graph, source_idx, Some(target_idx), |e| *e.weight());
        let mut predecessors_map: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        let path_result = petgraph::algo::astar(
            &self.network_graph,
            source_idx,
            |n| n == target_idx, // Goal condition: Is node n the target node?
            |e| *e.weight(),     // Edge cost: Use the weight of the edge (usize)
            |_| 0                // Heuristic: Use 0 for all nodes (makes A* equivalent to Dijkstra)
        );
        match path_result {
            Some((cost, node_indices_path)) => {
                println!("Client {} found path from {} to {} with cost {}", self.id, source, target, cost);
                let node_ids_path : Vec<NodeId> = node_indices_path.iter().filter_map(|&idx| self.network_graph.node_weight(idx).map(|node_info| node_info.id)).collect();
                Some(node_ids_path)
            }
            None => {
                println!("Client {} could not find a path from {} to {}", self.id, source, target);
                None
            }
        }
    }

    fn increment_drop(&mut self, from: NodeId, to: NodeId) {
        if let (Some(&from_idx), Some(&to_idx)) = (&self.node_id_to_index.get(&from), &self.node_id_to_index.get(&to)) {
            if let Some(edge_index) = self.network_graph.find_edge(from_idx, to_idx) {
                if let Some(weight) = self.network_graph.edge_weight_mut(edge_index) {
                    *weight = weight.saturating_add(1); //incrementa evitando overflow
                    println!("Client {} incremented drop cost on link {} -> {} to {}", self.id, from, to, *weight);
                }
            } else {
                println!("Client {} tried to increment drop on non-existent edge {} -> {}", self.id, from, to);
            }
        } else {
            println!("Client {} tried to increment drop, but nodes {} or {} not found in graph.", self.id, from, to);
        }
    }

    fn remove_node_from_graph(&mut self, node_id : NodeId) {
        if let Some(node_index) = self.node_id_to_index.remove(&node_id) {
            self.network_graph.remove_node(node_index);
            println!("Client {} removed node {} (index {:?}) from graph.", self.id, node_id, node_index);
        } else {
            println!("Client {} tried to remove non-existent node {} from graph.", self.id, node_id);
        }
    }

    #[allow(dead_code)] //non usa la funzione per ora
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
                //implementa:cosa fare se non si trova una rotta
                //1.avviare flood discovery
                //2.segnalare errore alla GUI
                return;
            }
        };

        let routing_header = SourceRoutingHeader {
            hops : route.clone(),
            hop_index : 1,
        };

        //- - - - creazione messaggio alto livello - - - -
        let high_level_message_content = format!("[message_for]::{client_id}::{message}", client_id=target_client_id, message=message_text); //DA SISTEMARE, FATTO DA IA
        let message_data_bytes = high_level_message_content.into_bytes();

        //- - - - message fragmentation - - - -
        const FRAGMENT_SIZE: usize = 128;
        let total_fragments = (message_data_bytes.len() + FRAGMENT_SIZE - 1) / FRAGMENT_SIZE;
        let mut fragments : Vec<Fragment> = Vec::new();
        let session_id = rand::random::<u64>();
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
            fragments : fragments.clone(),
            original_routing_header : routing_header.clone(),
        });

        //- - - - invio messaggi frammentati - - - -
        if routing_header.hops.len() > routing_header.hop_index {
            let first_hop = routing_header.hops[routing_header.hop_index];
            for fragment in fragments {
                let packet = Packet {
                    pack_type : PacketType::MsgFragment(fragment.clone()),
                    routing_header : routing_header.clone(),
                    session_id,
                };
                match self.send_to_neighbor(first_hop, packet) {
                    Ok(()) => println!("Client {} sent fragment {} for session {} to {}", self.id, fragment.fragment_index, session_id, first_hop),
                    Err(e) => eprintln!("Client {} Failed to send fragment {} for session {} to {}: {}", self.id, fragment.fragment_index, session_id, first_hop, e),
                }
            }
        } else {
            eprintln!("Client {} has no valid first hop in computed route {:?} to send the message to!", self.id, route);
            //implementa avvio flood discovery se non si conosce vicini/rotta non può essere calcolata
        }
    }

    #[allow(dead_code)] //non usa la funzione per ora
    fn get_node_info(&self, node_id : NodeId) -> Option<&NodeInfo> {
        self.node_id_to_index.get(&node_id).and_then(|&idx| self.network_graph.node_weight(idx))
    }

    fn send_to_neighbor (&mut self, neighbor_id : NodeId, packet : Packet) -> Result<(), String> {
        if let Some(sender) = self.packet_send.get(&neighbor_id) {
            match sender.send(packet) {
                Ok(()) => Ok(()),
                Err(e) => Err(format!("Failed to send packet to neighbor {}: {}", neighbor_id, e)),
            }
        } else {
            Err(format!("Neighbor {} not found in packet_send map.", neighbor_id))
        }
    }


    //chiarire la questione degli ack

    //implemanta funzioni per logica alto livello (login, logout, messageto, messagefrom, clientlistrequest, clientlistresponse(Vec<NodeId>), chatrequest(NodeId), chatstart(bool), charfinish)
    //le funzioni devono:
    //1.calcolare rotta verso server (Dijkstra)
    //2.crea header di source routing con rotta calcolata
    //3.frammentare messaggio di alto livello in MsgFragment
    //4.inviare frammenti al primo hop della rotta usando la funzione send_to_neighbor
    //5.memorizzare infos del messaggio in self.sent_messages (per i nack)
    
    //implementa gestione comandi simulation controller (addsender, removesender)
    

}
