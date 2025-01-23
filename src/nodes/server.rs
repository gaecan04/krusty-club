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


use std::collections::HashMap;
use crossbeam_channel::{Receiver, Sender};
use wg_2024::packet::{Ack, Fragment, Nack, NackType, Packet, PacketType};
use wg_2024::network::{NodeId, SourceRoutingHeader};
use log::{info, error, warn, debug};

#[derive(Debug, Clone)]
pub struct Server {
    pub id: u64, // Server ID
    pub received_fragments: HashMap<(u64, NodeId), Vec<Option<[u8; 128]>>>, // Maps (session_id, src_id) to fragment data
    pub fragment_lengths: HashMap<(u64, NodeId), u8>, // Maps (session_id, src_id) to length of the last fragment
    pub packet_sender: Sender<Packet>, // Channel to send packets to clients
    pub packet_receiver: Receiver<Packet>, // Channel to receive packets from clients/drones
}

impl Server {
    pub fn new(id: u64, packet_sender: Sender<Packet>, packet_receiver: Receiver<Packet>) -> Self {
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

 */
