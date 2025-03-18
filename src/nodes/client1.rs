use std::collections::{HashMap, HashSet};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;
use crate::droneK::drone::MyDrone;
use crossbeam_channel::{select, Receiver, Sender};
use std::{fs, thread};




/*

**********AGGIUNGERE LE FUNZIONI PER MANDARE I PACCHETTI***********


 */
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
                let mut new_hops :Vec<NodeId> = Vec::new(); // da prendere dall'utente
                packet.routing_header.hops = new_hops;
                packet.routing_header.hop_index = 0;
                self.send(packet);
            }
        }
    }

    fn create_topology(&mut self, flood_response: &FloodResponse){}


    //chiarire la questione degli ack
    fn reassemble_packet (&mut self, fragment: &Fragment, received_packets: &mut HashMap<u64 , Vec<u8>>, packet : &mut Packet){
        let session_id = packet.session_id;
        if let Some(mut content)= received_packets.get(&session_id){
            content.len()+= fragment.data.len();
            for i in 0..fragment.data.len()-1 {
                content.insert( (fragment.fragment_index*128+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*128{
                self.send_ack(packet, fragment);
            }
        }
        else {
            let mut content = vec![];
            content.len()+= fragment.data.len();
            for i in 0..fragment.data.len()-1 {
                content.insert( (fragment.fragment_index*128+(i as u64) ) as usize , fragment.data[i]);
            }
            if content.len() > ((fragment.total_n_fragments - 1) as usize)*128{
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
}
