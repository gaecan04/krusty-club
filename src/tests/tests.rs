use crossbeam_channel::unbounded;
use std::collections::HashMap;
use std::thread;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::SourceRoutingHeader;
use wg_2024::packet::{Ack, FloodRequest, FloodResponse, Fragment, Nack, NackType, NodeType, Packet, PacketType};

/* THE FOLLOWING TESTS CHECKS IF YOUR DRONE IS HANDLING CORRECTLY PACKETS (FRAGMENT) */


/// Creates a sample packet for testing purposes. For convenience, using 1-10 for clients, 11-20 for drones and 21-30 for servers
pub fn create_sample_packet() -> Packet {
    Packet {
        pack_type: PacketType::MsgFragment(Fragment {
            fragment_index: 1,
            total_n_fragments: 1,
            length: 128,
            data: [1; 128],
        }),
        routing_header: SourceRoutingHeader {
            hop_index: 1,
            hops: vec![1, 11, 12, 21],
        },
        session_id: 1,
    }
}

/// This function is used to test the packet forward functionality of a drone.
pub fn generic_fragment_forward<T: Drone + Send + 'static>() {
    // drone 2 <Packet>
    let (d_send, d_recv) = unbounded();
    // drone 3 <Packet>
    let (d2_send, d2_recv) = unbounded::<Packet>();
    // SC commands
    let (_d_command_send, d_command_recv) = unbounded();

    let neighbours = HashMap::from([(12, d2_send.clone())]);
    let mut drone = T::new(
        11,
        unbounded().0,
        d_command_recv,
        d_recv.clone(),
        neighbours,
        0.0,
    );
    // Spawn the drone's run method in a separate thread
    thread::spawn(move || {
        drone.run();
    });

    let mut msg = create_sample_packet();

    // "Client" sends packet to d
    d_send.send(msg.clone()).unwrap();
    msg.routing_header.hop_index = 2;

    // d2 receives packet from d1
    assert_eq!(d2_recv.recv().unwrap(), msg);
}

/// Checks if the packet is dropped by one drone. The drone MUST have 100% packet drop rate, otherwise the test will fail sometimes.
pub fn generic_fragment_drop<T: Drone + Send + 'static>() {
    // Client 1
    let (c_send, c_recv) = unbounded();
    // Drone 11
    let (d_send, d_recv) = unbounded();
    // SC commands
    let (_d_command_send, d_command_recv) = unbounded();

    let neighbours = HashMap::from([(12, d_send.clone()), (1, c_send.clone())]);
    let mut drone = T::new(
        11,
        unbounded().0, //controller_sender
        d_command_recv, //cont_recv
        d_recv.clone(), //packet-recv
        neighbours,  //packet_send
        1.0,
    );

    // Spawn the drone's run method in a separate thread
    thread::spawn(move || {
        drone.run();
    });

    let msg = create_sample_packet();

    // "Client" sends packet to the drone
    d_send.send(msg.clone()).unwrap();

    let dropped = Nack {
        fragment_index: 1,
        nack_type: NackType::Dropped,
    };
    let srh = SourceRoutingHeader {
        hop_index: 1,
        hops: vec![11, 1],
    };
    let nack_packet = Packet {
        pack_type: PacketType::Nack(dropped),
        routing_header: srh,
        session_id: 1,
    };

    // Client listens for packet from the drone (Dropped Nack)
    assert_eq!(c_recv.recv().unwrap(), nack_packet);
}

/// Checks if the packet is dropped by the second drone. The first drone must have 0% PDR and the second one 100% PDR, otherwise the test will fail sometimes.
pub fn generic_chain_fragment_drop<T: Drone + Send + 'static>() {
    // Client 1 channels
    let (c_send, c_recv) = unbounded();
    // Server 21 channels
    let (s_send, _s_recv) = unbounded();
    // Drone 11
    let (d_send, d_recv) = unbounded();
    // Drone 12
    let (d12_send, d12_recv) = unbounded();
    // SC - needed to not make the drone crash
    let (_d_command_send, d_command_recv) = unbounded();

    // Drone 11
    let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
    let mut drone = T::new(
        11,
        unbounded().0,
        d_command_recv.clone(),
        d_recv.clone(),
        neighbours11,
        0.0,
    );
    // Drone 12
    let neighbours12 = HashMap::from([(11, d_send.clone()), (21, s_send.clone())]);
    let mut drone2 = T::new(
        12,
        unbounded().0,
        d_command_recv.clone(),
        d12_recv.clone(),
        neighbours12,
        1.0,
    );

    // Spawn the drone's run method in a separate thread
    thread::spawn(move || {
        drone.run();
    });

    thread::spawn(move || {
        drone2.run();
    });

    let msg = create_sample_packet();

    // "Client" sends packet to the drone
    d_send.send(msg.clone()).unwrap();


    // Client receive an NACK originated from 'd2'
    assert_eq!(
        c_recv.recv().unwrap(),
        Packet {
            pack_type: PacketType::Nack(Nack {
                fragment_index: 1,
                nack_type: NackType::Dropped,
            }),
            routing_header: SourceRoutingHeader {
                hop_index: 2,
                hops: vec![12, 11, 1],
            },
            session_id: 1,
        }
    );
}

/// Checks if the packet can reach its destination. Both drones must have 0% PDR, otherwise the test will fail sometimes.
pub fn generic_chain_fragment_ack<T: Drone + Send + 'static>() {
    // Client<1> channels
    let (c_send, c_recv) = unbounded();
    // Server<21> channels
    let (s_send, s_recv) = unbounded();
    // Drone 11
    let (d_send, d_recv) = unbounded();
    // Drone 12
    let (d12_send, d12_recv) = unbounded();
    // SC - needed to not make the drone crash
    let (_d_command_send, d_command_recv) = unbounded();

    // Drone 11
    let neighbours11 = HashMap::from([(12, d12_send.clone()), (1, c_send.clone())]);
    let mut drone = T::new(
        11,
        unbounded().0,
        d_command_recv.clone(),
        d_recv.clone(),
        neighbours11,
        0.0,
    );
    // Drone 12
    let neighbours12 = HashMap::from([(11, d_send.clone()), (21, s_send.clone())]);
    let mut drone2 = T::new(
        12,
        unbounded().0,
        d_command_recv.clone(),
        d12_recv.clone(),
        neighbours12,
        0.0,
    );

    // Spawn the drone's run method in a separate thread
    thread::spawn(move || {
        drone.run();
    });

    thread::spawn(move || {
        drone2.run();
    });

    let mut msg = create_sample_packet();

    // "Client" sends packet to the drone
    d_send.send(msg.clone()).unwrap();


    msg.routing_header.hop_index = 3;
    // Server receives the fragment
    assert_eq!(s_recv.recv().unwrap(), msg);
}


pub fn test_flood_request<T: Drone + Send + 'static>() {
    use crossbeam_channel::{unbounded, Receiver, Sender};

    // Create channels
    let (client_send_11,drone_11_recv ) = unbounded(); // Client to Drone 11
    let (drone_11_send_1, client_recv)= unbounded();
    let (drone_11_send_12, drone_12_recv) = unbounded(); // Drone 11
    let (drone_12_send_11, drone_11_recv_unused) = unbounded(); // Drone 12
    let (drone_12_send_13, drone_13_recv) = unbounded();
    let (sim_ctrl_send, sim_ctrl_recv) = unbounded::<DroneEvent>(); // Drone 11 to Simulation Controller
    let (cmd_send, cmd_recv) = unbounded(); // Simulation Controller to Drone 11
    let (drone_13_send_12 , drone_12_recv_unused) = unbounded(); // Drone 13
    // Set up neighbors for Drone 11
    let neighbors11 = HashMap::from([(1, drone_11_send_1.clone()), (12, drone_11_send_12.clone())]);
    let mut drone11 = T::new(
        11,                // Drone 11 ID
        sim_ctrl_send.clone(), // Event channel
        cmd_recv.clone(),         // Command channel
        drone_11_recv.clone(), // Packet receive channel
        neighbors11,       // Neighbor connections
        0.0,              // PDR
    );

    // Set up neighbors for Drone 12
    let neighbors12 = HashMap::from([(11, drone_12_send_11.clone()),(13, drone_12_send_13.clone())]);
    let mut drone12 = T::new(
        12,                // Drone 12 ID
        sim_ctrl_send.clone(), // Event channel
        cmd_recv.clone(), // Command channel
        drone_12_recv.clone(), // Packet receive channel
        neighbors12,       // Neighbor connections
        0.0,              // PDR
    );

    let neighbors13 = HashMap::from([(12, drone_13_send_12.clone())]);
    let mut drone13 = T::new(
        13,                // Drone 12 ID
        sim_ctrl_send.clone(), // Event channel
        cmd_recv.clone(), // Command channel
        drone_13_recv.clone(), // Packet receive channel
        neighbors13,       // Neighbor connections
        0.0,              // PDR
    );

    // Spawn the drones in separate threads
    let drone11_thread = thread::spawn(move || {
        drone11.run();
    });
    let drone12_thread = thread::spawn(move || {
        drone12.run();
    });
    let drone13_thread = thread::spawn(move || {
        drone13.run();
    });

    // Create a FloodRequest packet
    let flood_request = Packet {
        pack_type: PacketType::FloodRequest(FloodRequest {
            flood_id: 1234,
            initiator_id: 1,
            path_trace: vec![(1, NodeType::Client)],

        }),
        routing_header: SourceRoutingHeader {
            hop_index: 1,
            hops: vec![1, 11, 12 , 13],
        },
        session_id: 42,
    };
    eprintln!("Client sending to drone ......");
    // Send the FloodRequest packet to Drone 11
    if client_send_11.send(flood_request.clone()).is_ok() {
        println!("FloodRequest successfully sent to Drone 11.");
    } else {
        eprintln!("Failed to send FloodRequest to Drone 11.");
    }

    eprintln!("{:?}",drone_11_recv.recv());


    // Wait for the forwarded FloodRequest from Drone 11 to Drone 12
    let forwarded_flood_request = drone_12_recv.recv_timeout(std::time::Duration::from_secs(10))
        .expect("Drone 12 did not receive the FloodRequest");

    // Check if the forwarded FloodRequest is correct
    if let PacketType::FloodRequest(flood_request) = &forwarded_flood_request.pack_type {
        print!("Fine 1");
        assert_eq!(flood_request.flood_id, 1234, "Flood ID mismatch");
        print!("Fine 2");
        assert!(flood_request.path_trace.contains(&(11, NodeType::Drone)), "Drone 11 did not add itself to the path trace");
        print!("Fine 3");
    } else {
        print!("Panic");
        panic!("Drone 12 received an unexpected packet type");
    }
    print!("Fine test");
    // Ensure Simulation Controller received a FloodResponse if appropriate
    drone11_thread.join().expect("Drone 11 thread panicked");
    drone12_thread.join().expect("Drone 12 thread panicked");
}


pub fn test_drone_crash<T: Drone + Send + 'static>() {
    // Create channels
    let (client_send, client_recv) = unbounded(); // Client to Drone 11
    let (drone_send, drone_recv) = unbounded();   // Drone 11
    let (neighbor_send, neighbor_recv) = unbounded(); // Neighbor to Drone 11
    let (sim_ctrl_send, sim_ctrl_recv) = unbounded(); // Drone to Simulation Controller
    let (cmd_send, cmd_recv) = unbounded();       // Simulation Controller to Drone 11

    // Create a neighbor drone network
    let neighbors = HashMap::from([(1, client_send.clone()), (3, neighbor_send.clone())]);
    let mut drone = T::new(
        2, // Drone ID
        sim_ctrl_send.clone(),
        cmd_recv,
        drone_recv,
        neighbors,
        0.0, // 0% PDR
    );

    // Start the drone in its own thread
    let drone_thread = thread::spawn(move || {
        drone.run();
    });

    // Create test packets
    let msg_fragment = Packet {
        pack_type: PacketType::MsgFragment(Fragment {
            fragment_index: 0,
            total_n_fragments: 1,
            length: 128,
            data: [0; 128],
        }),
        routing_header: SourceRoutingHeader {
            hop_index: 0,
            hops: vec![1, 2, 3],
        },
        session_id: 1,
    };

    let ack_packet = Packet {
        pack_type: PacketType::Ack(Ack { fragment_index: 0 }),
        routing_header: SourceRoutingHeader {
            hop_index: 1,
            hops: vec![3, 2, 1],
        },
        session_id: 1,
    };

    let flood_response = Packet {
        pack_type: PacketType::FloodResponse(FloodResponse {
            flood_id: 1001,
            path_trace: vec![(3, NodeType::Drone), (2, NodeType::Drone)],
        }),
        routing_header: SourceRoutingHeader {
            hop_index: 1,
            hops: vec![2, 1],
        },
        session_id: 1,
    };

    // Send packets to the drone
    drone_send.send(msg_fragment.clone()).unwrap();
    drone_send.send(ack_packet.clone()).unwrap();
    drone_send.send(flood_response.clone()).unwrap();

    // Issue the crash command
    cmd_send.send(DroneCommand::Crash).unwrap();

    // Simulate sending RemoveSender command to neighbors
    // This would simulate the removal of the drone as a sender
    cmd_send.send(DroneCommand::RemoveSender(3)).unwrap(); // Remove neighbor

    // Validate behavior during crash
    let mut processed_packets = Vec::new();
    while let Ok(event) = sim_ctrl_recv.try_recv() {
        match event {
            DroneEvent::PacketSent(packet) => processed_packets.push(packet),
            DroneEvent::PacketDropped(packet) => processed_packets.push(packet),
            _ => {}
        }
    }

    // Validate that:
    // - MsgFragment sends a Nack with `ErrorInRouting`
    let nack = processed_packets.iter().find(|p| matches!(p.pack_type, PacketType::Nack(_)));
    assert!(nack.is_some(), "Expected a Nack to be sent for MsgFragment");

    // Validate that Ack and FloodResponse are forwarded correctly
    let ack_forwarded = processed_packets
        .iter()
        .any(|p| matches!(p.pack_type, PacketType::Ack(_)));
    assert!(ack_forwarded, "Expected Ack to be forwarded");

    let flood_response_forwarded = processed_packets
        .iter()
        .any(|p| matches!(p.pack_type, PacketType::FloodResponse(_)));
    assert!(flood_response_forwarded, "Expected FloodResponse to be forwarded");

    // Ensure the drone thread exits gracefully
    drone_thread.join().expect("Drone thread panicked");
}



