use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use crossbeam_channel::{Sender, Receiver};
use std::thread;
use rand::Rng;
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Packet, PacketType, FloodRequest, FloodResponse, Fragment};
use wg_2024::packet::NodeType;
use crate::simulation_controller::gui_input_queue::{pop_all_gui_messages, SharedGuiInput};


pub fn start_client(
    client_id: NodeId,
    packet_rx: Receiver<Packet>,
    packet_senders: HashMap<NodeId, Sender<Packet>>,
    gui_input: SharedGuiInput,
) {
    thread::spawn(move || {
        println!("Temp Client {} started", client_id);
        println!("🧠 Arc ptr of client: {:p}", Arc::as_ptr(&gui_input));

        /*
                // Flooding (unchanged)
                let flood_id = rand::thread_rng().gen::<u64>();
                let flood_request = Packet {
                    session_id: flood_id,
                    routing_header: Default::default(),
                    pack_type: PacketType::FloodRequest(FloodRequest {
                        flood_id,
                        initiator_id: client_id,
                        path_trace: vec![(client_id, NodeType::Client)],
                    }),
                };

                for (_, sender) in &packet_senders {
                    let _ = sender.send(flood_request.clone());
                }

                let mut responses = HashSet::new();
                let timeout = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while std::time::Instant::now() < timeout {
                    if let Ok(packet) = packet_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                        if let PacketType::FloodResponse(resp) = packet.pack_type {
                            if responses.insert(resp.flood_id) {
                                println!("Client {} got response: {:?}", client_id, resp.path_trace);
                            }
                        }
                    }
                }

                println!("Client {} finished flood test", client_id);
        */
        // 🆕 Poll and send one message every second
        loop {
            if let Ok(mut map) = gui_input.lock() {
                if let Some(msgs) = map.get_mut(&client_id) {
                    if !msgs.is_empty() {
                        let msg = msgs.remove(0); // only ONE message per loop
                        drop(map); // release lock early

                        // Send to all servers
                        for (&server_id, sender) in &packet_senders {
                            let raw_bytes = msg.as_bytes();
                            let max_len = 128;
                            let total_frags = ((raw_bytes.len() + max_len - 1) / max_len) as u64;
                            let session_id = rand::random();

                            for (i, chunk) in raw_bytes.chunks(max_len).enumerate() {
                                let mut buf = [0u8; 128];
                                buf[..chunk.len()].copy_from_slice(chunk);

                                let fragment = Fragment {
                                    fragment_index: i as u64,
                                    total_n_fragments: total_frags,
                                    length: chunk.len() as u8,
                                    data: buf,
                                };

                                let packet = Packet {
                                    session_id,
                                    routing_header: SourceRoutingHeader::with_first_hop(vec![client_id, server_id]),
                                    pack_type: PacketType::MsgFragment(fragment),
                                };

                                let _ = sender.send(packet);
                            }
                        }
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_secs(1));
        }

    });
}

/*

let messages = pop_all_gui_messages(&self.gui_input, self.id);
for msg in messages {
    let fragment = make_fragment(&msg); // crea i fragment
    let packet = Packet {
        session_id: 0,
        routing_header: SourceRoutingHeader {
            hop_index: 0,
            hops: vec![self.id, server_id], // dinamico se necessario
        },
        pack_type: PacketType::MsgFragment(fragment),
    };
    if let Some(sender) = self.packet_send.get(&server_id) {
        let _ = sender.send(packet);
    }
}
*/

/*
use std::collections::{HashMap, HashSet};
use std::io;
use std::io::Write;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;
use crossbeam_channel::{select, Receiver, Sender};
use petgraph::graph::{Graph, NodeIndex, UnGraph};
use petgraph::algo::dijkstra;
use petgraph::Undirected;

#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_recv: Receiver<DroneCommand>,
    pub sent_messages: HashMap<u64, Vec<Fragment>>,
}

impl MyClient{
    fn new(id: NodeId, sim_contr_recv: Receiver<DroneCommand>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, sent_messages:HashMap<u64 , Vec<Fragment>>) -> Self {
        Self {
            id,
            sim_contr_recv,
            packet_recv,
            packet_send,
            sent_messages,
        }
    }

    fn run (&mut self){
        let mut seen_flood_ids = HashSet::new();
        let mut received_messages = HashMap::new();
        let mut graph = UnGraph::<i32, f32>::new_undirected();
        let mut node_map: HashMap<u8, NodeIndex> = HashMap::new();
        //aggiungere anche quello dei pacchetti mandati volendo oppure mettere il pacchetto che si sta mandando in una variabile e impedire di mandare altro fino a che non ha finito
        loop{
            select! {
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet...");
                    if let Ok(packet) = packet {
                        println!("Packet received by drone {} : {:?}",self.id, packet);
                        let mut message = vec![];
                        self.process_packet(packet, &mut seen_flood_ids, &mut received_messages, message);
                    } else {
                        println!("No packet received or channel closed.");
                    }
                },
            }
        }
    }

    fn process_packet (&mut self, mut packet: Packet, seen_flood_ids: &mut HashSet<u64>, received_packets: &mut HashMap<u64 , Vec<u8>>, message: Vec<MsgFragment()>) {

        match &packet.pack_type {
            PacketType::FloodRequest(request) => {
                self.process_flood_request(request, seen_flood_ids , packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                self.create_topology(response);
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut packet , fragment);
                self.reassemble_packet(fragment , received_packets, &mut packet);
            },
            PacketType::Ack(ack) => {
                //ok.
            },
            PacketType::Nack(nack ) => {
                self.process_nack(nack , &mut packet);
            }
        }
    }

    /*
    i messaggi che vengono mandati vengono divisi in un vettore di frammenti associato al suo id e conservati in un hashmap
    quando recuperiamo il messaggio cerchiamo l'id e troviamo il fragment index
     */
    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        match nack.nack_type {
            NackType::Dropped=>{
                if let Some(fragments) = self.sent_messages.get(&packet.session_id){
                    for  fragment in fragments{
                        if fragment.fragment_index == nack.fragment_index{
                            self.send(fragment);
                        }
                    }
                }
            },
            _ => {
                let new_hops :Vec<NodeId> = Vec::new(); // da fare con djikstra
                packet.routing_header.hops = new_hops;
                packet.routing_header.hop_index = 0;
                self.send(packet);
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
        let bytes = input.trim_end().as_bytes().to_vec();
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
                routing_header: Default::default(), //Djikstra
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

    fn reassemble_packet (&mut self, fragment: &Fragment, received_packets: &mut HashMap<u64 , Vec<u8>>, packet : &mut Packet){
        let session_id = packet.session_id;
        if let Some(mut content)= received_packets.get(&session_id){
            content.len()+= fragment.data.len();
            for i in 1..fragment.data.len() {
                content.insert( (fragment.fragment_index*127+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*127{
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
            }
            received_packets.insert(session_id ,content.clone());
        }

    }

    fn process_flood_request(&mut self, request: &FloodRequest, seen_flood_ids: &mut HashSet<u64>, header: SourceRoutingHeader){
        let mut updated_request = request.clone();
        if (seen_flood_ids.contains(&request.flood_id) || self.packet_send.len() == 1){
            updated_request.path_trace.push((self.id , NodeType::Client));
            //send flood response
        }
        else {
            seen_flood_ids.insert(request.flood_id);
            updated_request.path_trace.push((self.id, NodeType::Client));
            let sender_id = if updated_request.path_trace.len() > 1 {
                Some(updated_request.path_trace[updated_request.path_trace.len() - 2].0)
            } else {
                None
            };

            for (neighbor_id, sender) in self.packet_send.iter() {
                if Some(*neighbor_id) != sender_id{
                    updated_request.path_trace.push((*neighbor_id, NodeType::Drone));
                    let updated_path_trace = updated_request.path_trace.iter().map(|(id, )| *id).collect::<Vec<>>();
                    updated_request.path_trace.pop();
                    let packet = Packet{
                        pack_type: PacketType::FloodRequest(FloodRequest{
                            flood_id: updated_request.flood_id,
                            initiator_id:updated_request.path_trace[0].0.clone(),
                            path_trace: updated_request.path_trace.clone(),
                        }),
                        routing_header: header.clone(), // to fix
                        session_id:0,
                    };
                    if let Err(err) = sender.send(packet) {
                        println!("Error sending packet: {:?}", err);
                    }
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
    fn add_node_no_duplicate<N: std::cmp::Eq + std::hash::Hash + Copy, E>(
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
    fn process_flood_response (&mut self, response: &FloodResponse, mut graph: &mut Graph<u8, u8, Undirected>, mut node_map: HashMap<(u8 , NodeType), NodeIndex>) {
        for i in 0 .. response.path_trace.len()-1{
            let node1 = Self::add_node_no_duplicate(&mut graph, &mut node_map, response.path_trace[i].clone());
            let node2 = Self::add_node_no_duplicate(&mut graph, &mut node_map, response.path_trace[i+1].clone());
            Self::add_edge_no_duplicate(graph , node1 , node2 , 1.0);
        }
    }
}
 */