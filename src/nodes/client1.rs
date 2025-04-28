use std::collections::{HashMap, HashSet};
use crossbeam_channel::{Sender, Receiver};
use std::thread;
use rand::Rng;
use wg_2024::network::NodeId;
use wg_2024::packet::{Packet, PacketType, FloodRequest, FloodResponse};
use wg_2024::packet::NodeType;

pub fn start_client(
    client_id: NodeId,
    packet_rx: Receiver<Packet>,
    packet_senders: HashMap<NodeId, Sender<Packet>>,
) {
    thread::spawn(move || {
        println!("Temp Client {} started", client_id);

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
    });
}
