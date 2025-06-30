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
    println!("🚀 Starting main()");

    env_logger::init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "topologies/default.toml".to_string());

    let simulation_duration = std::env::args()
        .nth(3)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);

    let (event_sender, event_receiverr) = unbounded::<DroneEvent>();
    let (command_sender, command_receiver) = unbounded::<DroneCommand>();

    println!("✅ Channels created");

    let gui_input_queue = new_gui_input_queue();
    let simulation_log = Arc::new(Mutex::new(Vec::new()));

    let config = TOML_parser::parse_config(&config_path)?;
    println!("✅ Parsed config from {}", config_path);

    let parsed_config = Arc::new(Mutex::new(config.clone()));

    let drone_factory = Arc::new(
        |id, controller_send, controller_recv, packet_recv, packet_send, pdr| {
            println!("🔧 Creating MyDrone {}", id);
            Box::new(MyDrone::new(
                id,
                controller_send,
                controller_recv,
                packet_recv,
                packet_send,
                pdr,
            )) as Box<dyn Drone>
        },
    );

    let shared_senders: Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>> =
        Arc::new(Mutex::new(HashMap::new()));



    /*let command_senders: Arc<Mutex<HashMap<NodeId, Sender<DroneCommand>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    for id in config.drone.iter().map(|d| d.id) {
        command_senders.lock().unwrap().insert(id, command_sender.clone());
    }*/


    let initializer = Arc::new(Mutex::new(NetworkInitializer::new(
        &config_path,
        vec![],
        simulation_log.clone(),
        shared_senders.clone(),
    )?));
    println!("✅ NetworkInitializer created");

    let (receivers, senders,event_receiver) = initializer.lock().unwrap().setup_channels();
    println!("✅ setup_channels() completed");




    let packet_receivers = Arc::new(Mutex::new(receivers));
    let packet_senders = Arc::new(Mutex::new(senders));

    let shared_senders = initializer.lock().unwrap().shared_senders.clone().expect("Shared senders not initialized");

    let command_senders = initializer.lock().unwrap().command_senders.clone();

    let mut host_senders = HashMap::new();
    let mut host_receivers = HashMap::new();

    for id in config.client.iter().map(|c| c.id).chain(config.server.iter().map(|s| s.id)) {
        let (sc_to_host_tx, sc_to_host_rx) = unbounded::<Packet>();
        host_senders.insert(id, sc_to_host_tx);
        host_receivers.insert(id, sc_to_host_rx);
    }



    let controller = Arc::new(Mutex::new(SimulationController::new(
        parsed_config.clone(),
        event_sender.clone(),
        command_sender.clone(),
        drone_factory.clone(),
        gui_input_queue.clone(),
        initializer.clone(),
        packet_senders.clone(),    // ✅ shared
        packet_receivers.clone(),  // ✅ shared
        command_senders.clone(),
        host_senders.clone(),
        shared_senders.clone(),
    )));

    println!("✅ SimulationController created");

    initializer.lock().unwrap().set_controller(controller.clone());
    println!("✅ Controller injected into initializer");

    // 🔁 Now use the shared ones for drone creation
    let drone_impls = initializer.lock().unwrap().create_drone_implementations();

    println!("✅ Drone implementations created");

    initializer.lock().unwrap().drone_impls = drone_impls;

    println!("⏳ Calling initializer.initialize()");
    initializer.lock().unwrap().initialize(gui_input_queue.clone(),host_receivers.clone())?;
    println!("✅ initializer.initialize() completed");

    SimulationController::start_background_thread(controller.clone(), event_receiver.clone());
    println!("✅ Background thread started");

    println!("🖥️ Starting GUI");
    run_gui_application(
        event_sender.clone(),
        //event_receiver.clone(),
        command_sender.clone(),
        command_receiver.clone(),
        parsed_config,
        drone_factory,
        &config_path,
        gui_input_queue.clone(),
        simulation_log.clone(),
        packet_senders.clone(),
        packet_receivers.clone(),
        command_senders.clone(),
        shared_senders.clone(),
        host_senders.clone(),
    )?;
    println!("✅ GUI exited cleanly");

    Ok(())
}

fn run_gui_application(
    event_sender: Sender<DroneEvent>,
    //event_receiver: Receiver<DroneEvent>,
    command_sender: Sender<DroneCommand>,
    command_receiver: Receiver<DroneCommand>,
    config: Arc<Mutex<ParsedConfig>>,
    drone_factory: Arc<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>, Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32) -> Box<dyn Drone> + Send + Sync>,
    config_path: &str,
    gui_input_queue: SharedGuiInput,
    simulation_log: Arc<Mutex<Vec<String>>>,
    packet_senders: Arc<Mutex<HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>>>,
    packet_receivers: Arc<Mutex<HashMap<NodeId, Receiver<Packet>>>>,
    command_senders: Arc<Mutex<HashMap<NodeId, Sender<DroneCommand>>>>, // ✅ ADD THIS
    // ✅ ADD THIS TOO
    shared_senders: Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>,
    host_senders: HashMap<NodeId, Sender<Packet>>, // ✅ sc->hosts

) -> Result<(), Box<dyn Error>> {

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Drone Simulation",
        options,
        Box::new(|cc| {
            Ok(Box::new(NetworkApp::new_with_network(
                cc,
                event_sender.clone(),
                //event_receiver.clone(),
                command_sender.clone(),
                command_receiver.clone(),
                config.clone(),
                drone_factory.clone(),
                config_path,
                gui_input_queue.clone(),
                simulation_log.clone(),
                packet_senders.clone(),
                packet_receivers.clone(),
                command_senders.clone(), // ✅ ADD THIS TOO
                shared_senders.clone(),
                host_senders.clone(),
            )))

        }),
    )?;
    Ok(())
}
