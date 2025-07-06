use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::sync::{Arc, Mutex};
use crossbeam_channel::{unbounded, Receiver, Sender};
use log::{info, warn};
use rustastic_drone::RustasticDrone;
use wg_2024::packet::Packet;
use wg_2024::controller::{DroneCommand,DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use crate::network::initializer::{NetworkInitializer, ParsedConfig};
use crate::simulation_controller::network_designer::{Node, NodeType};
use crate::simulation_controller::gui_input_queue::{broadcast_topology_change, SharedGuiInput};
use crate::network::initializer::GroupImplFactory;
use crate::network::initializer::DroneImplementation;

pub struct SimulationController {
    network_config: Arc<Mutex<ParsedConfig>>,
    pub(crate) command_sender: Sender<DroneCommand>,
    pub(crate) command_senders: Arc<Mutex<HashMap<NodeId, Sender<DroneCommand>>>>,

    pub(crate) event_sender: Sender<DroneEvent>,
    pub(crate) network_graph: HashMap<NodeId, HashSet<NodeId>>,
    pub(crate) packet_senders: Arc<Mutex<HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>>>,
    pub(crate) packet_receivers: Arc<Mutex<HashMap<NodeId, Receiver<Packet>>>>,

    host_senders: HashMap<NodeId, Sender<Packet>>, // For SC → Host delivery

    drone_factory: Arc<dyn Fn(
        NodeId,
        Sender<DroneEvent>,
        Receiver<DroneCommand>,
        Receiver<Packet>,
        HashMap<NodeId, Sender<Packet>>,
        f32,
    ) -> Box<dyn Drone> + Send + Sync>,
    config: Arc<Mutex<ParsedConfig>>,
    pub gui_input: SharedGuiInput,
    pub group_implementations: HashMap<String, GroupImplFactory>,

    pub initializer: Arc<Mutex<NetworkInitializer>>,
    shared_senders:  Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>,
    inbox_senders:Arc<Mutex<HashMap<NodeId, Sender<Packet>>>>
}

struct NodeState {
    node_type: NodeType,
    active: bool,
}

impl SimulationController {
    pub fn new(
        network_config: Arc<Mutex<ParsedConfig>>,
        event_sender: Sender<DroneEvent>,
        command_sender: Sender<DroneCommand>,
        drone_factory: Arc<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>, Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32) -> Box<dyn Drone> + Send + Sync>,
        gui_input: SharedGuiInput,
        initializer: Arc<Mutex<NetworkInitializer>>,
        packet_senders: Arc<Mutex<HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>>>,
        packet_receivers: Arc<Mutex<HashMap<NodeId, Receiver<Packet>>>>,
        command_senders: Arc<Mutex<HashMap<NodeId, Sender<DroneCommand>>>>,
        host_senders: HashMap<NodeId, Sender<Packet>>, //SC->hosts
        shared_senders: Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>,
        inbox_senders: Arc<Mutex<HashMap<NodeId, Sender<Packet>>>>,
    ) -> Self {
        let group_implementations = SimulationController::load_group_implementations();

        let mut controller = SimulationController {
            network_config: network_config.clone(),
            config: network_config.clone(),
            event_sender: event_sender.clone(),
            command_sender: command_sender.clone(),
            command_senders,
            network_graph: HashMap::new(),
            packet_senders,
            packet_receivers,
            host_senders,
            drone_factory,
            gui_input,
            group_implementations,
            initializer,
            shared_senders,
            inbox_senders,
        };

        controller.initialize_network_graph();
        controller
    }

    pub fn start_background_thread(controller: Arc<Mutex<Self>>, event_receiver: Receiver<DroneEvent>) {
        let controller_clone = controller.clone(); // No need to double-Arc
        std::thread::spawn(move || {
            while let Ok(event) = event_receiver.recv() {
                let mut ctrl = controller_clone.lock().unwrap();
                ctrl.process_event(event);
            }
        });
    }

    // Initialize network graph from config
    fn initialize_network_graph(&mut self) {
        let config = self.network_config.lock().unwrap();

        for drone in &config.drone {
            let mut neighbors = HashSet::new();
            for &neighbor_id in &drone.connected_node_ids {
                neighbors.insert(neighbor_id);
            }
            self.network_graph.insert(drone.id, neighbors);
        }

        for client in &config.client {
            let mut neighbors = HashSet::new();
            for &drone_id in &client.connected_drone_ids {
                neighbors.insert(drone_id);

                self.network_graph.entry(drone_id)
                    .or_insert_with(HashSet::new)
                    .insert(client.id);
            }
            self.network_graph.insert(client.id, neighbors);
        }

        for server in &config.server {
            let mut neighbors = HashSet::new();
            for &drone_id in &server.connected_drone_ids {
                neighbors.insert(drone_id);

                self.network_graph.entry(drone_id)
                    .or_insert_with(HashSet::new)
                    .insert(server.id);
            }
            self.network_graph.insert(server.id, neighbors);
        }
    }
    pub(crate) fn process_event(&mut self, event: DroneEvent) {
        match event {
            DroneEvent::PacketSent(packet) => {
                let hops = &packet.routing_header.hops;
                let hop_index = packet.routing_header.hop_index;

                if hop_index > 0 && hop_index < hops.len() {
                    let sender = hops[hop_index - 1];
                    let receiver = hops[hop_index];
                    println!("📨Packet sent from {} to {}", sender, receiver);
                } else if hop_index == 0 && !hops.is_empty() {
                    let sender = hops[0];
                    let receiver = hops.get(1).copied();
                    match receiver {
                        Some(r) => println!("📨Packet sent from {} to {}", sender, r),
                        None => println!("Packet sent from {} but no receiver (single-hop)", sender),
                    }
                }
            },
            DroneEvent::PacketDropped(packet) => {
                let hops = &packet.routing_header.hops;
                let hop_index = packet.routing_header.hop_index;

                if hop_index > 0 && hop_index < hops.len() {
                    let sender = hops[hop_index - 1];
                    let receiver = hops[hop_index];
                    println!("🩸Packet dropped from {} to {}", sender, receiver);
                } else if hop_index == 0 && !hops.is_empty() {
                    let sender = hops[0];
                    let receiver = hops.get(1).copied();
                    match receiver {
                        Some(r) => println!("🩸Packet dropped from {} to {}", sender, r),
                        None => println!("Packet dropped from {} but no receiver (single-hop)", sender),
                    }
                }
            },
            DroneEvent::ControllerShortcut(packet) => {
                if let Some(dest_id) = packet.routing_header.destination() {
                    if let Some(sender) = self.host_senders.get(&dest_id) {
                        if let Err(e) = sender.send(packet.clone()) {
                            eprintln!("❌ Failed to send ControllerShortcut to node {}: {}", dest_id, e);
                        } else {
                            println!("✅ ControllerShortcut delivered to node {}", dest_id);
                        }
                    } else {
                        eprintln!("❌ No direct host_sender for destination {}", dest_id);
                    }
                } else {
                    eprintln!("❌ ControllerShortcut has no destination");
                }
            }

        }
    }

    // 🛠️🛠️🛠️ Drone Command Functions🛠️🛠️🛠️
    pub fn crash_drone(&mut self, drone_id: NodeId) -> Result<(), Box<dyn Error>> {
        // 1. Check node exists and is active
        if let Some(state) = self.get_node_state(drone_id) {
            if !state.active {
                return Err(format!("Drone {} is already inactive", drone_id).into());
            }
        } else {
            return Err(format!("Drone {} not found", drone_id).into());
        }

        let mut test_graph = self.network_graph.clone();
        test_graph.remove(&drone_id);

        if !self.is_crash_allowed(&test_graph, drone_id) {
            return Err("Crashing this drone would violate network constraints".into());
        }

        // ✅ ONLY NOW perform the crash

        // Remove from neighbors
        if let Some(neighbors) = self.network_graph.get(&drone_id) {
            let neighbors_clone: Vec<NodeId> = neighbors.iter().cloned().collect();

            for neighbor_id in &neighbors_clone {
                if let Some(NodeType::Drone) = self.get_node_type(*neighbor_id) {
                    if let Some(cmd_tx) = self.command_senders.lock().unwrap().get(neighbor_id) {
                        cmd_tx
                            .send(DroneCommand::RemoveSender(drone_id))
                            .map_err(|_| "Failed to send RemoveSender command")?;
                    }
                }
            }

            if let Some(cmd_tx) = self.command_senders.lock().unwrap().get(&drone_id) {
                cmd_tx
                    .send(DroneCommand::Crash)
                    .map_err(|_| "Failed to send Crash command")?;
            }

            for neighbor_id in &neighbors_clone {
                if let Some(set) = self.network_graph.get_mut(neighbor_id) {
                    set.remove(&drone_id);
                }
            }
        }


        //🆕🆕🆕🆕🆕NEW update the config
        {
            let mut cfg = self.network_config.lock().unwrap();

            for drone in &mut cfg.drone {
                drone.connected_node_ids.retain(|&id| id != drone_id);
            }

            for client in &mut cfg.client {
                client.connected_drone_ids.retain(|&id| id != drone_id);
            }

            for server in &mut cfg.server {
                server.connected_drone_ids.retain(|&id| id != drone_id);
            }
        }



        // Final cleanup
        self.network_graph.remove(&drone_id);
        self.command_senders.lock().unwrap().remove(&drone_id);
        self.packet_senders.lock().unwrap().remove(&drone_id);

        // 🧼 Clean up shared_senders entries for crashed drone
        if let Ok(mut shared) = self.shared_senders.lock() {
            let keys_to_remove: Vec<(NodeId, NodeId)> = shared
                .keys()
                .filter(|(a, b)| *a == drone_id || *b == drone_id)
                .cloned()
                .collect();

            for key in keys_to_remove {
                shared.remove(&key);
                println!("🧹 Removed shared_senders link {:?}", key);
            }
        }

        // Mark inactive
        if let Some(mut state) = self.get_node_state(drone_id) {
            state.active = false;
        }


        broadcast_topology_change(&self.gui_input,&self.network_config,&format!("[FloodRequired]::Crash::{}",drone_id));
        Ok(())
    }

    pub fn remove_link(&mut self, a: NodeId, b: NodeId) -> Result<(), Box<dyn std::error::Error>> {
        // 1. Check if link exists
        if let Some(neighbors) = self.network_graph.get(&a) {
            if !neighbors.contains(&b) {
                return Err(format!("No link exists between nodes {} and {}", a, b).into());
            }
        } else {
            return Err(format!("Node {} not found in network graph", a).into());
        }
        // 2. Check if removal is allowed (doesn't violate constraints)
        if !self.is_removal_allowed(a, b) {
            return Err("Removing this link would violate network constraints".into());
        }
        // 3. Send RemoveSender commands to both nodes if drones
        if self.get_node_type(a) == Some(NodeType::Drone) {
            if let Some(sender) = self.command_senders.lock().unwrap().get(&a) {
                sender.send(DroneCommand::RemoveSender(b))?;
            }
        }
        if self.get_node_type(b) == Some(NodeType::Drone) {
            if let Some(sender) = self.command_senders.lock().unwrap().get(&b) {
                sender.send(DroneCommand::RemoveSender(a))?;
            }
        }
        // 4. Update the network graph
        if let Some(neighbors) = self.network_graph.get_mut(&a) {
            neighbors.remove(&b);
        }
        if let Some(neighbors) = self.network_graph.get_mut(&b) {
            neighbors.remove(&a);
        }
        // 5. Update the config
        {
            let mut cfg = self.network_config.lock().unwrap();
            for drone in &mut cfg.drone {
                if drone.id == a {
                    drone.connected_node_ids.retain(|&id| id != b);
                }
                if drone.id == b {
                    drone.connected_node_ids.retain(|&id| id != a);
                }
            }
            for client in &mut cfg.client {
                if client.id == a {
                    client.connected_drone_ids.retain(|&id| id != b);
                }
                if client.id == b {
                    client.connected_drone_ids.retain(|&id| id != a);
                }
            }
            for server in &mut cfg.server {
                if server.id == a {
                    server.connected_drone_ids.retain(|&id| id != b);
                }
                if server.id == b {
                    server.connected_drone_ids.retain(|&id| id != a);
                }
            }
        }

        if let Ok(mut map) = self.shared_senders.lock() {
            if map.remove(&(a, b)).is_some() {
                info!("🧹 Removed ({}, {}) from shared_senders", a, b);
            }
            if map.remove(&(b, a)).is_some() {
                info!("🧹 Removed ({}, {}) from shared_senders", b, a);
            }
        }
        // 6. Broadcast topology change
        broadcast_topology_change(
            &self.gui_input,
            &self.network_config,
            &format!("[FloodRequired]::RemoveSender::{}::{}", a, b),
        );
        println!("✅ Successfully removed link between {} and {}", a, b);
        Ok(())
    }

    pub fn add_link(&mut self, a: NodeId, b: NodeId) -> Result<(), Box<dyn std::error::Error>> {
        //  1. Validate nodes exist and are initialized ===
        for &node in &[a, b] {
            if self.get_node_type(node) == Some(NodeType::Drone)
                && !self.command_senders.lock().unwrap().contains_key(&node)
            {
                println!("🚨 Missing command_sender for node {}", node);
                return Err(format!("No command sender for node {}", node).into());
            }
            if !self.packet_senders.lock().unwrap().contains_key(&node) {
                println!("🚨 Missing packet_sender map for node {}", node);
                return Err(format!("No packet sender for node {}", node).into());
            }
        }

        //  2. Constraint: Clients must connect to at most 2 drones ===
        for &(client_id, drone_id) in &[(a, b), (b, a)] {
            if self.get_node_type(client_id) == Some(NodeType::Client)
                && self.get_node_type(drone_id) == Some(NodeType::Drone)
            {
                let neighbors = self.network_graph.get(&client_id).cloned().unwrap_or_default();
                let drone_neighbors = neighbors
                    .iter()
                    .filter(|id| self.get_node_type(**id) == Some(NodeType::Drone))
                    .count();

                if drone_neighbors >= 2 {
                    println!("🚨 Client {} already has 2 drone connections", client_id);
                    return Err("Client connection constraint violated".into());
                }
            }
        }

        // 3. Create bidirectional channels ===
        let (tx_ab, rx_ab) = crossbeam_channel::unbounded::<Packet>();
        let (tx_ba, rx_ba) = crossbeam_channel::unbounded::<Packet>();

        // 4. Insert into packet_senders ===
        {
            let mut psenders = self.packet_senders.lock().unwrap();
            psenders.entry(a).or_default().insert(b, tx_ab.clone());
            psenders.entry(b).or_default().insert(a, tx_ba.clone());
        }

        //  5. Spawn forwarding threads ===
        if let Some(inbox_b) = self.inbox_senders.lock().unwrap().get(&b).cloned() {
            std::thread::spawn(move || {
                for pkt in rx_ab {
                    if let Err(e) = inbox_b.send(pkt) {
                        eprintln!("❌ Forwarding thread {} → {} failed: {}", a, b, e);
                        break;
                    }
                }
                println!("⚠️ Forwarding thread from {} to {} exited", a, b);
            });
        }

        if let Some(inbox_a) = self.inbox_senders.lock().unwrap().get(&a).cloned() {
            std::thread::spawn(move || {
                for pkt in rx_ba {
                    if let Err(e) = inbox_a.send(pkt) {
                        eprintln!("❌ Forwarding thread {} → {} failed: {}", b, a, e);
                        break;
                    }
                }
                println!("⚠️ Forwarding thread from {} to {} exited", b, a);
            });
        }

        //  6. Send AddSender to drones ===
        for (node, neighbor, sender) in vec![
            (a, b, tx_ab.clone()),
            (b, a, tx_ba.clone()),
        ] {
            if self.get_node_type(node) == Some(NodeType::Drone) {
                if let Some(cmd_tx) = self.command_senders.lock().unwrap().get(&node) {
                    cmd_tx
                        .send(DroneCommand::AddSender(neighbor, sender))
                        .map_err(|e| format!("Failed to send AddSender to {}: {}", node, e))?;
                }
            }
        }


        //  7. Update network graph and config ===
        self.network_graph.entry(a).or_default().insert(b);
        self.network_graph.entry(b).or_default().insert(a);

        {
            let a_type = self.get_node_type(a);
            let b_type = self.get_node_type(b);
            let mut cfg = self.network_config.lock().unwrap();

            match (a_type, b_type) {
                (Some(NodeType::Drone), Some(NodeType::Drone)) => {
                    cfg.append_drone_connection(a, b);
                    cfg.append_drone_connection(b, a);
                }
                (Some(NodeType::Drone), Some(NodeType::Client)) => {
                    cfg.append_drone_connection(a, b);
                    cfg.append_client_connection(b, a);
                }
                (Some(NodeType::Client), Some(NodeType::Drone)) => {
                    cfg.append_drone_connection(b, a);
                    cfg.append_client_connection(a, b);
                }
                (Some(NodeType::Drone), Some(NodeType::Server)) => {
                    cfg.append_drone_connection(a, b);
                    cfg.append_server_connection(b, a);
                }
                (Some(NodeType::Server), Some(NodeType::Drone)) => {
                    cfg.append_drone_connection(b, a);
                    cfg.append_server_connection(a, b);
                }
                _ => {}
            }
        }

        //  8. Update shared_senders (if applicable) ===
        if let Ok(mut shared) = self.shared_senders.lock() {
            shared.insert((a, b), tx_ab.clone());
            shared.insert((b, a), tx_ba.clone());
            info!("🧪 Inserted ({}, {}) and ({}, {}) into shared_senders", a, b, b, a);
        }

        //  9. Notify GUI ===
        println!("🤎🧸🍂 Successfully added link between {} and {}", a, b);
        broadcast_topology_change(
            &self.gui_input,
            &self.network_config,
            &format!("[FloodRequired]::AddSender::{}::{}", a, b),
        );

        Ok(())
    }
    pub fn spawn_drone(&mut self, id: NodeId, pdr: f32, connections: Vec<NodeId>, ) -> Result<(), Box<dyn Error>> {
        // 1) Validate
        if !self.validate_new_drone(id, &connections)? {
            return Err("New drone configuration violates network constraints".into());
        }

        // 2) Update ParsedConfig (used by GUI)
        {
            let mut cfg = self.network_config.lock().unwrap();

            if !cfg.drone.iter().any(|d| d.id == id) {
                cfg.add_drone(id);
            }

            cfg.set_drone_pdr(id, pdr);
            cfg.set_drone_connections(id, connections.clone());

            for &peer in &connections {
                cfg.append_drone_connection(peer, id);
                // Add bidirectional connections for clients/servers
                if cfg.client.iter().any(|c| c.id == peer) {
                    cfg.append_client_connection(peer, id);
                } else if cfg.server.iter().any(|s| s.id == peer) {
                    cfg.append_server_connection(peer, id);
                }
            }
        }

        // 3) Update network graph
        for &peer in &connections {
            self.network_graph.entry(id).or_default().insert(peer);
            self.network_graph.entry(peer).or_default().insert(id);
        }

        // 4) Channels: controller
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        self.command_senders.lock().unwrap().insert(id, cmd_tx);

        // 5) ⚠️ CRITICAL FIX: Create a single receiver for the new drone
        let (new_drone_main_tx, new_drone_main_rx) = crossbeam_channel::unbounded();

        // Store the main receiver for the drone
        self.packet_receivers.lock().unwrap().insert(id, new_drone_main_rx.clone());

        // 6) Initialize packet_senders map for the new drone
        self.packet_senders.lock().unwrap().insert(id, HashMap::new());

        self.inbox_senders.lock().unwrap().insert(id, new_drone_main_tx.clone());

        // 7) ⚠️ CRITICAL FIX: Create bidirectional channels and update packet_senders
        {
            let mut psenders = self.packet_senders.lock().unwrap();

            for &peer in &connections {
                psenders.entry(peer).or_insert_with(HashMap::new);

                let (tx_to_peer, rx_from_new_drone) = crossbeam_channel::unbounded::<Packet>();
                psenders.get_mut(&id).unwrap().insert(peer, tx_to_peer.clone());
                psenders.get_mut(&peer).unwrap().insert(id, new_drone_main_tx.clone());

                // Forward rx_from_new_drone into peer's inbox
                if let Some(peer_inbox_tx) = self.inbox_senders.lock().unwrap().get(&peer) {
                    let peer_inbox_tx = peer_inbox_tx.clone();
                    std::thread::spawn(move || {
                        for pkt in rx_from_new_drone {
                            if let Err(e) = peer_inbox_tx.send(pkt) {
                                eprintln!("❌ Forwarding thread {} → {} failed: {}", id, peer, e);
                                break;
                            }
                        }
                        println!("⚠️ Forwarding thread from {} to {} exited", id, peer);
                    });
                } else {
                    eprintln!("❌ No inbox Sender found for peer {}", peer);
                }

            }
        }

        // 8) Build send map for the new drone
        let packet_send_map = {
            let psenders = self.packet_senders.lock().unwrap();
            psenders.get(&id).cloned().unwrap_or_default()
        };

        // 9) Spawn drone thread
        let controller_send = self.event_sender.clone();
        let controller_recv = cmd_rx;
        let packet_recv = new_drone_main_rx; // Use the main receiver

        let sky_factory = self
            .group_implementations
            .get("group_1")
            .expect("rustastic_drone group implementation must exist");

        let mut drone = sky_factory(id, controller_send, controller_recv, packet_recv, packet_send_map, pdr);
        std::thread::spawn(move || {
            drone.run();
        });

        // 10) ⚠️ CRITICAL: Send AddSender commands to peers
        for &peer in &connections {
            // Tell peer drones about the new drone's sender
            if self.get_node_type(peer) == Some(NodeType::Drone) {
                if let Some(peer_cmd_tx) = self.command_senders.lock().unwrap().get(&peer) {
                    if let Some(sender_to_peer) = self.packet_senders.lock().unwrap().get(&id).and_then(|m| m.get(&peer)) {
                        peer_cmd_tx
                            .send(DroneCommand::AddSender(id, sender_to_peer.clone()))
                            .map_err(|e| format!("Failed to tell peer {} about new drone {}: {}", peer, id, e))?;
                    }
                }
            }

            // Tell the new drone about its peers
            if let Some(new_drone_cmd_tx) = self.command_senders.lock().unwrap().get(&id) {
                if let Some(sender_to_new_drone) = self.packet_senders.lock().unwrap().get(&peer).and_then(|m| m.get(&id)) {
                    new_drone_cmd_tx
                        .send(DroneCommand::AddSender(peer, sender_to_new_drone.clone()))
                        .map_err(|e| format!("Failed to tell new drone {} about peer {}: {}", id, peer, e))?;
                }
            }
        }

        // 11) ⚠️ Update shared_senders map
        if let Ok(mut shared) = self.shared_senders.lock() {
            let psenders = self.packet_senders.lock().unwrap();
            for &peer in &connections {
                // Insert both directions
                if let Some(sender_to_peer) = psenders.get(&id).and_then(|m| m.get(&peer)) {
                    shared.insert((id, peer), sender_to_peer.clone());
                }
                if let Some(sender_to_new_drone) = psenders.get(&peer).and_then(|m| m.get(&id)) {
                    shared.insert((peer, id), sender_to_new_drone.clone());
                }
            }
        } else {
            warn!("❌ Failed to lock shared_senders during spawn_drone");
        }

        // 12) Broadcast to GUI
        broadcast_topology_change(
            &self.gui_input,
            &self.network_config,
            &format!("[FloodRequired]::SpawnDrone::{}::{:?}", id, connections),
        );

        println!("✅ Successfully spawned drone {} with connections {:?}", id, connections);

        // 13) Add small delay to ensure all commands are processed
        std::thread::sleep(std::time::Duration::from_millis(100));

        Ok(())
    }
    pub fn add_connection(&mut self, a: NodeId, b: NodeId) {
        self.network_graph.entry(a).or_default().insert(b);
        self.network_graph.entry(b).or_default().insert(a);
    }

    pub fn load_group_implementations() -> HashMap<String, GroupImplFactory> {
        let mut group_implementations = HashMap::new();

        group_implementations.insert(
            "group_1".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr| {
                Box::new(RustasticDrone::new(
                    id,
                    sim_contr_send,
                    sim_contr_recv,
                    packet_recv,
                    packet_send,
                    pdr,
                )) as Box<dyn DroneImplementation>
            }) as GroupImplFactory,
        );


        group_implementations
    }

    pub fn set_packet_drop_rate(&mut self, drone_id: NodeId, rate: f32) -> Result<(), Box<dyn Error>> {
        if let Some(sender) = self.command_senders.lock().unwrap().get(&drone_id) {
            broadcast_topology_change(&self.gui_input,&self.network_config,&"[FloodRequired]::newpdr".to_string());

            sender.send(DroneCommand::SetPacketDropRate(rate))
                .map_err(|_| "Failed to send SetPacketDropRate command".into())



        } else {
            Err("Drone not found".into())
        }
    }


    //✅✅✅controls before applying the DroneCommand✅✅✅
    fn is_crash_allowed(&self, test_graph: &HashMap<NodeId, HashSet<NodeId>>, crashing_node: NodeId) -> bool {
        for server_id in self.get_all_server_ids() {
            let neighbors = test_graph.get(&server_id);
            let drone_neighbors = neighbors.map_or(0, |n| {
                n.iter()
                    .filter(|id| {
                        self.get_node_type(**id)
                            .map_or(false, |t| t == NodeType::Drone && test_graph.contains_key(*id))
                    })
                    .count()
            });

            if drone_neighbors < 2 {
                println!("🚨 Server {} must be connected to at least 2 drones", server_id);
                return false;
            }
        }

        if !self.all_clients_and_servers_mutually_reachable(&test_graph, Some(crashing_node)) {
            println!("🚨 Crash would break client-server mutual reachability");
            return false;
        }


        if !self.is_connected(test_graph, Some(crashing_node)) {
            println!("🚨 Graph would become disconnected");
            return false;
        }

        true
    }

    pub fn is_removal_allowed(&self, node_a: NodeId, node_b: NodeId) -> bool {
        let mut test_graph = self.network_graph.clone();

        if let Some(neighbors) = test_graph.get_mut(&node_a) {
            neighbors.remove(&node_b);
        }
        if let Some(neighbors) = test_graph.get_mut(&node_b) {
            neighbors.remove(&node_a);
        }

        self.is_topology_valid(&test_graph)
    }

    fn is_topology_valid(&self, test_graph: &HashMap<NodeId, HashSet<NodeId>>) -> bool {
        for server_id in self.get_all_server_ids() {
            let neighbors = test_graph.get(&server_id);
            let drone_neighbors = neighbors.map_or(0, |n| {
                n.iter()
                    .filter(|id| {
                        self.get_node_type(**id)
                            .map_or(false, |t| t == NodeType::Drone && test_graph.contains_key(*id))
                    })
                    .count()
            });

            if drone_neighbors < 2 {
                println!("🚨 Server {} must be connected to at least 2 drones", server_id);
                return false;
            }
        }

        if !self.all_clients_and_servers_mutually_reachable(&test_graph, None) {
            println!("🚨 Crash would break client-server mutual reachability");
            return false;
        }


        if !self.is_connected(test_graph,None) {
            println!("🚨 Graph would become disconnected");
            return false;
        }

        true
    }

    pub fn bfs_reachable_servers(&self, start_id: NodeId, graph: &HashMap<NodeId, HashSet<NodeId>>, ) -> HashSet<NodeId> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut reachable_servers = HashSet::new();

        visited.insert(start_id);
        queue.push_back(start_id);

        while let Some(current) = queue.pop_front() {
            if !graph.contains_key(&current) {
                continue;
            }
            if self.get_node_type(current) == Some(NodeType::Server) {
                reachable_servers.insert(current);
            }

            if let Some(neighbors) = graph.get(&current) {
                for &neighbor in neighbors {
                    if !visited.contains(&neighbor) {
                        visited.insert(neighbor);
                        queue.push_back(neighbor);
                    }
                }
            }
        }
        reachable_servers
    }

    fn is_connected(&self, graph: &HashMap<NodeId, HashSet<NodeId>>, excluded_node: Option<NodeId>, ) -> bool {
        if graph.is_empty() {
            return true;
        }

        let start_node = graph.keys()
            .find(|&&node_id| {
                Some(node_id) != excluded_node && graph.contains_key(&node_id) });

        let Some(start_node) = start_node else {
            return true;
        };

        let mut visited = HashSet::new();
        let mut queue = vec![*start_node];

        while let Some(node) = queue.pop() {
            if visited.insert(node) {
                if let Some(neighbors) = graph.get(&node) {
                    for &neighbor in neighbors {
                        if Some(neighbor) != excluded_node && graph.contains_key(&neighbor) {
                            queue.push(neighbor);
                        }
                    }
                }
            }
        }

        let active_node_count = graph.keys()
            .filter(|&&node_id| {
                Some(node_id) != excluded_node &&
                    self.get_node_state(node_id)
                        .map_or(false, |state| state.active)
            })
            .count();

        visited.len() == active_node_count
    }

    pub fn all_clients_and_servers_mutually_reachable(&self, graph: &HashMap<NodeId, HashSet<NodeId>>, excluded_node: Option<NodeId>, ) -> bool {

        let clients: Vec<NodeId> = self.get_all_client_ids()
            .into_iter()
            .filter(|id| Some(*id) != excluded_node && graph.contains_key(id))
            .collect();

        let servers: Vec<NodeId> = self.get_all_server_ids()
            .into_iter()
            .filter(|id| Some(*id) != excluded_node && graph.contains_key(id))
            .collect();

        for &client in &clients {
            for &server in &servers {
                println!("🔍 Checking: Client {} → Server {}", client, server);

                if !self.can_reach(graph, client, server, excluded_node) {
                    println!("❌ Client {} cannot reach Server {}", client, server);
                    return false;
                }
                println!("✅ Client {} can reach Server {}", client, server);

            }
        }

        for &server in &servers {
            for &client in &clients {
                if !self.can_reach(graph, server, client, excluded_node) {
                    println!("❌ Server {} cannot reach Client {}", server, client);
                    return false;
                }
            }
        }

        true
    }

    fn can_reach(&self, graph: &HashMap<NodeId, HashSet<NodeId>>, from: NodeId, to: NodeId, excluded_node: Option<NodeId>, ) -> bool {

        if from == to {
            return true;
        }

        if !graph.contains_key(&from) || !graph.contains_key(&to) {
            println!("❌ One of the nodes is not in graph: from={} in_graph={} | to={} in_graph={}",
                     from, graph.contains_key(&from), to, graph.contains_key(&to));
            return false;
        }

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(from);
        visited.insert(from);

        while let Some(current) = queue.pop_front() {

            if current == to {
                return true;
            }

            if let Some(neighbors) = graph.get(&current) {
                for &neighbor in neighbors {
                    if Some(neighbor) == excluded_node || visited.contains(&neighbor) {
                        continue;
                    }

                    let is_target = neighbor == to;
                    let is_dronelike = self.get_node_type(neighbor) == Some(NodeType::Drone);

                    if (is_dronelike || is_target) && graph.contains_key(&neighbor) {
                        visited.insert(neighbor);
                        queue.push_back(neighbor);
                    }
                }
            }
        }
        println!("🚫 NO PATH from {} to {}", from, to);
        false
    }

    fn validate_new_drone(&self, id: NodeId, connections: &[NodeId]) -> Result<bool, Box<dyn Error>> {
        // Check for duplicate ID
        if self.network_graph.contains_key(&id) {
            return Err("Drone ID already exists".into());
        }

        // Check that all connections exist in the network
        for &conn_id in connections {
            if !self.network_graph.contains_key(&conn_id) {
                return Err(format!("Connection node {} does not exist", conn_id).into());
            }
        }

        // Enforce: Clients must connect to at most 2 drones
        for &conn_id in connections {
            if self.get_node_type(conn_id) == Some(NodeType::Client) {
                // Count how many drones are already connected to this client
                let neighbors = self.network_graph.get(&conn_id).cloned().unwrap_or_default();
                let drone_neighbors = neighbors.iter()
                    .filter(|id| self.get_node_type(**id) == Some(NodeType::Drone))
                    .count();

                if drone_neighbors >= 2 {
                    println!("🚨A client already has 2 drone connections. Cannot connect new drone");
                    return Err(format!(
                        "Client {} already has 2 drone connections. Cannot connect new drone {}.",
                        conn_id, id
                    ).into());
                }
            }
        }

        Ok(true)
    }



    //🧭🧭🧭all the getters🧭🧭🧭
    fn get_node_type(&self, id: NodeId) -> Option<NodeType> {
        let cfg = self.config.lock().unwrap();
        if cfg.drone.iter().any(|d| d.id == id) {
            Some(NodeType::Drone)
        } else if cfg.client.iter().any(|c| c.id == id) {
            Some(NodeType::Client)
        } else if cfg.server.iter().any(|s| s.id == id) {
            Some(NodeType::Server)
        } else {
            None
        }
    }
    pub fn get_all_drone_ids(&self) -> Vec<NodeId> {
        self.config
            .lock()
            .unwrap()
            .drone
            .iter()
            .map(|d| d.id)
            .collect()
    }
    pub fn get_all_client_ids(&self) -> Vec<NodeId> {
        self.config
            .lock()
            .unwrap()
            .client
            .iter()
            .map(|c| c.id)
            .collect()
    }
    pub fn get_all_server_ids(&self) -> Vec<NodeId> {
        self.config
            .lock()
            .unwrap()
            .server
            .iter()
            .map(|s| s.id)
            .collect()
    }
    pub fn get_node_state(&self, node_id: NodeId) -> Option<Node> {
        let config = self.network_config.lock().unwrap();

        let default_position = (0.0, 0.0);

        for drone in &config.drone {
            if drone.id == node_id {
                return Some(Node {
                    id: node_id as usize,
                    node_type: NodeType::Drone,
                    pdr: drone.pdr,
                    active: self.network_graph.contains_key(&node_id),
                    position: default_position,
                    manual_position:false,
                });
            }
        }

        for client in &config.client {
            if client.id == node_id {
                return Some(Node {
                    id: node_id as usize,
                    node_type: NodeType::Client,
                    pdr: 1.0,
                    active: self.network_graph.contains_key(&node_id),
                    position: default_position,
                    manual_position:false,
                });
            }
        }

        for server in &config.server {
            if server.id == node_id {
                return Some(Node {
                    id: node_id as usize,
                    node_type: NodeType::Server,
                    pdr: 1.0,
                    active: self.network_graph.contains_key(&node_id),
                    position: default_position,
                    manual_position:false,
                });
            }
        }

        None
    }
    pub fn get_packet_sender(&self, from: NodeId, to: NodeId) -> Option<Sender<Packet>> {
        self.packet_senders.lock().unwrap().get(&from)?.get(&to).cloned()
    }
    pub fn register_command_sender(&mut self, node_id: NodeId, sender: Sender<DroneCommand>) {
        self.command_senders.lock().unwrap().insert(node_id, sender);
    }
    pub fn register_packet_sender(&mut self, from: NodeId, to: NodeId, sender: Sender<Packet>) {
        self.packet_senders
            .lock().unwrap().entry(from)
            .or_insert_with(HashMap::new)
            .insert(to, sender);
    }
    pub fn registered_nodes(&self) -> Vec<NodeId> {
        self.packet_senders.lock().unwrap().keys().cloned().collect()
    }

}
