mod utils;
mod network;
mod nodes;
mod gui;
mod simulation_controller;

use std::thread;
use crate::gui::MyApp;
/*
fn main() {
    // Load the network configuration from the `.toml` file
    let config_path = "path/to/network_config.toml";
    let network_config = match config::NetworkConfig::from_file(config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error loading network configuration: {}", e);
            return;
        }

    };

    println!("Network configuration loaded successfully!");

    // run the egui
    println!("Launching GUI...");
    // Call the GUI logic directly on the main thread
    if let Err(e) = gui::run() {
        eprintln!("Failed to launch GUI: {}", e);
    }

    // Start the Network Initializer in its own thread
    let network_handle = thread::spawn(move || {
        println!("Initializing network...");
        match network::initializer::initialize_network(network_config) {
            Ok(_) => println!("Network initialized successfully!"),
            Err(e) => eprintln!("Error initializing network: {}", e),
        }
    });

    // Start the Simulation Controller in its own thread
    let simulation_handle = thread::spawn(|| {
        println!("Starting Simulation Controller...");
        match network::simulation::run_simulation_controller() {
            Ok(_) => println!("Simulation Controller finished!"),
            Err(e) => eprintln!("Error in Simulation Controller: {}", e),
        }
    });

    // Wait for all threads to complete
    if let Err(e) = gui_handle.join() {
        eprintln!("Error in GUI thread: {:?}", e);
    }

    if let Err(e) = network_handle.join() {
        eprintln!("Error in Network Initializer thread: {:?}", e);
    }

    if let Err(e) = simulation_handle.join() {
        eprintln!("Error in Simulation Controller thread: {:?}", e);
    }

    println!("All threads have completed. Exiting program.");
}

 */


fn main() {

}