/*


Clients and servers operate with high level Messages which are disassembled into atomically sized
packets that are routed through the drone network. The Client-Server Protocol standardizes and regulates
the format of these messages and their exchange.

The previously mentioned packets can be: Fragment, Ack, Nack, FloodRequest, FloodResponse.

As described in the main document,
Messages must be serialized and can be possibly fragmented,
and the Fragments can be possibly dropped by drones.

Serialization
As described in the main document, Message fragment cannot contain dynamically-sized data structures
(that is, no Vec, no String, no HashMap etc.). Therefore, packets will contain large, fixed-size arrays instead.

 pub struct Fragment {
	fragment_index: u64,
	total_n_fragments: u64,
	length: u8,
	// assembler will fragment/de-fragment data into bytes.
	data: [u8; 128] // usable for image with .into_bytes()
}

To reassemble fragments into a single packet, a client or server uses the fragment header as follows:

    1- The client or server receives a fragment.
    2- It first checks the (session_id, src_id) tuple in the header.
    3- If it has not received a fragment with the same (session_id, src_id) tuple, then it creates a vector (Vec<u8>
    with capacity of total_n_fragments * 128) where to copy the data of the fragments.
    4- It would then copy length elements of the data array at the correct offset in the vector.
    Note: if there are more than one fragment, length must be 128 for all fragments except for the last.
    The length of the last one is specified by the length component inside the fragment,

If the client or server has already received a fragment with the same session_id, then it just needs to
copy the data of the fragment in the vector.

Once that the client or server has received all fragments (that is, fragment_index 0 to total_n_fragments - 1),
 then it has reassembled the whole message and sends back an Ack.
*/

use std::collections::HashMap;
use crossbeam_channel::{Receiver, RecvError, Sender};
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
    recovery_in_progress:  HashMap<(u64, NodeId), bool>, // Tracks if recovery is already in progress for a session
    drop_counts: HashMap<(u64, NodeId), usize>, // Track number of drops per session
}

impl server {
    pub(crate) fn new(id: u8, packet_sender: Sender<Packet>, packet_receiver: Receiver<Packet>) -> Self {
        // Log server creation
        info!("Server {} created.", id);

        Self {
            id,
            received_fragments: HashMap::new(),
            fragment_lengths: HashMap::new(),
            packet_sender: HashMap::new(),
            packet_receiver,
            recovery_in_progress: HashMap::new(),
            drop_counts: HashMap::new(),
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

        // If this is the first fragment, also initialize the recovery tracker
        self.recovery_in_progress.entry(key).or_insert(false);

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
            // Clear recovery status since message is complete
            self.recovery_in_progress.remove(&key);
        } else {
            // Only start recovery if we've received a significant portion but not all fragments
            let received_count = entry.iter().filter(|f| f.is_some()).count();
            let total_count = fragment.total_n_fragments as usize; //--> this should introduce a delay???

            // Only trigger recovery if we've received more than half the fragments but not all
            if received_count > total_count / 2 && received_count < total_count && !self.recovery_in_progress[&key] {
                self.recovery_in_progress.insert(key, true);
                self.recover_lost_fragments(session_id, routing_header);
            }
        }
    }
    //fn process_nack(&mut self, nack: &Nack, packet: &mut Packet)
    fn handle_nack(&mut self, session_id: u64, nack: &Nack, routing_header: SourceRoutingHeader) {

        info!("Recieved NACK for fragment {} with type {:?} in session {}", nack.fragment_index, nack.nack_type, session_id);
        let key = (session_id, routing_header.hops[0]);
        let drop_count= self.drop_counts.entry(key).or_insert(0);
        *drop_count += 1;

        match nack.nack_type {
            NackType::Dropped => { // the only case i recieve nack Dropped is when i am forwarding the fragments to the second client
                // Nacktype: ErrorInRouting ---> floodRequest --> find crashed drone --> remove from graph --> calculate path to dijstra and keep sending
                //put a limit of 5 dropped fragments, at the 6th the server will flood the network sending a floodrequest
                // to the drone to whom it is connected.
                if *drop_count > 5 {
                    info!("Drop threshold exceeded for session {:?}, initiating flood", key);
                    // Generate a unique flood ID (can use a counter or session_id)
                    let flood_id = session_id;

                    let flood_request = FloodRequest {
                        flood_id: flood_id,
                        initiator_id: self.id as NodeId,
                        path_trace: vec![(self.id as NodeId, NodeType::Server)], //contains my id,
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

                    if let Some(next_hop) = flood_packet.routing_header.hops.get(flood_packet.routing_header.hop_index) {
                        if let Some(sender) = self.packet_sender.get(next_hop) {
                            match sender.try_send(flood_packet.clone()) {
                                Ok(()) => {
                                    info!("FloodRequest sent to node {} for session {}", next_hop, session_id);
                                    // Reset drop count after successfully sending flood request
                                    self.drop_counts.insert(key, 0);
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

                } else {
                    // Normal NACK handling for under-threshold drops
                    info!("Drop count for session {:?}: {}/5", key, *drop_count);
                    // Send NACK back along the original path
                    let nack_packet = Packet::new_nack(
                        SourceRoutingHeader {
                            hop_index: 0,
                            hops: vec![routing_header.hops[routing_header.hops.len() - 1]],
                        },
                        session_id,
                        nack.clone()
                    );
                    let destination = routing_header.hops[routing_header.hops.len() - 1];
                    if let Some(sender) = self.packet_sender.get(&destination) {
                        if let Err(err) = sender.try_send(nack_packet) {
                            error!("Failed to resend Nack for fragment {}: {}", nack.fragment_index, err);
                        } else {
                            info!("Sent NACK for fragment {} back to node {}", nack.fragment_index, destination);
                        }
                    } else {
                        error!("No sender found for destination node {}", destination);
                    }
                }
            }
            _ => {
                    warn!("Received non-dropped NACK type, flooding mechanism not implemented yet");
            }
                // For other NackTypes (like routing issues), use flooding approach
                //self.flood(session_id, nack.fragment_index, routing_header); -------------> NOT WORKING: TO SEE HOW FLOODING IS IMPLEMENTED, WHO CAN FLOOD, MAYBE CONSIDER TRAIT?????
        }
    }
    ///Function to recover the missing fragments:
    fn recover_lost_fragments(&mut self, session_id: u64, routing_header: SourceRoutingHeader) {
        let key = (session_id, routing_header.hops[0]);
        if let Some(fragments) = self.received_fragments.get(&key) {
            let mut missing_indexes = Vec::new();
            for (index, fragment) in fragments.iter().enumerate() {
                if fragment.is_none() {
                    missing_indexes.push(index as u64);
                }
            }

            if !missing_indexes.is_empty() {
                warn!("Detected missing fragments {:?} for session {:?}", missing_indexes, key);
                for missing_index in missing_indexes {
                    // Create NACK packet for each missing fragment
                    let nack = Nack {
                        fragment_index: missing_index,
                        nack_type: NackType::Dropped,
                    };

                    // Create reversed routing header to send back to source
                    let reverse_routing_header = SourceRoutingHeader {
                        hop_index: 0, // Start at first hop when sending back
                        hops: routing_header.hops.iter().rev().copied().collect(),
                    };

                    // Create NACK packet
                    let nack_packet = Packet::new_nack(
                        reverse_routing_header,
                        session_id,
                        nack
                    );

                    // Send the NACK packet to the first hop in the reversed path
                    if let Some(&first_hop) = nack_packet.routing_header.hops.first() {
                        if let Some(sender) = self.packet_sender.get(&first_hop) {
                            match sender.try_send(nack_packet) {
                                Ok(()) => {
                                    info!("Sent NACK for missing fragment {} in session {}", missing_index, session_id);
                                }
                                Err(e) => {
                                    error!("Failed to send NACK for missing fragment {}: {:?}", missing_index, e);
                                }
                            }
                        } else {
                            error!("No sender found for node {}", first_hop);
                        }
                    } else {
                        error!("No route available to send NACK for fragment {}", missing_index);
                    }
                }
            } else {
                info!("All fragments received for session {:?}", key);
            }
        } else {
            warn!("No fragment record found for session {:?}", key);
        }
    }



    fn handle_complete_message(&mut self, key: (u64, NodeId), routing_header: SourceRoutingHeader) {
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

        // Send an acknowledgment
        //self.send_ack(key, routing_header);
    }

    // modifica cosi che ogni volta che ricevo un frammento mando un ack, con il proprio index number.
    fn send_ack(&mut self, packet: &mut Packet, fragment: &Fragment) {
        let ack_packet = Packet {
            pack_type: PacketType::Ack(Ack {
                fragment_index: fragment.fragment_index,
            }),
            routing_header: SourceRoutingHeader {
                hop_index: packet.routing_header.hops.len() - 1,
                hops: packet.routing_header.hops.iter().rev().copied().collect(),
            },
            session_id: packet.session_id,
        };
        if let Some(next_hop) = packet.routing_header.hops.get(packet.routing_header.hop_index){
            if let Some(sender) = self.packet_sender.get(&next_hop){
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
    /*
    pub struct Fragment {
        pub fragment_index: u64,
        pub total_n_fragments: u64,
        pub length: u8,
        pub data: [u8; FRAGMENT_DSIZE],
    }
     */


    //handle_flood_response is still to be checked properly and tested.
    fn handle_flood_response(&mut self, session_id: u64, flood_response: FloodResponse, routing_header: SourceRoutingHeader) {
        info!("Received FloodResponse for flood_id {} in session {}", flood_response.flood_id, session_id);

        // Extract the path from the response
        let path = flood_response.path_trace
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<NodeId>>();

        info!("New path discovered: {:?}", path);
        //integrare tutti i collegamenti e nodi del path dentro il grado

        // TODO: Use this path for future communications with this client
        // You might want to store this path in a new field in your server struct

        // Reset drop count for this session since we've now established a new path
        let key = (session_id, path[0]); // First node should be the client
        self.drop_counts.insert(key, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*; // Assuming the server module and related types are in the parent module
    use crossbeam_channel::unbounded;
    use wg_2024::packet::{Packet, PacketType, Fragment, Ack};
    use wg_2024::network::{SourceRoutingHeader, NodeId};

    #[test]
    fn test_server_reassembly_and_ack() {
        // Create channels for sending/receiving packets.
        // packet_sender is used by the server to send out packets (ACKs),
        // and packet_receiver is what the server listens to.
        let (ack_tx, ack_rx) = unbounded(); // For packets sent by server (ack)
        let (packet_tx, packet_rx) = unbounded(); // For packets arriving at the server

        // Instantiate the server.
        let mut srv = server::new(1, ack_tx, packet_rx);

        // For this test, we'll use a fixed session id and a dummy source NodeId.
        let session_id: u64 = 42;
        let src_node: NodeId = 10; // assuming NodeId is a type alias like u8
        // Create a routing header with the source in the hops list.
        let routing_header = SourceRoutingHeader {
            hop_index: 0,
            hops: vec![src_node],
        };

        // --- Prepare Fragment 0 (first fragment) ---
        let mut data0 = [0u8; 128];
        // For test purposes, fill data0 with a sequence of bytes.
        for i in 0..128 {
            data0[i] = i as u8;
        }
        let fragment0 = Fragment {
            fragment_index: 0,
            total_n_fragments: 2, // message will be split into 2 fragments
            length: 128,          // full length for non-final fragment
            data: data0,
        };

        // --- Prepare Fragment 1 (last fragment) ---
        let mut data1 = [0u8; 128];
        // We'll use a short message for the last fragment; for instance "hello world"
        let last_msg = b"hello world"; // 11 bytes
        // Copy the meaningful bytes into data1.
        data1[..last_msg.len()].copy_from_slice(last_msg);
        let fragment1 = Fragment {
            fragment_index: 1,
            total_n_fragments: 2,
            length: last_msg.len() as u8, // actual length of the last fragment
            data: data1,
        };

        // Create Packet wrappers for each fragment.
        let packet0 = Packet {
            pack_type: PacketType::MsgFragment(fragment0),
            routing_header: routing_header.clone(),
            session_id,
        };
        let packet1 = Packet {
            pack_type: PacketType::MsgFragment(fragment1),
            routing_header: routing_header.clone(),
            session_id,
        };

        println!("Sending fragment packet 0:\n{:#?}", packet0);
        println!("Sending fragment packet 1:\n{:#?}", packet1);

        // Send the fragments to the server.
        packet_tx.send(packet0).unwrap();
        packet_tx.send(packet1).unwrap();
        // Drop the sender so that server.run() can eventually exit its loop.
        drop(packet_tx);

        // Run the server. This will process incoming packets, reassemble the message,
        // and send back an acknowledgment.
        srv.run();

        // At this point, the server should have reassembled the message and sent an ACK.
        // Check that exactly one packet was sent on the ack channel.
        let acks: Vec<Packet> = ack_rx.try_iter().collect();
        println!("Received ACK packets:\n{:#?}", acks);
        assert_eq!(acks.len(), 1, "Expected one acknowledgment packet.");

        // Verify the ACK packet has the right session_id and reversed routing header.
        let ack_packet = &acks[0];
        match &ack_packet.pack_type {
            PacketType::Ack(ack) => {
                println!("ACK packet details:\n{:#?}", ack);
                assert_eq!(ack_packet.session_id, session_id, "Session ID should match.");
                // The server reverses the hops; for a single-element hops list the reversal is the same.
                assert_eq!(ack_packet.routing_header.hops, vec![src_node]);
                // hop_index should be hops.len()-1 (which is 0 in this case).
                assert_eq!(ack_packet.routing_header.hop_index, 0);
            }
            _ => panic!("Expected an ACK packet, but got a different packet type."),
        }

        // Optionally, one could also check the logs or add hooks to inspect the reassembled message.
        // In our current server implementation the message is logged but not exposed.
    }
    #[test]
    fn test_recovery_missing_fragment() {
        // Create channels for sending/receiving packets
        let (server_tx, server_rx) = unbounded(); // Packets sent by server
        let (packet_tx, packet_rx) = unbounded(); // Packets received by server

        // Instantiate the server
        let mut srv = server::new(1, server_tx, packet_rx);

        // Test setup
        let session_id: u64 = 43;
        let src_node: NodeId = 10;
        let dst_node: NodeId = 20;
        let intermediate_node: NodeId = 15;

        // Create a routing header with multiple hops to test recovery with route changes
        let routing_header = SourceRoutingHeader {
            hop_index: 0,
            hops: vec![src_node, intermediate_node, dst_node],
        };

        // Create a message that will be split into 3 fragments
        // Prepare fragment 0
        let mut data0 = [0u8; 128];
        for i in 0..128 {
            data0[i] = i as u8;
        }
        let fragment0 = Fragment {
            fragment_index: 0,
            total_n_fragments: 3,
            length: 128,
            data: data0,
        };

        // Prepare fragment 2 (last fragment)
        let mut data2 = [0u8; 128];
        let last_msg = b"final fragment";
        data2[..last_msg.len()].copy_from_slice(last_msg);
        let fragment2 = Fragment {
            fragment_index: 2,
            total_n_fragments: 3,
            length: last_msg.len() as u8,
            data: data2,
        };

        // Create packet wrappers
        let packet0 = Packet {
            pack_type: PacketType::MsgFragment(fragment0),
            routing_header: routing_header.clone(),
            session_id,
        };

        let packet2 = Packet {
            pack_type: PacketType::MsgFragment(fragment2),
            routing_header: routing_header.clone(),
            session_id,
        };

        // Send fragments 0 and 2 (skip 1 to simulate packet loss)
        println!("Sending fragments 0 and 2 (skipping 1 to test recovery)");

        // First send fragment 0
        srv.handle_fragment(
            packet0.session_id,
            if let PacketType::MsgFragment(frag) = packet0.pack_type { &frag } else { panic!() },
            packet0.routing_header
        );

        // Then send fragment 2
        srv.handle_fragment(
            packet2.session_id,
            if let PacketType::MsgFragment(frag) = packet2.pack_type { &frag } else { panic!() },
            packet2.routing_header
        );

        // Check if the server sent a NACK for the missing fragment
        let sent_packets: Vec<Packet> = server_rx.try_iter().collect();
        println!("Server sent {} packets", sent_packets.len());

        // Find NACKs for fragment 1
        let nacks: Vec<&Packet> = sent_packets.iter()
            .filter(|p| {
                if let PacketType::Nack(nack) = &p.pack_type {
                    nack.fragment_index == 1 && p.session_id == session_id
                } else {
                    false
                }
            })
            .collect();

        assert_eq!(nacks.len(), 1, "Server should have sent exactly one NACK for missing fragment 1");

        // Verify the NACK has the correct details
        let nack_packet = nacks[0];
        if let PacketType::Nack(nack) = &nack_packet.pack_type {
            assert_eq!(nack.fragment_index, 1, "NACK should be for fragment 1");
            // Verify the routing header is correct for sending back to source
            assert_eq!(nack_packet.routing_header.hops[0], dst_node,
                       "NACK should be routed back with first hop being the destination");
        } else {
            panic!("Expected a NACK packet");
        }

        // Verify recovery_in_progress is set to true
        let key = (session_id, src_node);
        assert!(srv.recovery_in_progress[&key], "Recovery in progress flag should be set");
    }
    /*
    #[test]
    fn test_recovery_missing_fragment() {
        // Create channels for sending/receiving packets
        let (server_tx, server_rx) = unbounded(); // Packets sent by server
        let (packet_tx, packet_rx) = unbounded(); // Packets received by server

        // Instantiate the server
        let mut srv = server::new(1, server_tx, packet_rx);

        // Test setup
        let session_id: u64 = 43;
        let src_node: NodeId = 10;
        let dst_node: NodeId = 20;
        let intermediate_node: NodeId = 15;

        // Create a routing header with multiple hops to test recovery with route changes
        let routing_header = SourceRoutingHeader {
            hop_index: 0,
            hops: vec![src_node, intermediate_node, dst_node],
        };

        // Create a message that will be split into 3 fragments
        // Prepare fragment 0
        let mut data0 = [0u8; 128];
        for i in 0..128 {
            data0[i] = i as u8;
        }
        let fragment0 = Fragment {
            fragment_index: 0,
            total_n_fragments: 3,
            length: 128,
            data: data0,
        };

        // Prepare fragment 1
        let mut data1 = [0u8; 128];
        for i in 0..128 {
            data1[i] = (i + 128) as u8;
        }
        let fragment1 = Fragment {
            fragment_index: 1,
            total_n_fragments: 3,
            length: 128,
            data: data1,
        };

        // Prepare fragment 2 (last fragment)
        let mut data2 = [0u8; 128];
        let last_msg = b"final fragment";
        data2[..last_msg.len()].copy_from_slice(last_msg);
        let fragment2 = Fragment {
            fragment_index: 2,
            total_n_fragments: 3,
            length: last_msg.len() as u8,
            data: data2,
        };

        // Create packet wrappers
        let packet0 = Packet {
            pack_type: PacketType::MsgFragment(fragment0),
            routing_header: routing_header.clone(),
            session_id,
        };

        let packet2 = Packet {
            pack_type: PacketType::MsgFragment(fragment2),
            routing_header: routing_header.clone(),
            session_id,
        };

        // Send fragments 0 and 2 (skip 1 to simulate packet loss)
        println!("Sending fragment 0 and 2 (skipping 1 to test recovery)");
        packet_tx.send(packet0).unwrap();
        packet_tx.send(packet2).unwrap();

        // Process these packets with a separate thread to allow testing the response
        std::thread::spawn(move || {
            // Process incoming packets in a separate thread
            for _ in 0..2 {
                if let Ok(packet) = packet_rx.recv() {
                    match &packet.pack_type {
                        PacketType::MsgFragment(fragment) => {
                            srv.handle_fragment(packet.session_id, fragment.clone(), packet.routing_header.clone());
                        },
                        _ => {}
                    }
                }
            }
        });

        // Wait for the server to process and detect missing fragment
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Check if the server sent a NACK for the missing fragment
        let sent_packets: Vec<Packet> = server_rx.try_iter().collect();
        println!("Server sent {} packets", sent_packets.len());

        // Verify at least one packet was sent and it was a NACK
        assert!(!sent_packets.is_empty(), "Server should have sent at least one packet");

        // Find NACKs for fragment 1
        let nacks: Vec<&Packet> = sent_packets.iter()
            .filter(|p| {
                if let PacketType::Nack(nack) = &p.pack_type {
                    nack.fragment_index == 1 && p.session_id == session_id
                } else {
                    false
                }
            })
            .collect();

        assert!(!nacks.is_empty(), "Server should have sent a NACK for missing fragment 1");

        // Verify the NACK has the correct details
        let nack_packet = nacks[0];
        if let PacketType::Nack(nack) = &nack_packet.pack_type {
            assert_eq!(nack.fragment_index, 1, "NACK should be for fragment 1");
            // Verify routing header is reversed (destination becomes source)
            assert_eq!(nack_packet.routing_header.hops[0], dst_node,
                       "NACK should be routed back to destination");
        } else {
            panic!("Expected a NACK packet");
        }
    }
*/
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
