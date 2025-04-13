mod network;
mod nodes;
mod utils;
mod simulation_controller;

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use crossbeam_channel::{Receiver, Sender};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use wg_2024::packet::Packet;
use simulation_controller::app::NetworkApp;
use crate::network::TOML_parser;
use crate::network::initializer::{DroneImplementation, MyDrone, NetworkInitializer, ParsedConfig};
use utils::serialization_fix;

//pub type DroneImplementation = dyn wg_2024::drone::Drone;

// Concrete implementation of the Drone trait

/*pub trait DroneImplementation: Drone {
    // Additional methods specific to your implementation
}*/


fn main() -> Result<(), Box<dyn Error>> {
    // Config and duration
    let config_path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "topologies/butterfly.toml".to_string());

    let simulation_duration = std::env::args()
        .nth(3)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);

    // Initialize network and shared channels
    let (controller_send, controller_recv, packet_recv, packet_send_map, config) =
        initialize_network_channels(&config_path)?;

    // Create drone implementations using the NetworkInitializer
    let drone_impls = NetworkInitializer::create_drone_implementations(
        &config,
        controller_send.clone(),
        controller_recv.clone(),
        packet_recv,
        packet_send_map,
    );

    // Initialize the network
    let mut initializer = NetworkInitializer::new(&config_path, drone_impls)?;
    initializer.initialize()?;
    println!("Network initialized successfully");


    // Decide mode
    if std::env::args().any(|arg| arg == "--headless") {
        println!("Running in headless mode");
        run_headless_simulation(simulation_duration);
    } else {
        println!("Starting GUI application");
        run_gui_application(controller_send,controller_recv, Arc::new(Mutex::new(config)))?;
    }

    Ok(())
}

fn run_headless_simulation(duration: u64) {
    println!("Running simulation for {} seconds", duration);
    std::thread::sleep(std::time::Duration::from_secs(duration));
    println!("Simulation completed");
}

fn run_gui_application(
    controller_send: Sender<DroneEvent>,
    _controller_recv: Receiver<DroneCommand>, // You can remove this if unused in GUI
    config: Arc<Mutex<ParsedConfig>>,
) -> Result<(), Box<dyn Error>> {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "Drone Simulation",
        options,
        Box::new(|cc| Ok(Box::new(NetworkApp::new_with_network(cc, controller_send, config)))),
    )?;

    Ok(())
}


//la funz seguente serve solo per displayare il network
fn initialize_network_channels(
    config_path: &str,
) -> Result<
    (
        Sender<DroneEvent>,
        Receiver<DroneCommand>,
        Receiver<Packet>,
        HashMap<NodeId, Sender<Packet>>,
        ParsedConfig,
    ),
    Box<dyn Error>,
> {
    let config = TOML_parser::parse_config(config_path)?;

    let (controller_send, _controller_event_recv) = crossbeam_channel::unbounded::<DroneEvent>();
    let (command_send, controller_recv) = crossbeam_channel::unbounded::<DroneCommand>();
    let (packet_send, packet_recv) = crossbeam_channel::unbounded::<Packet>();

    let packet_send_map: HashMap<NodeId, Sender<Packet>> = config
        .drone
        .iter()
        .enumerate()
        .map(|(i, _)| (NodeId::from(i as u8), packet_send.clone()))
        .collect();

    Ok((
        controller_send,
        controller_recv,
        packet_recv,
        packet_send_map,
        config,
    ))
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