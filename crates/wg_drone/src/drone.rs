use std::collections::HashSet;
use std::collections::HashMap;
use crossbeam_channel::{select, Receiver, Sender};
use rand::Rng;
use wg_controller::DroneCommand;
use wg_network::{NodeId, SourceRoutingHeader};
use wg_packet::{Ack, FloodRequest, FloodResponse, Nack, NackType, NodeType, Packet, PacketType};
//use crate::MyDrone;

#[derive(Debug, Clone)]
pub struct Drone {
    pub id: NodeId,
    pub pdr: f32,
    pub packet_recv: Receiver<Packet>, // Receives packets from other nodes
    pub packet_send: HashMap<NodeId, Sender<Packet>>, // Sends packets to neighbors
    pub sim_contr_send: Sender<DroneCommand>, // Sends events to Simulation Controller
    pub sim_contr_recv: Receiver<DroneCommand>, // Receives commands from Simulation Controller
}



impl Drone {
    pub fn new(id: NodeId, pdr: f32, sim_contr_send: Sender<DroneCommand>, sim_contr_recv: Receiver<Command>) -> Self {
        Self {
            id,
            pdr,
            packet_recv: crossbeam_channel::unbounded().1,
            packet_send: HashMap::new(),
            sim_contr_send,
            sim_contr_recv,
        }
    }


    pub fn run(&mut self) {
        let mut seen_flood_ids: HashSet<u64> = HashSet::new(); // Track seen flood IDs locally

        loop {
            select! {
                recv(self.packet_recv) -> packet => {
                    if let Ok(packet) = packet {
                        self.handle_packet(packet, &mut seen_flood_ids); // Pass the HashSet to handle_packet
                    }
                },
                recv(self.sim_contr_recv) -> command => {
                    if let Ok(command) = command {
                        self.handle_command(command);
                    }
                }
            }
        }
    }

    fn handle_packet(&mut self, packet: Packet, seen_flood_ids: &mut HashSet<u64>) {
        match packet.pack_type {
            PacketType::MsgFragment(ref fragment) => {
                if self.should_drop_packet() {
                    self.send_nack(packet.clone(), NackType::Dropped);
                } else {
                    self.forward_packet(packet.clone());

                }
            },

            PacketType::Ack(ack)=>{
                let ack_packet = Packet {
                    pack_type: PacketType::Ack(ack),
                    routing_header: packet.routing_header.clone(),
                    session_id: packet.session_id,
                };
                self.forward_back(ack_packet);
            },
            PacketType::Nack(nack)=>{
                let nack_packet = Packet {
                    pack_type: PacketType::Nack(nack),
                    routing_header: packet.routing_header.clone(),
                    session_id: packet.session_id,
                };
                self.forward_back(nack_packet);
            },

            PacketType::FloodRequest(request) => {
                self.process_flood_request(request, seen_flood_ids);
            },
            PacketType::FloodResponse(response) => {
                self.process_flood_response(response, seen_flood_ids);
            },

            _ => {}
        }
    }

    fn handle_command(&mut self, command: DroneCommand) {
        match command {
            DroneCommand::Crash => {
                println!("Drone {} crashed.", self.id);
                return; // Exit the run loop to terminate the thread.
            },
            DroneCommand::AddChannel(node_id, sender) => {
                self.packet_send.insert(node_id, sender);
            },
            /*Command::SetPacketDropRate(new_pdr) => {
                self.pdr = new_pdr;
            },*/
            _ => {}
        }
    }

    fn forward_packet(& self, mut packet: Packet) {
        //ack should be received by client
        if let Some(next_hop) = packet.routing_header.hops.clone().get(packet.routing_header.hop_index) {
            if let Some(sender) = self.packet_send.get(next_hop) {
                packet.routing_header.hop_index += 1;
                sender.send(packet.clone()).unwrap();
                //send ack to prev node until client
                self.send_ack(packet);
            } else {
                self.send_nack(packet, NackType::ErrorInRouting(*next_hop));
            }
        }else {
            self.send_nack(packet, NackType::DestinationIsDrone);
        }
    }

    fn should_drop_packet(&self) -> bool {
        let mut rng = rand::thread_rng();
        rng.gen::<f32>() < self.pdr
    }

    fn send_nack(&self, packet: Packet, nack_type: NackType) {
        /* if let Some(sender_id) = packet.routing_header.hops.first() { //1t elemnt del vettore del source routing
             if let Some(sender_channel) = self.packet_send.get(sender_id) {
                 let nack = Packet {
                     pack_type: PacketType::Nack(Nack {
                         fragment_index: match packet.pack_type {
                             PacketType::MsgFragment(fragment) => fragment.fragment_index,
                             _ => 0, // Default for non-fragment packets
                         },
                         nack_type,
                     }),
                     routing_header: SourceRoutingHeader {
                         hop_index: 0,
                         hops: packet.routing_header.hops.iter().rev().cloned().collect(), // Reverse hops
                     },
                     session_id: packet.session_id,
                 };

                 // Send the Nack
                 if let Err(e) = sender_channel.send(nack) {
                     eprintln!("Failed to send Nack: {}", e);
                 }
             }
         }*/

        //we have more than a drone !!!!
        //1-create packet
        let nack = Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: packet.session_id,
                nack_type:nack_type,
            }),
            routing_header: packet.routing_header.clone(),
            session_id:packet.session_id+1,
        };
        //2- mandare il nack al packet index-1
        self.forward_back(nack);
    }

    fn send_ack(&self, packet: Packet) {
        let mut ack =Packet{
            pack_type: PacketType::Ack(Ack{
                fragment_index: packet.session_id,
            }),
            routing_header: packet.routing_header.clone(),
            session_id: packet.session_id+1, //global var required !!!
        };
        self.forward_back(ack);

    }

    fn forward_back(&self, mut packet: Packet){
        packet.routing_header.hop_index -=1;
        let prev_hop=packet.routing_header.hops.get(packet.routing_header.hop_index).unwrap();
        let sender = self.packet_send.get(prev_hop).unwrap();
        sender.send(packet.clone()).unwrap();

    }

    fn process_flood_request(&mut self, request: FloodRequest, seen_flood_ids: &mut HashSet<u64>) {
        // Check if this flood_id has already been received
        if seen_flood_ids.contains(&request.flood_id) {
            // If already processed, return a FloodResponse
            self.send_flood_response(request);
            return;
        }

        // Add self to the path_trace
        let mut updated_request = request.clone();
        updated_request.path_trace.push((self.id, NodeType::Drone));

        // Track this flood_id to prevent reprocessing
        seen_flood_ids.insert(request.flood_id);

        // Forward the FloodRequest to neighbors (except the sender)
        for (neighbor_id, sender_channel) in self.packet_send.iter() {
            if updated_request.initiator_id != *neighbor_id {
                let forwarded_request = updated_request.clone();
                sender_channel.send(Packet {
                    pack_type: PacketType::FloodRequest(forwarded_request),
                    routing_header: SourceRoutingHeader {
                        hop_index: 0,
                        hops: vec![self.id, *neighbor_id], // Example route
                    },
                    session_id: 0, // Session ID not relevant for flood
                }).unwrap_or_else(|e| eprintln!("Failed to forward FloodRequest to neighbor {}: {}", neighbor_id, e));
            }
        }

        // If this node has no further neighbors, send a FloodResponse
        if self.packet_send.is_empty() {
            self.send_flood_response(request);
        }
    }

    fn process_flood_response(&mut self, response: FloodResponse, seen_flood_ids: &mut HashSet<u64>) {
        // Check if this flood_id has already been processed
        if seen_flood_ids.contains(&response.flood_id) {
            // If already processed, return
            return;
        }

        // Mark this flood_id as seen
        seen_flood_ids.insert(response.flood_id);

        // Print the path_trace for debugging (or update the routing table)
        println!("Drone {} received FloodResponse: {:?}", self.id, response.path_trace);

        // You can use response.path_trace to update the routing or network topology here
        // For example, updating internal routing table or computing best paths

        // Optionally, forward the flood response to neighbors
        for (neighbor_id, sender_channel) in self.packet_send.iter() {
            // Don't send back to the initiator
            if response.path_trace.last().map_or(false, |(id, _)| *id != *neighbor_id) {
                let forwarded_response = response.clone();
                sender_channel.send(Packet {
                    pack_type: PacketType::FloodResponse(forwarded_response),
                    routing_header: SourceRoutingHeader {
                        hop_index: 0,
                        hops: response.path_trace.iter().rev().map(|(id, _)| *id).collect(),
                    },
                    session_id: 0,
                }).unwrap_or_else(|e| eprintln!("Failed to forward FloodResponse to neighbor {}: {}", neighbor_id, e));
            }
        }
    }

    fn send_flood_response(&self, request: FloodRequest) {
        let flood_response = FloodResponse {
            flood_id: request.flood_id,
            path_trace: request.path_trace.clone(),
        };

        /* // Reverse the path_trace for the response
         let reversed_path = request.path_trace.iter().rev().map(|(id, _)| *id).collect::<Vec<NodeId>>();

         // Route back to the initiator
         if let Some(sender_channel) = self.packet_send.get(&request.initiator_id) { //c - d - s
             let packet = Packet {
                 pack_type: PacketType::FloodResponse(flood_response),
                 routing_header: SourceRoutingHeader {
                     hop_index: 0,
                     hops: reversed_path,
                 },
                 session_id: 0, // Session ID not needed for flood response
             };

             // Send the FloodResponse back to the initiator
             if let Err(e) = sender_channel.send(packet) {
                 eprintln!("Failed to send FloodResponse to initiator {}: {}", request.initiator_id, e);
             }
         }*/

        //algo discussed in general

    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{unbounded, Sender, Receiver};
    use rand::Rng;
    use wg_network::{SourceRoutingHeader};
    use wg_packet::{Fragment, FloodRequest, FloodResponse, PacketType};

    #[test]
    fn test_drone_packet_forwarding_and_dropping() {
        // Setup: Initialize sender and receiver for communication
        let (sim_contr_send, sim_contr_recv) = unbounded();
        let (packet_send_1, packet_recv_1) = unbounded(); // Drone input
        let (packet_send_2, packet_recv_2) = unbounded(); // Neighbor input/output

        // Create the drone with a specific PDR
        let mut drone = Drone {
            id: 1,
            pdr: 0.0, // PDR = 0 ensures no packets are dropped in this test.
            packet_recv: packet_recv_1.clone(),
            packet_send: HashMap::from([(2, packet_send_2)]), // Neighbor ID 2
            sim_contr_send: sim_contr_send.clone(),
            sim_contr_recv: sim_contr_recv.clone(),
        };

        // Spawn a thread to run the drone
        let drone_thread = std::thread::spawn(move || {
            drone.run();
        });

        // Step 1: Send a regular message (MsgFragment) to the drone
        let packet = Packet {
            pack_type: PacketType::MsgFragment(Fragment {
                fragment_index: 0,
                total_n_fragments: 1,
                length: 80,
                data: [0; 80],
            }),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: vec![1, 2], // Route: Drone 1 → Neighbor 2
            },
            session_id: 1234,
        };

        packet_send_1.send(packet.clone()).unwrap();

        // Step 2: Verify that the neighbor receives the forwarded packet
        let received_packet = packet_recv_2.recv().unwrap();
        assert_eq!(received_packet.session_id, 1234);
        assert_eq!(received_packet.routing_header.hop_index, 1); // hop_index incremented
        assert_eq!(received_packet.routing_header.hops, vec![1, 2]);

        // Step 3: Test dropping packets (PDR > 0)
        let (packet_send_3, packet_recv_3) = unbounded(); // New neighbor
        let mut drone_with_drop = Drone {
            id: 1,
            pdr: 1.0, // PDR = 1 ensures all packets are dropped.
            packet_recv: packet_recv_1,
            packet_send: HashMap::from([(3, packet_send_3)]), // Neighbor ID 3
            sim_contr_send: sim_contr_send.clone(),
            sim_contr_recv: sim_contr_recv.clone(),
        };

        // Step 4: Send a packet to the new drone
        packet_send_1.send(packet.clone()).unwrap();

        // Ensure the packet is NOT received by the neighbor
        let result = packet_recv_3.recv_timeout(std::time::Duration::from_millis(100));
        assert!(result.is_err()); // Packet should be dropped

        // Cleanup the thread
        drone_with_drop.sim_contr_send.send(DroneCommand::Crash).unwrap();
        drone_thread.join().unwrap();
    }

    #[test]
    fn test_flood_request_and_response_handling() {
        // Setup: Initialize sender and receiver for communication
        let (sim_contr_send, sim_contr_recv) = unbounded();
        let (packet_send_1, packet_recv_1) = unbounded(); // Drone input
        let (packet_send_2, packet_recv_2) = unbounded(); // Neighbor input/output

        // Create the drone
        let mut drone = Drone {
            id: 1,
            pdr: 0.0, // PDR = 0 ensures no packets are dropped in this test.
            packet_recv: packet_recv_1.clone(),
            packet_send: HashMap::from([(2, packet_send_2)]), // Neighbor ID 2
            sim_contr_send: sim_contr_send.clone(),
            sim_contr_recv: sim_contr_recv.clone(),
        };

        // Spawn a thread to run the drone
        let drone_thread = std::thread::spawn(move || {
            drone.run();
        });

        // Step 1: Simulate sending a FloodRequest
        let flood_request = FloodRequest {
            flood_id: 12345,
            initiator_id: 0,
            path_trace: vec![(0, NodeType::Server)],
        };

        packet_send_1.send(Packet {
            pack_type: PacketType::FloodRequest(flood_request.clone()),
            routing_header: SourceRoutingHeader {
                hop_index: 0,
                hops: vec![],
            },
            session_id: 0,
        }).unwrap();

        // Step 2: Drone processes the FloodRequest and sends a FloodResponse
        let received_packet = packet_recv_2.recv().unwrap();
        assert_eq!(received_packet.pack_type, PacketType::FloodRequest(flood_request.clone()));

        // Step 3: Ensure that the FloodResponse is forwarded to the initiator
        let flood_response = FloodResponse {
            flood_id: 12345,
            path_trace: vec![(0, NodeType::Server), (1, NodeType::Drone)], // Example path
        };

        // Send FloodResponse back to the initiator
        drone.send_flood_response(flood_request);

        // Step 4: Verify the FloodResponse is forwarded to the neighbor (initiator)
        let response_packet = packet_recv_2.recv().unwrap();
        assert_eq!(response_packet.pack_type, PacketType::FloodResponse(flood_response));

        // Cleanup the thread
        drone.sim_contr_send.send(DroneCommand::Crash).unwrap();
        drone_thread.join().unwrap();
    }
}


