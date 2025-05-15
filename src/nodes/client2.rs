use std::collections::{HashMap, HashSet};
use std::io;
use std::io::Write;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;
use crossbeam_channel::{select, Receiver, Sender};
use egui::CursorIcon::Default;
use log::warn;
use petgraph::graph::{Graph, NodeIndex, UnGraph};
use petgraph::algo::dijkstra;
use petgraph::data::Build;
use petgraph::prelude::EdgeRef;
use petgraph::Undirected;
use wg_2024::packet::NodeType::{Client, Server};



/*
rendere il codice più leggibile: aggiungere commenti e mettere in ordine
mettere le funzioni già implementate nel wgl dove possibile
*/











#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_recv: Receiver<DroneCommand>,
    pub sent_messages: HashMap<u64, Vec<Fragment>>,
    pub net_graph: Graph<u8, u8, Undirected>,
    pub node_map: HashMap<(NodeId , NodeType), NodeIndex>,
    pub received_packets: HashMap<u64 , Vec<u8>>,
    pub seen_flood_ids : HashSet<u64>,
    pub received_messages : HashMap<u64 , Vec<u8>>,
}

impl MyClient{
    fn new(id: NodeId, sim_contr_recv: Receiver<DroneCommand>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, sent_messages:HashMap<u64 , Vec<Fragment>>, net_graph: Graph<u8, u8, Undirected>, node_map: HashMap<(u8 , NodeType), NodeIndex>, received_packets: HashMap<u64 , Vec<u8>>, seen_flood_ids : HashSet<u64>, received_messages : HashMap<u64 , Vec<u8>>) -> Self {
        Self {
            id,
            sim_contr_recv,
            packet_recv,
            packet_send,
            sent_messages,
            net_graph,
            node_map,
            received_packets,
            seen_flood_ids,
            received_messages,
        }
    }

    fn run (&mut self){
        loop{
            select! {
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet...");
                    if let Ok(packet) = packet {
                        println!("Packet received by drone {} : {:?}",self.id, packet);
                        self.process_packet(packet);
                    } else {
                        println!("No packet received or channel closed.");
                    }
                },
            }
        }
    }


    fn process_packet (&mut self, mut packet: Packet) {

        match &packet.pack_type {
            PacketType::FloodRequest(request) => {
                self.process_flood_request(request, packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                self.create_topology(response);
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut packet , fragment);
                self.reassemble_packet(fragment , &mut packet);
            },
            PacketType::Ack(_ack) => {
                //ok.
            },
            PacketType::Nack(nack ) => {
                self.process_nack(nack , &mut packet);
            }
        }
    }

    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        match nack.nack_type {
            NackType::Dropped=>{
                Self::increase_cost(self ,packet.routing_header.hops[0]);
                if let Some(fragments) = self.sent_messages.get(&packet.session_id){
                    for  fragment in fragments{
                        if fragment.fragment_index == nack.fragment_index{
                            self.send(fragment);
                        }
                    }
                }
            },
            NackType::ErrorInRouting(NodeId)=>{
                self.node_map.remove(NodeId);
                Self::remove_all_edges_with_node(self , NodeId);
                self.send_flood_request();
            }
            _ => {
                self.send_flood_request();
            }
        }
    }

    fn send_packet(&self){
        print!("Enter message type: ");
        io::stdout().flush().unwrap();
        let mut message_type = String::new();
        io::stdin().read_line(&mut message_type).expect("Failed to read line");
        let mut input = String::new();
        io::stdin().read_line(&mut input).expect("Failed to read line");
        let bytes = input.trim_end();
        let chunks: Vec<Vec<u8>> = bytes.chunks(127).map(|chunk| chunk.to_vec()).collect();
        for i in 0..chunks.len()-1 {
            let mut fragment:Fragment = Fragment {
                fragment_index: i as u64,
                total_n_fragments: chunks.len() as u64,
                length: chunks[i].len() as u8,
                data,
            };
            fragment.data[1 .. 127] = *chunks[i];
            fragment.data[0] = message_type.as_bytes()[0];
            let  packet = Packet{
                routing_header: SourceRoutingHeader{
                    hop_index : 0,
                    hops : self.best_path(self.id , self.get_server_id()).unwrap(),
                },
                session_id: 0,
                pack_type: MsgFragment(fragment),
            };
            if let Some(next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index){
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

    }

    fn send_flood_request(&mut self){
        let flood_request  = FloodRequest :: initialize(100 , self.id , Client);
        let packet = Packet {
            pack_type: PacketType::FloodRequest(flood_request),
            session_id: 0,
            routing_header: SourceRoutingHeader{
                hop_index: 0,
                hops: [self.id].to_vec(),
            }
        };
        for  sender in self.packet_send.iter() {
            sender.send(packet.clone()).unwrap()
        }

    }

    fn reassemble_packet (&mut self, fragment: &Fragment, packet : &mut Packet){
        let session_id = packet.session_id;
        if let Some(mut content)= self.received_packets.get(&session_id){
            content.len()+= fragment.data.len();
            for i in 0..fragment.data.len() {
                content.insert( (fragment.fragment_index*128+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*128{
                self.send_ack(packet, fragment);
            }
        }
        else {
            let mut content = vec![];
            content.len()+= fragment.data.len();
            for i in 1..fragment.data.len() {
                content.insert( (fragment.fragment_index*127+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*127{
                self.send_ack(packet, fragment);
                self.packet_command_handling(content);
            }
            self.received_packets.insert(session_id ,content.clone());
        }

    }

    fn packet_command_handling(&mut self, message : Vec<u8>) {
        let message_string = String::from_utf8_lossy(&message).to_string();
        let tokens: Vec<&str> = message_string.trim().splitn(3, "::").collect();
        match tokens.as_slice() {
            ["[MessageFrom]", _client_id, _msg]=>{}
            ["[ChatStart]", _success]=>{}
            ["[ClientListResponse]", _clients]=>{}
            ["[HistoryResponse]", _response]=>{}
            _=>{
                warn!("Wrong message format. The message: {} , doesn't respect any known format", message_string);
            }
        }
    }


fn process_flood_request(&mut self, request: &FloodRequest, header: SourceRoutingHeader){
        let mut updated_request = request.clone();
        if self.seen_flood_ids.contains(&request.flood_id) || self.packet_send.len() == 1{
            updated_request.path_trace.push((self.id ,Client));
            //send flood response
        }
        else {
            self.seen_flood_ids.insert(request.flood_id);
            updated_request.path_trace.push((self.id, Client));
            let sender_id = if updated_request.path_trace.len() > 1 {
                Some(updated_request.path_trace[updated_request.path_trace.len() - 2].0)
            } else {
                None
            };
//generating FloodResponse on the first passing of the flood
            let response_packet = Packet{
                pack_type : PacketType :: FloodResponse(FloodResponse{
                    flood_id : updated_request.flood_id,
                    path_trace : updated_request.path_trace.clone(),
                }),
                routing_header: header.clone(),
                session_id: 0,
            };
            let response_sender = self.packet_send.get(&updated_request.initiator_id);
            response_sender.send(response_packet).unwrap();
//keeping the flood going
            for (neighbor_id, sender) in self.packet_send.iter() {
                let packet = Packet{
                    pack_type: PacketType::FloodRequest(FloodRequest{
                        flood_id: updated_request.flood_id,
                        initiator_id:updated_request.path_trace[0].0.clone(),
                        path_trace: updated_request.path_trace.clone(),
                    }),
                    routing_header: header.clone(), // to fix
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
        for i in 0 .. response.path_trace.len()-1{
            let node1 = Self::add_node_no_duplicate(&mut self.net_graph, &mut self.node_map, response.path_trace[i].clone());
            let node2 = Self::add_node_no_duplicate(&mut self.net_graph, &mut self.node_map, response.path_trace[i+1].clone());
            Self::add_edge_no_duplicate(&mut self.net_graph , node1 , node2 , 1.0);
        }
    }
    fn add_edge_no_duplicate<N, E>(
        graph: &mut UnGraph<N, E>,
        a: NodeIndex,
        b: NodeIndex,
        weight: f64
    ) -> bool {
        if !graph.contains_edge(a, b) {
            graph.add_edge(a, b, weight);
            true
        } else {
            false
        }
    }
    fn add_node_no_duplicate<N: Eq + std::hash::Hash + Copy, E>(
        graph: &mut UnGraph<N, E>,
        node_map: &mut HashMap<(u8 , NodeType), NodeIndex>,
        value: (u8 , NodeType)
    ) -> NodeIndex {
        if let Some(&idx) = node_map.get(&value) {
            // Node with this value already exists
            idx
        } else {
            // Create new node
            let idx = graph.add_node(value);
            node_map.insert(value, idx);
            idx
        }
    }
    fn remove_all_edges_with_node(&mut self, crash_id: NodeId) {
        let index = self.node_map.get(&(crash_id, NodeType::Drone)).unwrap();
        let mut edges_to_remove =  Vec::new();
        edges_to_remove = self.net_graph.edges(*index).filter_map(|edge_ref| {
            // Get the source and target of the edge
            let source = edge_ref.source();
            let target = edge_ref.target();

            // If either endpoint matches our target node, keep this edge
            if source == *index || target == *index {
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
        let index = self.node_map.get(&(dropper_id, NodeType::Drone)).unwrap();
        let mut edges_to_increase =  Vec::new();
        edges_to_increase = self.net_graph.edges(*index).filter_map(|edge_ref| {
            // Get the source and target of the edge
            let source = edge_ref.source();
            let target = edge_ref.target();

            // If either endpoint matches our target node, keep this edge
            if source == *index || target == *index {
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
        let source_idx = *self.node_map.get(&(source,Client))?;
        let target_idx = *self.node_map.get(&(target,Server))?;

        // Dijkstra: restituisco anche da dove vengo (predecessore)
        let mut predecessors: HashMap<NodeIndex, NodeIndex> = HashMap::new();
        let _ = dijkstra(&self.net_graph, source_idx, Some(target_idx), |e| {
            let from = e.source();
            let to = e.target();
            // Se trovo un nuovo nodo raggiungibile, registro da chi arrivo
            predecessors.entry(to).or_insert(from);
            *e.weight()
        });

        // Se non ho trovato un cammino fino a target, esco
        if !predecessors.contains_key(&target_idx) {
            return None;
        }

        // Ora ricostruisco il path usando i predecessori
        let mut path = vec![self.net_graph[target_idx]];
        let mut current = target_idx;
        while current != source_idx {
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
        while let Some (ids) = self.node_map.keys().next(){
            if *ids.1 == Server {
                return ids.0;
            }
        }
    }
}
