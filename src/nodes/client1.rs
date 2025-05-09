use std::collections::{HashMap, HashSet};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;
//use crate::droneK::drone::MyDrone;
use crossbeam_channel::{select, Receiver, Sender};
use std::{fs, thread};




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

#[derive(Debug,Clone)]
pub struct MyClient{
    pub id: NodeId,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_recv: Receiver<DroneCommand>,
    pub sent_messages: HashMap<u64, SentMessageInfo>,
    pub received_messages : HashMap<u64, ReceivedMessageState>, //per determinare quando il messaggio è completo (ha tutti i frammenti)
}

impl MyClient{
    fn new(id: NodeId, sim_contr_recv: Receiver<DroneCommand>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, sent_messages:HashMap<u64 , SentMessageInfo>) -> Self {
        Self {
            id,
            sim_contr_recv,
            packet_recv,
            packet_send,
            sent_messages,
            received_messages : HashMap::new(),
        }
    }

    fn run (&mut self){
        let mut seen_flood_ids = HashSet::new();
        let mut received_messages = HashMap::new();
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

    fn process_packet (&mut self, mut packet: Packet, seen_flood_ids: &mut HashSet<u64>, received_packets: &mut HashMap<u64 , ReceivedMessageState>, message: Vec<Fragment>) {

        let packet_type = packet.pack_type.clone();
        match packet_type {
            PacketType::FloodRequest(request) => {
                self.process_flood_request(&request, seen_flood_ids , packet.routing_header.clone());
            },
            PacketType::FloodResponse(response) => {
                self.create_topology(&response);
            },
            PacketType::MsgFragment(fragment) => {
                self.send_ack(&mut packet , &fragment);
                self.reassemble_packet(&fragment , received_packets, &mut packet);
            },
            PacketType::Ack(ack) => {
                //ok.
            },
            PacketType::Nack(nack ) => {
                self.process_nack(&nack , &mut packet);
            }
        }
    }

    /*
    i messaggi che vengono mandati vengono divisi in un vettore di frammenti associato al suo id e conservati in un hashmap
    quando recuperiamo il messaggio cerchiamo l'id e troviamo il fragment index
     */

    fn send_to_neighbor (&mut self, neighbor_id : NodeId, packet : Packet) -> Result<(), String> {
        if let Some(sender) = self.packet_send.get(&neighbor_id) {
            match sender.send(packet) {
                Ok(()) => {
                    Ok(())
                }
                Err(e) => {
                    Err(format!("Failed to send packet to neighbor {}: {}", neighbor_id, e))
                }
            }
        } else {
            Err(format!("Neighbor {} not found", neighbor_id))
        }
    }
    fn process_nack(&mut self, nack: &Nack, packet: &mut Packet) {
        match nack.nack_type {
            NackType::Dropped=>{
                if let Some(original_packet_info) = self.sent_messages.get(&packet.session_id) { //original_packet_info dovrebbe contenere i frammenti E l'header di routing originale
                    if let Some(fragment_to_resend) = original_packet_info.fragments.iter().find(|f| f.fragment_index == nack.fragment_index) {
                        if let Some(neighbor_to_resend_to) = packet.routing_header.hops.get(packet.routing_header.hop_index) {
                            let packet_to_resend = Packet {
                                pack_type : PacketType::MsgFragment(fragment_to_resend.clone()),
                                routing_header : original_packet_info.original_routing_header.clone(),
                                session_id : packet.session_id,
                            };
                            match self.send_to_neighbor(*neighbor_to_resend_to, packet_to_resend) {
                                Ok(()) => println!("Resent dropped fragment {} for session {}", nack.fragment_index, packet.session_id),
                                Err(e) => eprintln!("Failed to resend dropped fragment {} for session {}: {}", nack.fragment_index, packet.session_id, e)
                            }
                        } else {
                            eprintln!("Received dropped nack for session {} fragment {}, but nack routing header is invalid (no hop index {}). Cannot resend", packet.session_id, nack.fragment_index, packet.routing_header.hop_index);
                        }
                    } else {
                        eprintln!("Received dropped nack for session {}, but original packet info not found in sent_messages", packet.session_id);
                    }
                } else {
                    eprintln!("Received dropped nack for session {}, but original packet info not found in sent_messages", packet.session_id);
                }
            },
            _ => {
                eprintln!("Received nack of type {:?} for session {}", nack.nack_type, packet.session_id);
                match nack.nack_type {
                    NackType::ErrorInRouting(problem_node_id) => {
                        eprintln!("Routing error reported for session {}: node {} was not a neighbor of the drone at the problematic hop", packet.session_id, problem_node_id);
                        //implementa logica per informare il modulo routing che il nodo "problem_node_id" o il link verso di esso sono inaffidabili
                        //possibile dover ricalcolare la rotta
                    }
                    NackType::DestinationIsDrone => {
                        eprintln!("Routing error reported for session {}: packet reached a drone, but the route indicates a client/server destination", packet.session_id);
                        //implemeta logica per segnalare che la rotta calcolata potrebbe essere errata
                    }
                    NackType::UnexpectedRecipient(received_at_node_id) => {
                        eprintln!("Routing error reported for session {}: packet intended for a different node arrived at drone {}. Route mismatch?", packet.session_id, received_at_node_id);
                        //implementa logica pe segnalare disallineamento della rotta o errore di topologia
                    }
                    NackType::Dropped => {
                        eprintln!("Received unexpected dropped nack in generic handler for session {}", packet.session_id);
                        if let Some(original_packet_info) = self.sent_messages.get(&packet.session_id) { //original_packet_info dovrebbe contenere i frammenti E l'header di routing originale
                            if let Some(fragment_to_resend) = original_packet_info.fragments.iter().find(|f| f.fragment_index == nack.fragment_index) {
                                if let Some(neighbor_to_resend_to) = packet.routing_header.hops.get(packet.routing_header.hop_index) {
                                    let packet_to_resend = Packet {
                                        pack_type : PacketType::MsgFragment(fragment_to_resend.clone()),
                                        routing_header : original_packet_info.original_routing_header.clone(),
                                        session_id : packet.session_id,
                                    };
                                    match self.send_to_neighbor(*neighbor_to_resend_to, packet_to_resend) {
                                        Ok(()) => println!("Resent dropped fragment {} for session {}", nack.fragment_index, packet.session_id),
                                        Err(e) => eprintln!("Failed to resend dropped fragment {} for session {}: {}", nack.fragment_index, packet.session_id, e)
                                    }
                                } else {
                                    eprintln!("Received dropped nack for session {} fragment {}, but nack routing header is invalid (no hop index {}). Cannot resend", packet.session_id, nack.fragment_index, packet.routing_header.hop_index);
                                }
                            } else {
                                eprintln!("Received dropped nack for session {}, but original packet info not found in sent_messages", packet.session_id);
                            }
                        }
                    }
                }
            }
        }
    }

    fn create_topology(&mut self, flood_response: &FloodResponse){}
    
    //chiarire la questione degli ack
    fn reassemble_packet (&mut self, fragment: &Fragment, received_packets: &mut HashMap<u64 , ReceivedMessageState>, packet : &mut Packet){
        let session_id = packet.session_id;
        const FRAGMENT_DATA_SIZE : usize = 128;
        let state = received_packets.entry(session_id).or_insert_with(|| {
            let total_size = fragment.total_n_fragments as usize * FRAGMENT_DATA_SIZE;
            ReceivedMessageState {
                data : vec![0; total_size],
                total_fragments : fragment.total_n_fragments,
                received_indeces : HashSet::new(),
            }
        });
        if state.received_indeces.contains(&fragment.fragment_index) {
            println!("Received duplicate fragment {} for session {}. Ignoring", fragment.fragment_index, session_id);
            return;
        }
        let offset = fragment.fragment_index as usize * FRAGMENT_DATA_SIZE;
        let fragment_len = fragment.length as usize;
        let fragment_data_slice = if fragment_len <= fragment.data.len() {
            &fragment.data[..fragment_len]
        } else {
            eprintln!("Received fragment {} for session {} with invalid declared length {} >>> actual data size {}", fragment.fragment_index, session_id, fragment_len, fragment.data.len());
            return;
        };

        if offset + fragment_len <= state.data.len() {
            let target_slice = &mut state.data[offset..offset + fragment_len];
            target_slice.copy_from_slice(fragment_data_slice);

            //verifica completamento e invio ACK
            state.received_indeces.insert(fragment.fragment_index);
            if state.received_indeces.len() == state.total_fragments as usize {
                println!("Message for sesssion {} reassembled successfully", session_id);
                //implementa logica per passare messaggio completo
                //invia ACK
                let original_hops = packet.routing_header.hops.clone();
                let reversed_hops : Vec<NodeId> = original_hops.iter().rev().copied().collect();
                //implementa funzione send_final_ack(session_id, reversed_hops)
                self.received_messages.remove(&session_id);
            }
        } else {
            eprintln!("Received fragment {} for session {} with offset {} and length {} which exceeds excepted message size {}. Ignoring fragment", fragment.fragment_index, session_id, offset, fragment_len, state.data.len());
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
                    let updated_path_trace = updated_request.path_trace.iter().map(|(id,_)| *id).collect::<Vec<NodeId>>();
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
}
