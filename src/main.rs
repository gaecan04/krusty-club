mod network;
mod nodes;
mod simulation_controller;
use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::thread;
use crossbeam_channel::{Receiver, Sender, unbounded};
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use wg_2024::packet::Packet;
use simulation_controller::app::NetworkApp;
use simulation_controller::SC_backend::SimulationController;
use crate::network::TOML_parser;
use crate::network::initializer::{DroneImplementation, MyDrone, NetworkInitializer, ParsedConfig};
use crate::simulation_controller::gui_input_queue::{push_gui_message, new_gui_input_queue, SharedGuiInput};

fn main() -> Result<(), Box<dyn Error>> {


    env_logger::init();
    // Config and duration
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "topologies/default.toml".to_string());

    let simulation_duration = std::env::args()
        .nth(3)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);

    let (event_sender, event_receiver) = unbounded::<DroneEvent>();
    let (command_sender, command_receiver) = unbounded::<DroneCommand>();

    let gui_input_queue = new_gui_input_queue();

    println!("🔗 GUI_INPUT addr (main): {:p}", &*gui_input_queue.lock().unwrap());

    let config = TOML_parser::parse_config(&config_path)?;
    let parsed_config = Arc::new(Mutex::new(config.clone()));

    // ✅ Create drone_factory closure => Required by updated SC constructor
    let drone_factory = Arc::new(
        |id, controller_send, controller_recv, packet_recv, packet_send, pdr| {
            Box::new(MyDrone::new(
                id,
                controller_send,
                controller_recv,
                packet_recv,
                packet_send,
                pdr,
            )) as Box<dyn Drone>
        }
    );

    // Create Simulation Controller with factory
    let controller = Arc::new(Mutex::new(SimulationController::new(
        parsed_config.clone(),
        event_sender.clone(),
        command_sender.clone(),
        drone_factory.clone(),              // <- Correct type and position
        gui_input_queue.clone(),
    )));


    let mut initializer = NetworkInitializer::new(
        &config_path,
        vec![], // temporary, you'll assign drone_impls after setup_channels()
        controller.clone(),
    )?;

   initializer.setup_channels();

    // Create drone implementations using the NetworkInitializer
    let drone_impls = NetworkInitializer::create_drone_implementations(
        &config,
        event_sender.clone(),
        command_receiver.clone(),
        &initializer.packet_receivers,
        &initializer.packet_senders,
    );



    initializer.drone_impls = drone_impls;

    initializer.initialize(gui_input_queue.clone())?;
    println!("Network initialized successfully");

    println!("Starting GUI application");
    SimulationController::start_background_thread(controller.clone(),event_receiver.clone());
    run_gui_application(
        event_sender.clone(),
        event_receiver.clone(),
        command_sender.clone(),
        command_receiver.clone(),
        parsed_config,
        drone_factory,
        &config_path,
        gui_input_queue.clone(),
    )?;

    Ok(())
}

/*fn run_headless_simulation(
    duration: u64,
    _config: Arc<Mutex<ParsedConfig>>,
    controller: Arc<Mutex<SimulationController>>,
) {
    println!("Running simulation for {} seconds", duration);

    let controller_clone = controller.clone();
    thread::spawn(move || {
        let mut controller = controller_clone.lock().unwrap();
        controller.run();
    });

    std::thread::sleep(std::time::Duration::from_secs(duration));
    println!("Simulation completed");
}*/

fn run_gui_application(
    event_sender: Sender<DroneEvent>,
    event_receiver: Receiver<DroneEvent>,
    command_sender: Sender<DroneCommand>,
    command_receiver: Receiver<DroneCommand>,
    config: Arc<Mutex<ParsedConfig>>,
    drone_factory: Arc<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>, Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32) -> Box<dyn Drone> + Send + Sync>,
    config_path: &str,
    gui_input_queue: SharedGuiInput,
) -> Result<(), Box<dyn Error>> {
    let options = eframe::NativeOptions::default();


    eframe::run_native(
        "Drone Simulation",
        options,
        Box::new(|cc| {
            Ok(Box::new(NetworkApp::new_with_network(
                cc,
                event_sender.clone(),
                event_receiver.clone(),
                command_sender.clone(),
                command_receiver.clone(),
                config.clone(),
                drone_factory.clone(),
                config_path,
                gui_input_queue.clone(),
                )))
        }),
    )?;
    Ok(())

}

/*fn initialize_network_channels(
    config_path: &str,
) -> Result<
    (
        Sender<DroneEvent>,
        Receiver<DroneEvent>,
        Sender<DroneCommand>,
        Receiver<DroneCommand>,
        Receiver<Packet>,
        Arc<HashMap<NodeId, Sender<Packet>>>,
        ParsedConfig,
    ),
    Box<dyn Error>,
> {
    let config = TOML_parser::parse_config(config_path)?;

    let (event_sender, event_receiver) = unbounded::<DroneEvent>();
    let (command_sender, command_receiver) = unbounded::<DroneCommand>();
    let (packet_send, packet_recv) = unbounded::<Packet>();

    let mut packet_send_map = HashMap::new();
    for node in config.drone.iter().map(|d| d.id)
        .chain(config.client.iter().map(|c| c.id))
        .chain(config.server.iter().map(|s| s.id))
    {
        packet_send_map.insert(node, packet_send.clone());
    }

    Ok((
        event_sender,
        event_receiver,
        command_sender,
        command_receiver,
        packet_recv,
        Arc::new(packet_send_map),
        config,
    ))
}
*/



