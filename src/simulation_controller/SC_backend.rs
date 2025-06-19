use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::sync::{Arc, Mutex};
use crossbeam_channel::{Receiver, Sender};
use skylink::SkyLinkDrone;
use wg_2024::packet::Packet;
use wg_2024::controller::{DroneCommand,DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use crate::network::initializer::{ParsedConfig};
use crate::simulation_controller::network_designer::{Node, NodeType};
use crate::simulation_controller::gui_input_queue::{broadcast_topology_change, SharedGuiInput};
use crate::network::initializer::GroupImplFactory;
use crate::network::initializer::DroneImplementation;


pub struct SimulationController {
    network_config: Arc<Mutex<ParsedConfig>>,
    pub(crate) command_sender: Sender<DroneCommand>,
    pub(crate) command_senders: HashMap<NodeId, Sender<DroneCommand>>,
    pub(crate) event_sender: Sender<DroneEvent>,
    pub(crate) network_graph: HashMap<NodeId, HashSet<NodeId>>,
    packet_senders: HashMap<NodeId, Sender<Packet>>,
    pub(crate) packet_receivers: HashMap<NodeId, Receiver<Packet>>,

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


}




impl SimulationController {

    pub fn new(
        network_config: Arc<Mutex<ParsedConfig>>,
        event_sender: Sender<DroneEvent>,
        command_sender: Sender<DroneCommand>,
        drone_factory: Arc<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>, Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32) -> Box<dyn Drone> + Send + Sync>,
        gui_input: SharedGuiInput,
    ) -> Self {
        let group_implementations = SimulationController::load_group_implementations();

        let mut controller = SimulationController {
            network_config: network_config.clone(),
            config: network_config.clone(),
            event_sender: event_sender.clone(),
            command_sender: command_sender.clone(),            command_senders: HashMap::new(),
            network_graph: HashMap::new(),
            packet_senders: HashMap::new(),
            packet_receivers:HashMap::new() ,
            drone_factory,
            gui_input,
            group_implementations,


        };
        // Initialize the network graph
        controller.initialize_network_graph();

        controller
    }

    // Initialize network graph from config
    fn initialize_network_graph(&mut self) {
        let config = self.network_config.lock().unwrap();

        // Add drone connections
        for drone in &config.drone {
            let mut neighbors = HashSet::new();
            for &neighbor_id in &drone.connected_node_ids {
                neighbors.insert(neighbor_id);
            }
            self.network_graph.insert(drone.id, neighbors);
        }

        // Add client connections
        for client in &config.client {
            let mut neighbors = HashSet::new();
            for &drone_id in &client.connected_drone_ids {
                neighbors.insert(drone_id);

                // Add bidirectional connection
                self.network_graph.entry(drone_id)
                    .or_insert_with(HashSet::new)
                    .insert(client.id);
            }
            self.network_graph.insert(client.id, neighbors);
        }

        // Add server connections
        for server in &config.server {
            let mut neighbors = HashSet::new();
            for &drone_id in &server.connected_drone_ids {
                neighbors.insert(drone_id);

                // Add bidirectional connection
                self.network_graph.entry(drone_id)
                    .or_insert_with(HashSet::new)
                    .insert(server.id);
            }
            self.network_graph.insert(server.id, neighbors);
        }
    }

    // Register a command sender for a node


    pub fn start_background_thread(controller: Arc<Mutex<Self>>, event_receiver: Receiver<DroneEvent>) {
        let controller_clone = controller.clone(); // No need to double-Arc
        std::thread::spawn(move || {
            while let Ok(event) = event_receiver.recv() {
                let mut ctrl = controller_clone.lock().unwrap();
                ctrl.process_event(event);
            }
        });
    }

    // Process a drone event
    pub(crate) fn process_event(&mut self, event: DroneEvent) {
        match event {
            DroneEvent::PacketSent(packet) => {
                let hops = &packet.routing_header.hops;
                let hop_index = packet.routing_header.hop_index;

                if hop_index > 0 && hop_index < hops.len() {
                    let sender = hops[hop_index - 1];
                    let receiver = hops[hop_index];
                    println!("Packet sent from {} to {}", sender, receiver);
                } else if hop_index == 0 && !hops.is_empty() {
                    // This means the sender is the first hop, and the receiver is the next hop if present
                    let sender = hops[0];
                    let receiver = hops.get(1).copied();
                    match receiver {
                        Some(r) => println!("Packet sent from {} to {}", sender, r),
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
                    println!("Packet dropped from {} to {}", sender, receiver);
                } else if hop_index == 0 && !hops.is_empty() {
                    let sender = hops[0];
                    let receiver = hops.get(1).copied();
                    match receiver {
                        Some(r) => println!("Packet dropped from {} to {}", sender, r),
                        None => println!("Packet dropped from {} but no receiver (single-hop)", sender),
                    }
                }
            },
            DroneEvent::ControllerShortcut(packet) => {
                if let Some(dest_id) = packet.routing_header.destination() {
                    if let Some(sender) = self.packet_senders.get(&dest_id) {
                        if let Err(e) = sender.send(packet.clone()) {
                            eprintln!("Failed to forward ControllerShortcut to node {}: {}", dest_id, e);
                        } else {
                            println!("ControllerShortcut forwarded to node {}", dest_id);
                        }
                    } else {
                        eprintln!("No packet sender found for destination node {}", dest_id);
                    }
                } else {
                    eprintln!("ControllerShortcut has no destination");
                }
            }
        }
    }

    // Command to make a drone crash
    pub fn crash_drone(&mut self, drone_id: NodeId) -> Result<(), Box<dyn Error>> {
        // 1. Check node exists and is active
        if let Some(state) = self.get_node_state(drone_id) {
            if !state.active {
                return Err(format!("Drone {} is already inactive", drone_id).into());
            }
        } else {
            return Err(format!("Drone {} not found", drone_id).into());
        }

        // 2. Clone the graph and simulate the crash
        let mut test_graph = self.network_graph.clone();
        test_graph.remove(&drone_id);

        if !self.is_crash_allowed(&test_graph) {
            return Err("Crashing this drone would violate network constraints".into());
        }

        // ✅ ONLY NOW perform the crash

        // Remove from neighbors
        if let Some(neighbors) = self.network_graph.get(&drone_id) {
            let neighbors_clone: Vec<NodeId> = neighbors.iter().cloned().collect();

            for neighbor_id in &neighbors_clone {
                if let Some(cmd_tx) = self.command_senders.get(neighbor_id) {
                    cmd_tx
                        .send(DroneCommand::RemoveSender(drone_id))
                        .map_err(|_| "Failed to send RemoveSender command")?;
                }
            }

            if let Some(cmd_tx) = self.command_senders.get(&drone_id) {
                broadcast_topology_change(&self.gui_input,&self.network_config,&"[FloodRequired]::Crash".to_string());

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

        // Final cleanup
        self.network_graph.remove(&drone_id);
        self.command_senders.remove(&drone_id);
        self.packet_senders.remove(&drone_id);

        // Mark inactive
        if let Some(mut state) = self.get_node_state(drone_id) {
            state.active = false;
        }

        Ok(())
    }


    fn is_crash_allowed(&self, test_graph: &HashMap<NodeId, HashSet<NodeId>>) -> bool {
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
                println!("🚨 server must be connected to at least 2 drones");
                return false; //
            }
        }

        // ✅ Also check: all clients can still reach a server
        for client_id in self.get_all_client_ids() {
            if let Some(state) = self.get_node_state(client_id) {
                if state.active {
                    let reachable = self.bfs_reachable_servers(client_id, test_graph);
                    if reachable.is_empty() {

                        return false;
                    }
                }
            }
        }
        true
    }

    pub fn is_removal_allowed(&self, node_a: NodeId, node_b: NodeId) -> bool {
        // Clone the current network graph
        let mut test_graph = self.network_graph.clone();

        // Simulate removal of the bidirectional link
        if let Some(neighbors) = test_graph.get_mut(&node_a) {
            neighbors.remove(&node_b);
        }
        if let Some(neighbors) = test_graph.get_mut(&node_b) {
            neighbors.remove(&node_a);
        }

        // Reuse existing logic!
        self.is_crash_allowed(&test_graph)
    }


    // Add this method to your SimulationController
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

        // 3. Send RemoveSender commands to both nodes
        if let Some(sender) = self.command_senders.get(&a) {
            sender
                .send(DroneCommand::RemoveSender(b))
                .map_err(|e| format!("Failed to send RemoveSender to {}: {}", a, e))?;
        }
        if let Some(sender) = self.command_senders.get(&b) {
            sender
                .send(DroneCommand::RemoveSender(a))
                .map_err(|e| format!("Failed to send RemoveSender to {}: {}", b, e))?;
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

            // Update drone connections
            for drone in &mut cfg.drone {
                if drone.id == a {
                    drone.connected_node_ids.retain(|&id| id != b);
                }
                if drone.id == b {
                    drone.connected_node_ids.retain(|&id| id != a);
                }
            }

            // Update client connections
            for client in &mut cfg.client {
                if client.id == a {
                    client.connected_drone_ids.retain(|&id| id != b);
                }
                if client.id == b {
                    client.connected_drone_ids.retain(|&id| id != a);
                }
            }

            // Update server connections
            for server in &mut cfg.server {
                if server.id == a {
                    server.connected_drone_ids.retain(|&id| id != b);
                }
                if server.id == b {
                    server.connected_drone_ids.retain(|&id| id != a);
                }
            }
        }

        // 6. Broadcast topology change
        broadcast_topology_change(
            &self.gui_input,
            &self.network_config,
            &"[FloodRequired]::RemoveSender".to_string(),
        );

        println!("✅ Successfully removed link between {} and {}", a, b);
        Ok(())
    }

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

    pub fn bfs_reachable_servers(
        &self,
        start_id: NodeId,
        graph: &HashMap<NodeId, HashSet<NodeId>>,
    ) -> HashSet<NodeId> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut reachable_servers = HashSet::new();

        visited.insert(start_id);
        queue.push_back(start_id);

        while let Some(current) = queue.pop_front() {
            if let Some(state) = self.get_node_state(current) {
                if !state.active {
                    continue;
                }

                // ✅ If it's a server, mark it as reachable
                if matches!(state.node_type, NodeType::Server) {
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
        }
        reachable_servers
    }

    // Check if a graph is connected using BFS
    fn is_connected(&self, graph: &HashMap<NodeId, HashSet<NodeId>>) -> bool {
        if graph.is_empty() {
            return true;
        }
        let start_node = *graph.keys().next().unwrap();
        let mut visited = HashSet::new();
        let mut queue = vec![start_node];

        while let Some(node) = queue.pop() {
            if visited.insert(node) {
                if let Some(neighbors) = graph.get(&node) {
                    for &neighbor in neighbors {
                        if !visited.contains(&neighbor) {
                            queue.push(neighbor);
                        }
                    }
                }
            }
        }
        visited.len() == graph.len()
    }

    // Set the packet drop rate for a drone
    pub fn set_packet_drop_rate(&mut self, drone_id: NodeId, rate: f32) -> Result<(), Box<dyn Error>> {
        if let Some(sender) = self.command_senders.get(&drone_id) {
            broadcast_topology_change(&self.gui_input,&self.network_config,&"[FloodRequired]::newpdr".to_string());

            sender.send(DroneCommand::SetPacketDropRate(rate))
                .map_err(|_| "Failed to send SetPacketDropRate command".into())

        } else {
            Err("Drone not found".into())
        }
    }

    // Spawn a new drone in the network
    pub fn spawn_drone(
        &mut self,
        id: NodeId,
        pdr: f32,
        connections: Vec<NodeId>,
    ) -> Result<(), Box<dyn Error>> {
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

        // 3) ✅ CRITICAL: Update network graph - UNCOMMENT AND FIX
        for &peer in &connections {
            self.network_graph.entry(id).or_default().insert(peer);
            self.network_graph.entry(peer).or_default().insert(id);
        }

        // 4) Channels: controller
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        self.command_senders.insert(id, cmd_tx);

        // 5) Channels: packet
        let (pkt_tx, pkt_rx) = crossbeam_channel::unbounded();
        self.packet_senders.insert(id, pkt_tx.clone());
        self.packet_receivers.insert(id, pkt_rx.clone());

        // 6) Build send map for the new drone
        let mut packet_send_map = HashMap::new();
        for &peer in &connections {
            if let Some(tx) = self.packet_senders.get(&peer) {
                packet_send_map.insert(peer, tx.clone());
            }
        }

        // 7) Spawn drone thread
        let controller_send = self.event_sender.clone();
        let controller_recv = cmd_rx;
        let packet_recv = pkt_rx;

        let sky_factory = self
            .group_implementations
            .get("group_7")
            .expect("SkyLink group implementation must exist");

        let mut drone = sky_factory(id, controller_send, controller_recv, packet_recv, packet_send_map, pdr);
        std::thread::spawn(move || {
            drone.run();
        });

        // 8) ✅ CRITICAL: Send AddSender commands bidirectionally
        let new_drone_packet_tx = self.packet_senders.get(&id).unwrap().clone();

        for &peer in &connections {
            // Tell the new drone about its peer
            if let Some(peer_packet_tx) = self.packet_senders.get(&peer) {
                if let Some(new_drone_cmd_tx) = self.command_senders.get(&id) {
                    new_drone_cmd_tx
                        .send(DroneCommand::AddSender(peer, peer_packet_tx.clone()))
                        .map_err(|e| format!("Failed to tell new drone {} about peer {}: {}", id, peer, e))?;
                }
            }

            // Tell the peer about the new drone
            if let Some(peer_cmd_tx) = self.command_senders.get(&peer) {
                peer_cmd_tx
                    .send(DroneCommand::AddSender(id, new_drone_packet_tx.clone()))
                    .map_err(|e| format!("Failed to tell peer {} about new drone {}: {}", peer, id, e))?;
            }
        }

        // 9) ✅ NEW: Force topology refresh for flood protocol
        // Send a special command to refresh routing tables
        for &peer in &connections {
            if let Some(peer_cmd_tx) = self.command_senders.get(&peer) {
                // If you have a RefreshTopology or similar command, send it here
                // peer_cmd_tx.send(DroneCommand::RefreshTopology).ok();
            }
        }

        // 10) ✅ NEW: Initialize the new drone's flood protocol state
        if let Some(new_drone_cmd_tx) = self.command_senders.get(&id) {
            // Send initialization command to ensure flood protocol is active
            // new_drone_cmd_tx.send(DroneCommand::InitializeFloodProtocol).ok();
        }

        // 11) Broadcast to GUI
        broadcast_topology_change(
            &self.gui_input,
            &self.network_config,
            &"[FloodRequired]::SpawnDrone".to_string(),
        );

        println!("✅ Successfully spawned drone {} with connections {:?}", id, connections);

        // 12) ✅ NEW: Add small delay to ensure all commands are processed
        std::thread::sleep(std::time::Duration::from_millis(100));

        Ok(())
    }



    // Validate that adding a new drone won't violate network constraints
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
        // Check that the network will remain valid
        // (This is a simplified check - you might need more validation)
        Ok(true)
    }


    pub fn load_group_implementations() -> HashMap<String, GroupImplFactory> {
        let mut group_implementations = HashMap::new();

        group_implementations.insert(
            "group_7".to_string(),
            Box::new(|id, sim_contr_send, sim_contr_recv, packet_recv, packet_send, pdr| {
                Box::new(SkyLinkDrone::new(
                    id,
                    sim_contr_send,
                    sim_contr_recv,
                    packet_recv,
                    packet_send,
                    pdr,
                )) as Box<dyn DroneImplementation>
            }) as GroupImplFactory, // ✅ this is now correct
        );


        // Optional: add more groups here if needed

        group_implementations
    }

    pub fn add_link(&mut self, a: NodeId, b: NodeId) -> Result<(), Box<dyn std::error::Error>> {
        // 1. Validate that nodes exist
        if !self.command_senders.contains_key(&a) {
            return Err(format!("No command sender for node {}", a).into());
        }
        if !self.command_senders.contains_key(&b) {
            return Err(format!("No command sender for node {}", b).into());
        }
        if !self.packet_senders.contains_key(&a) {
            return Err(format!("No packet sender for node {}", a).into());
        }
        if !self.packet_senders.contains_key(&b) {
            return Err(format!("No packet sender for node {}", b).into());
        }

        // 2. Avoid duplicate links
        if let Some(neighbors) = self.network_graph.get(&a) {
            if neighbors.contains(&b) {
                return Err(format!("Nodes {} and {} are already linked", a, b).into());
            }
        }

        // 3. Clone packet senders (used to send packets to these nodes)
        let packet_to_a = self.packet_senders[&a].clone();
        let packet_to_b = self.packet_senders[&b].clone();

        // 4. Send AddSender to each node
        let command_to_a = self.command_senders[&a].clone();
        command_to_a
            .send(DroneCommand::AddSender(b, packet_to_b))
            .map_err(|e| format!("Failed to send AddSender to {}: {}", a, e))?;

        let command_to_b = self.command_senders[&b].clone();
        command_to_b
            .send(DroneCommand::AddSender(a, packet_to_a))
            .map_err(|e| format!("Failed to send AddSender to {}: {}", b, e))?;

        // 5. Update the internal network graph
        self.network_graph.entry(a).or_default().insert(b);
        self.network_graph.entry(b).or_default().insert(a);
        broadcast_topology_change(&self.gui_input,&self.network_config,&"[FloodRequired]::AddSender".to_string());
        Ok(())
    }
    pub fn add_connection(&mut self, a: NodeId, b: NodeId) {
        self.network_graph.entry(a).or_default().insert(b);
        self.network_graph.entry(b).or_default().insert(a);
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
                    position: default_position, // fallback
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

    pub fn get_packet_sender(&self, node_id: &NodeId) -> Option<&Sender<Packet>> {
        self.packet_senders.get(node_id)
    }

    pub fn register_command_sender(&mut self, node_id: NodeId, sender: Sender<DroneCommand>) {
        self.command_senders.insert(node_id, sender);
    }

    pub fn register_packet_sender(&mut self, node_id: NodeId, sender: Sender<Packet>) {
        self.packet_senders.insert(node_id, sender);
    }
    pub fn registered_nodes(&self) -> Vec<NodeId> {
        self.packet_senders.keys().cloned().collect()
    }
}