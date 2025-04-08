mod network;
mod nodes;
mod utils;
mod simulation_controller;

use std::collections::HashMap;
use std::error::Error;
use crossbeam_channel::{Receiver, Sender};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use wg_2024::packet::Packet;
use simulation_controller::app::NetworkApp;
use crate::network::TOML_parser;
use crate::network::initializer::{DroneImpl, NetworkInitializer, ParsedConfig};

pub type DroneImplementation = dyn wg_2024::drone::Drone;

fn main() -> Result<(), Box<dyn Error>> {
    // Initialize the application either with GUI or in headless mode
    if std::env::args().any(|arg| arg == "--headless") {
        // Headless mode - just initialize the network and run simulation
        println!("Running in headless mode");
        run_headless_simulation()?;
    } else {
        // GUI mode - launch the application
        println!("Starting GUI application");
        run_gui_application()?;
    }

    Ok(())
}

fn run_headless_simulation() -> Result<(), Box<dyn Error>> {
    // Parse the config file
    let config_path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "topologies/star.toml".to_string());

    println!("Loading network topology from: {}", config_path);
    let config = TOML_parser::parse_config(&config_path)?;

    // Initialize communication channels
    // Initialize communication channels correctly
    let (controller_send, _controller_event_recv) = crossbeam_channel::unbounded::<DroneEvent>(); // For sending DroneEvents
    let (command_send, controller_recv) = crossbeam_channel::unbounded::<DroneCommand>(); // For receiving DroneCommands
    let (packet_send, packet_recv) = crossbeam_channel::unbounded::<Packet>();

    // If you need a HashMap for packet_send:
    let packet_send_map: HashMap<NodeId, Sender<Packet>> = vec![
        (NodeId::from(0), packet_send.clone()), // Assuming a NodeId 0 here
        // Add more nodes if needed
    ]
        .into_iter()
        .collect();

    // Create drone implementations
    let drone_impls = create_drone_implementations(
        &config,
        controller_send, // Send DroneEvent
        controller_recv, // Receive DroneCommand
        packet_recv,
        packet_send_map, // Pass the map of senders
    );

    // Initialize the network
    let mut initializer = NetworkInitializer::new(&config_path, drone_impls)?;
    initializer.initialize()?;

    println!("Network initialized successfully");

    // Run the simulation for a specified duration or until stopped
    let simulation_duration = std::env::args()
        .nth(3)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60); // Default to 60 seconds

    println!("Running simulation for {} seconds", simulation_duration);
    std::thread::sleep(std::time::Duration::from_secs(simulation_duration));

    println!("Simulation completed");
    Ok(())
}

fn run_gui_application() -> Result<(), Box<dyn Error>> {
    // Launch GUI with network configuration
    let options = eframe::NativeOptions::default();


    eframe::run_native(
        "Network Topology Simulator",
        options,
        Box::new(|cc| {
            // You can pass context to the app here
            let app = NetworkApp::new(cc);
            Ok(Box::new(app))
        }),
    )?;

    Ok(())
}

// Helper function to create drone implementations
pub fn create_drone_implementations(
    config: &ParsedConfig, // The parsed configuration
    controller_send: Sender<DroneEvent>,
    controller_recv: Receiver<DroneCommand>,
    packet_recv: Receiver<Packet>,
    packet_send: HashMap<NodeId, Sender<Packet>>,
) -> Vec<DroneImpl> {
    let mut implementations: Vec<DroneImpl> = Vec::new();

    // Iterate over the drones in the config (this assumes `config` holds the necessary drone details)
    for (i, drone_config) in config.drone.iter().enumerate() {
        // For each drone, create the necessary channels, ID, and PDR
        let id = NodeId::from(i as u8); // Assuming IDs are sequential from 0 to 9
        let pdr = drone_config.pdr; // Assuming PDR is part of the config for each drone

        // Now, create the drone instance dynamically
        // For each drone in the configuration, create the corresponding drone instance

        // Example: Create a default drone implementation (you could also use different implementations per team)
        // Assuming `Drone` is a concrete struct implementing the `DroneImplementation` trait
        let drone_impl = Drone::new(
            id,
            controller_send.clone(),
            controller_recv.clone(),
            packet_recv.clone(),
            packet_send.clone(),
            pdr,
        );


        // Push the drone into the list of implementations
        implementations.push(Box::new(drone_impl));
    }

    // If no drone implementations were found, use a default implementation
    if implementations.is_empty() {
        println!("Warning: No drone implementations found. Using default implementation.");
        let default_drone = Drone::new(
            NodeId::from(0), // Example ID for default drone
            controller_send,
            controller_recv,
            packet_recv,
            packet_send,
            1.0, // Example PDR for default drone
        );
        implementations.push(Box::new(default_drone));
    }

    implementations
}


/*
mod network;
mod nodes;
mod utils;
mod simulation_controller;

use simulation_controller::app::NetworkApp;

fn main() -> Result<(), eframe::Error> {
    // Initialize network topology
    /*let network_initializer = NetworkInitializer::new("topologies/star.toml")
        .expect("Failed to initialize network");
*/

    // Launch GUI with network configuration
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Network Topology Simulator",
        options,
        Box::new(|_cc| Ok(Box::new(NetworkApp::default())))
    )
}*/