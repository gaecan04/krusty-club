use std::collections::HashSet;
use std::collections::HashMap;
use std::process::id;
use crossbeam_channel::{select, select_biased, Receiver, Sender};
use rand::Rng;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Nack, NackType, NodeType, Packet, PacketType};
use wg_2024::packet::PacketType::MsgFragment;
use wg_2024::drone::Drone;


#[derive(Debug, Clone)]
pub struct MyDrone {
    pub id: NodeId,
    pub pdr: f32,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_send: Sender<DroneEvent>, // Sends events to Simulation Controller
    pub sim_contr_recv: Receiver<DroneCommand>, // Receives commands from Simulation Controller
}

impl Drone for MyDrone {
    fn new(id: NodeId, sim_contr_send: Sender<DroneEvent>, sim_contr_recv: Receiver<DroneCommand>, packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, pdr: f32) -> Self {
        Self {
            id,
            sim_contr_send,
            sim_contr_recv,
            packet_recv,
            packet_send,
            pdr,
        }
    }

    fn run(&mut self) {
        let mut seen_flood_ids: HashSet<u64> = HashSet::new(); // Track seen flood IDs locally
        loop {
            select_biased! {
                recv(self.sim_contr_recv) -> command => {
                    if let Ok(command) = command {
                        println!("command received");
                        self.handle_command(command);
                    }else{
                        println!("Simulation Controller channel closed. Exiting loop.");
                        break;
                    }
                }
                recv(self.packet_recv) -> packet => {
                    println!("Checking for received packet...");
                    if let Ok(packet) = packet {
                        println!("Packet received by drone: {:?}", packet);
                        self.handle_packet(packet, &mut seen_flood_ids);
                    } else {
                        println!("No packet received or channel closed.");
                    }
                 },
            }
        }
    }


}
impl MyDrone {
    fn handle_packet(&mut self, mut packet: Packet, seen_flood_ids: &mut HashSet<u64>) {
        //1 if yes
        if packet.routing_header.hops[packet.routing_header.hop_index] == self.id {
            //2
            packet.routing_header.hop_index += 1;
            //3
            if packet.routing_header.hop_index == packet.routing_header.hops.len() {
                //if yes
                self.send_nack(packet, NackType::DestinationIsDrone);
            } else {
                //4
                let next_hop = packet.routing_header.hops[packet.routing_header.hop_index].clone();
                if let Some(sender) = self.packet_send.get(&next_hop).cloned() {
                    //5

                    self.process_packet(packet, seen_flood_ids, &sender);
                } else {
                    //if not neighbor
                    self.send_nack(packet, NackType::ErrorInRouting(next_hop));
                }
            }
        //1 if no
        } else {
            self.send_nack(packet, NackType::UnexpectedRecipient(self.id));
        }
    }

    //related to step5
    fn process_packet(&mut self, mut packet: Packet, seen_flood_ids: &mut HashSet<u64>, sender: &Sender<Packet>) {
        match packet.pack_type {
            PacketType::FloodRequest(request) => {
                println!("Flood request: {:?}", packet.session_id);
                //packet.routing_header.hop_index+=1;
                self.process_flood_request(request, packet.routing_header.clone(), seen_flood_ids);
            },
            PacketType::FloodResponse(response) => {
                self.forward_back_response(response); //modify the trace, pop first element
            },

            PacketType::MsgFragment(ref fragment) => {

                if self.should_drop_packet() {
                    //send to sim a NodeEvent:: Dropped
                    self.sim_contr_send
                        .send(DroneEvent::PacketDropped(packet.clone()))
                        .unwrap_or_else(|err| eprintln!("Packet has been dropped but sending to sim_controller failed... : {}", err));

                    //manipulate test cases inside send_nack
                    self.send_nack(packet.clone(), NackType::Dropped);
                } else {

                    sender.send(packet.clone()).unwrap_or_else(|err| {
                        eprintln!("Failed to forward packet: {}", err);
                    });
                    //self.send_ack(packet.clone()); //each time a msg fragment is sent, send also an ack backward.
                    self.sim_contr_send
                        .send(DroneEvent::PacketSent(packet.clone()))
                        .unwrap_or_else(|err| eprintln!("Packet has been sent correctly, sending to sim_controller now... : {}", err));

                }
            },

            PacketType::Ack(ack) => {
                let ack_packet = Packet {
                    pack_type: PacketType::Ack(ack),
                    routing_header: packet.routing_header.clone(),
                    session_id: packet.session_id,
                };
                self.forward_back(ack_packet);
            },
            PacketType::Nack(nack) => {
                let nack_packet = Packet {
                    pack_type: PacketType::Nack(nack),
                    routing_header: packet.routing_header.clone(),
                    session_id: packet.session_id,
                };
                self.forward_back(nack_packet);
            },
            _ => {}
        }
    }

    fn handle_command(&mut self, command: DroneCommand) {
        match command {
            DroneCommand::SetPacketDropRate(new_pdr) => {
                self.pdr = new_pdr;
            },
            DroneCommand::Crash => {
                println!("Drone {} crashed.", self.id);
                self.process_crash();
                //check if the receiver channel is empty.
                // if is_empty() == true ->  close all the senders (channels) in the hashmap of the drone (neighbors)
                // if is_empty() == false -> process the packet
                return; // Exit the run loop to terminate the thread.
            },
            DroneCommand::AddSender(node_id, sender) => {
                self.packet_send.insert(node_id, sender);
            },
            DroneCommand::RemoveSender(node_id) => {
                self.packet_send.remove(&node_id);
            }
            _ => {}
        }
    }

    fn process_crash(&mut self) {
        println!("Drone {} is entering crashing state.", self.id);

        // Step 1: Notify neighbors to remove references to this drone
        /*
        for (neighbor_id, sender_channel) in self.packet_send.iter() {
            // Notify the neighbor to remove this drone
            if let Some(neighbor) = self.packet_send.get(neighbor_id) {
                //here should remove from the neighbor hashmap of neighbors the drone which crashed,
                println!("Drone {} removed from neighbor {}.", self.id, neighbor_id);
            }

        }*/

        // Step 2: Wait for the receiver channel to be empty
        while !self.packet_recv.is_empty() {
            // Drain remaining packets if new ones arrive
            while let Ok(packet) = self.packet_recv.try_recv() {
                match &packet.pack_type {
                    PacketType::FloodRequest(_) => {} // Ignore FloodRequest packets
                    PacketType::Ack(_) | PacketType::Nack(_) | PacketType::FloodResponse(_) => {
                        self.forward_back(packet.clone());
                    }
                    _ => {
                        self.send_nack(packet.clone(), NackType::ErrorInRouting(self.id));
                    }
                }
            }
        }
        // PROMEMORIA : COME PROCESSIAMO I MESSAGE_FRAGMENT???????????????????????????????????????????
        // Step 2: Close all sender channels to neighbors
        self.packet_send.clear();
        //self.packet_recv=
        println!("Drone {} has crashed and disconnected from neighbors.", self.id);
    }

    fn should_drop_packet(&self) -> bool {
        let mut rng = rand::thread_rng(); // Create a random number generator
        rng.gen_range(0.0..1.0) < self.pdr // Generate a random f32 in [0.0, 1.0)
    }

    fn send_nack(&self, packet: Packet, nack_type: NackType) {
        let fragmentIndex = if let MsgFragment(fragment) = &packet.pack_type {
            fragment.fragment_index
        } else {
            0
        };
        let  nack_packet = Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: fragmentIndex,
                nack_type,
            }),
            routing_header: SourceRoutingHeader{hop_index:1,hops: packet.routing_header.hops[0..packet.routing_header.hop_index].to_owned().iter().rev().copied().collect() },
            session_id: packet.session_id, // Increment session ID if needed
        };
        self.forward_back(nack_packet);
    }

    /*fn send_ack(&self, packet: Packet) {
        let fragmentIndex = match &packet.pack_type {
            MsgFragment(fragment) => fragment.fragment_index,
            _ => 0,
        };

        let ack = Packet {
            pack_type: PacketType::Ack(Ack {
                fragment_index: fragmentIndex,
            }),
            routing_header: SourceRoutingHeader{hop_index:1,hops: packet.routing_header.hops[0..packet.routing_header.hop_index].to_owned().iter().rev().copied().collect() },
            session_id: packet.session_id,
        };

        self.forward_back(ack);
    }*/

    fn forward_back(&self, mut packet: Packet) {

        //packet.routing_header.hop_index += 1;
        // Get the previous hop
        if let Some(prev_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index) {
            if let Some(sender) = self.packet_send.get(&prev_hop) {
                match sender.try_send(packet.clone()) {
                    Ok(()) => {
                        // Cloning ensures that packet is not moved
                        sender.send(packet.clone()).unwrap();
                    }
                    Err(e) => {
                        eprintln!("Failed to forward_back packet: {}", e);
                        // Clone again for sending via ControllerShortcut
                        self.sim_contr_send
                            .send(DroneEvent::ControllerShortcut(packet.clone()))
                            .unwrap_or_else(|err| {
                                eprintln!("Failed to send Ack/NACK via ControllerShortcut: {}", err);
                            });
                    }
                }
            }
        }
    }


    fn process_flood_request(&mut self, request: FloodRequest, original_routing_header: SourceRoutingHeader, seen_flood_ids: &mut HashSet<u64>) {
        let mut updated_request = request.clone();

        if seen_flood_ids.contains(&request.flood_id) {
            updated_request.path_trace.push((self.id, NodeType::Drone));
            self.send_flood_response(request);
        } else {
            seen_flood_ids.insert(updated_request.flood_id);

            // Add this drone to the path trace
            updated_request.path_trace.push((self.id, NodeType::Drone));
            print!("Path trace of request is: {:?}", updated_request.path_trace);

            // Get the ID of the sender (second-to-last hop in the path trace)
            //let sender_id = request.path_trace.iter().rev().nth(1).map(|(id, _)| *id);
            let sender_id = if updated_request.path_trace.len() > 1 {
                Some(updated_request.path_trace[updated_request.path_trace.len() - 2].0)
            } else {
                None
            };

            //************* POSSIBLE ERROR ****************
            eprintln!(
                "Drone {}: Forwarding FloodRequest with flood_id {}. Sender ID: {:?}",
                self.id, request.flood_id, sender_id
            );

            // Forward the FloodRequest to all neighbors except the sender
            for (neighbor_id, sender) in self.packet_send.iter() {
                if Some(*neighbor_id) != sender_id {
                    updated_request.path_trace.push((*neighbor_id,NodeType::Drone));
                    let mut updated_path_trace = updated_request.path_trace.iter().map(|(id,_) | *id ).collect::<Vec<_>>();
                    updated_request.path_trace.pop();
                    eprintln!(
                        "Drone {}: Sending FloodRequest to neighbor {}",
                        self.id, neighbor_id
                    );
                    let packet = Packet {
                        pack_type: PacketType::FloodRequest(FloodRequest {
                            flood_id: updated_request.flood_id,
                            initiator_id: updated_request.path_trace[0].0.clone(),
                            path_trace: updated_request.path_trace.clone(),
                        }),

                        routing_header: original_routing_header.clone(),
                        session_id: 0, // Or use the appropriate session ID if needed
                    };


                    if let Err(err) = sender.send(packet) {
                        eprintln!(
                            "Drone {}: Failed to forward FloodRequest to neighbor {}: {}",
                            self.id, neighbor_id, err
                        );
                    }
                }
            }

            // If this drone has no neighbors except the sender, send a FloodResponse
            if self.packet_send.len() == 1 && sender_id.is_some() {
                self.send_flood_response(updated_request);
            }
        }
    }


    fn send_flood_response(&self, request: FloodRequest) {

        // Create a FloodResponse packet
        let flood_res = FloodResponse {
            flood_id: request.flood_id,
            path_trace: request.path_trace.clone(),
        };
        // Send the FloodResponse back, following the reversed path
        self.forward_back_response(flood_res);
    }

    fn forward_back_response(&self, response: FloodResponse) {
        // Get the previous hop in the path (the second node in the reversed path)
        if let Some(index) = response.path_trace.iter().position(|(x,_)| *x == self.id) {
            println!("The index of {} is {}", self.id, index);
            let next_hop = response.path_trace[index+1].0;
            // The sec ond node in the reversed path is the previous hop
            let response_cloned = response.clone();
            let packet = Packet {
                pack_type: PacketType::FloodResponse(response_cloned), // Correct packet type is FloodResponse
                routing_header: SourceRoutingHeader {
                    hop_index: index, // The last hop in the reversed path
                    hops: response.path_trace.iter().rev().map(|(id, _)| *id).collect(),
                },
                session_id: 0, // Or use the appropriate session ID if needed
            };
            // Find the sender channel for the previous hop
            if let Some(sender) = self.packet_send.get(&next_hop) {
                // Send a FloodResponse back to the previous hop
                sender.send(packet).unwrap();
            } else {
                //send to channel directed to simulation controller
                self.sim_contr_send
                    .send(DroneEvent::ControllerShortcut(packet.clone()))
                    .unwrap_or_else(|err| {
                        eprintln!("Failed to send FloodResponse via ControllerShortcut: {}", err);
                    });
            }
        } else {
            println!("{} is not in the vector", self.id);
        }

    }
}

#[cfg(test)]
mod tests {
   use crate::tests::tests::{generic_chain_fragment_ack, generic_chain_fragment_drop, generic_fragment_drop, generic_fragment_forward, test_drone_crash, test_flood_request};
    //use crate::droneK::drone::MyDrone;
    use crate::droneK::drone::*;
/*
    #[test]
    fn test_fragment_forward() {
        generic_fragment_forward::<MyDrone>();
    }


    #[test]
    fn test_fragment_drop() {
        generic_fragment_drop::<MyDrone>();
    }

    #[test]
    fn test_chain_fragment_drop() {
        generic_chain_fragment_drop::<MyDrone>();
    }


    #[test]
    fn test_chain_fragment_ack() {
        generic_chain_fragment_ack::<MyDrone>();
    }
*/
    #[test]
    fn test_of_flood_request(){test_flood_request::<MyDrone>();}

/*
    #[test]
    fn test_of_drone_crash(){test_drone_crash::<MyDrone>();}
*/
}

