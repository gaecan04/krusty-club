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
use crossbeam_channel::{Receiver, Sender};
use wg_2024::packet::{Ack, Fragment, Nack, NackType, Packet, PacketType};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use log::{info, error, warn, debug};

#[derive(Debug, Clone)]
pub struct server {
    pub id: u8, // Server ID
    pub received_fragments: HashMap<(u64, NodeId), Vec<Option<[u8; 128]>>>, // Maps (session_id, src_id) to fragment data
    pub fragment_lengths: HashMap<(u64, NodeId), u8>, // Maps (session_id, src_id) to length of the last fragment
    pub packet_sender: Sender<Packet>, // Channel to send packets to clients
    pub packet_receiver: Receiver<Packet>, // Channel to receive packets from clients/drones
}

impl server {
    pub fn new(id: u8, packet_sender: Sender<Packet>, packet_receiver: Receiver<Packet>) -> Self {
        // Log server creation
        info!("Server {} created.", id);

        Self {
            id,
            received_fragments: HashMap::new(),
            fragment_lengths: HashMap::new(),
            packet_sender,
            packet_receiver,
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
            if let Ok(packet) = self.packet_receiver.recv() {
                match packet.pack_type {
                    PacketType::MsgFragment(fragment) => {
                        self.handle_fragment(packet.session_id, fragment, packet.routing_header);
                    }
                    _ => {
                        warn!("Server {} received unexpected packet type.", self.id);
                    }
                }
            } else {
                error!("No packet received or channel closed.");
                break;
            }
        }
    }

    /// Handle fragment processing
    fn handle_fragment(&mut self, session_id: u64, fragment: Fragment, routing_header: SourceRoutingHeader) {
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
            self.handle_complete_message(key, routing_header.clone());
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
        self.send_ack(key, routing_header);
    }

    fn send_ack(&mut self, key: (u64, NodeId), routing_header: SourceRoutingHeader) {
        let ack_packet = Packet {
            pack_type: PacketType::Ack(Ack {
                fragment_index: 0, // Not applicable for complete message acknowledgment
            }),
            routing_header: SourceRoutingHeader {
                hop_index: routing_header.hops.len() - 1,
                hops: routing_header.hops.iter().rev().copied().collect(),
            },
            session_id: key.0,
        };

        if let Err(err) = self.packet_sender.send(ack_packet) {
            error!("Failed to send Ack for session {:?}: {}", key, err);
        } else {
            info!("Acknowledgment sent for session {:?}", key);
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
}
