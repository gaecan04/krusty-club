use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use eframe::{egui, App, CreationContext};
use eframe::egui::{Stroke, StrokeKind};
use egui::{Color32, RichText, Vec2, Rect, Sense, Shape};
use crate::network::initializer::{MyDrone, ParsedConfig};
use crate::simulation_controller::network_designer::NetworkRenderer;
use std::thread;
use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::debug_text::print;
use crate::simulation_controller::SC_backend::SimulationController;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use wg_2024::packet::Packet;

enum AppState {
    Welcome,
    Topology,
    Simulation,
}

enum Tab {
    NetworkView,
    Chat,
}

pub struct NetworkApp {
    state: AppState,
    current_tab: Tab,
    network_renderer: Option<NetworkRenderer>,
    topology_selected: bool,
    selected_topology: Option<String>,
    simulation_log: Vec<String>,
    chat_messages: Vec<String>,
    chat_input: String,
    is_simulation_running: bool,
    zoom_level: f32,
    pan_offset: Vec2,
    available_topologies: Vec<String>,
    network_config: Option<Arc<Mutex<ParsedConfig>>>,
    controller_send: Option<Sender<DroneEvent>>,
    simulation_controller: Option<Arc<Mutex<SimulationController>>>,
    controller_thread: Option<thread::JoinHandle<()>>,
    // Fields for drone operations (mainly for separate UI elements)
    selected_drone_id: NodeId,
    pdr_drone_id: NodeId,
    pdr_value: f32,
    show_spawn_drone_popup: bool,
    new_drone_id: NodeId,
    new_drone_pdr: f32,
    new_drone_connections_str: String,
}

impl NetworkApp {
    // Constructor to accept creation context
    pub fn new(_cc: &CreationContext) -> Self {
        // You can access persistent state or other context info here if needed
        let mut app = Self::default();

        // Scan the topologies directory to find available configuration files
        if let Ok(entries) = std::fs::read_dir("topologies") {
            for entry in entries.flatten() {
                if let Some(file_name) = entry.file_name().to_str() {
                    if file_name.ends_with(".toml") {
                        let topology_name = file_name.trim_end_matches(".toml").to_string();
                        app.available_topologies.push(topology_name);
                    }
                }
            }
        }

        // Log available topologies
        app.simulation_log.push("Application started".to_string());
        app.simulation_log.push(format!("Found {} topology configurations", app.available_topologies.len()));

        app
    }

    fn set_topology(&mut self, topology: &str) {
        // Attempt to load and parse the network configuration
        let config_path = format!("topologies/{}.toml", topology);

        match crate::network::TOML_parser::parse_config(&config_path) {
            Ok(config) => {
                // Log success and network stats
                self.simulation_log.push(format!("Loaded topology '{}'", topology));
                self.simulation_log.push(format!(
                    "Network has {} drones, {} clients, {} servers",
                    config.drone.len(),
                    config.client.len(),
                    config.server.len()
                ));

                // Detect topology type and log it
                let config_arc = Arc::new(Mutex::new(config));
                if let Some(topology_name) = config_arc.lock().unwrap().detect_topology() {
                    self.simulation_log.push(format!("Detected topology: {}", topology_name));
                } else {
                    self.simulation_log.push("Could not detect known topology (may be custom)".to_string());
                }

                // Store the config
                self.network_config = Some(config_arc.clone());

                // Initialize the network renderer with the config
                self.network_renderer = Some(NetworkRenderer::new_from_config(topology, 50.0, 50.0, config_arc.clone()));

                // Pass controller_send to NetworkRenderer if available
                if let (Some(renderer), Some(sender)) = (&mut self.network_renderer, &self.controller_send) {
                    renderer.set_controller_sender(sender.clone());
                    self.simulation_log.push("Controller sender connected to network renderer".to_string());
                }

                // Pass controller to the renderer if available
                if let (Some(renderer), Some(controller)) = (&mut self.network_renderer, &self.simulation_controller) {
                    renderer.set_simulation_controller(controller.clone());
                    self.simulation_log.push("Simulation controller connected to network renderer".to_string());
                }

                self.selected_topology = Some(topology.to_string());
                self.topology_selected = true;
                self.state = AppState::Simulation;
            }
            Err(e) => {
                // Log error
                self.simulation_log.push(format!("Failed to load topology '{}': {}", topology, e));
            }
        }
    }

    fn flood_network(&mut self) {
        self.simulation_log.push("Flood Network initiated".to_string());

        // If you have the network initialized, send flood request to servers and clients
        if let Some(config) = &self.network_config {
            let config = config.lock().unwrap();

            // Example: Send flood requests to servers and clients
            // This is a simplified example - you would need to implement
            // the actual flood request sending logic

            for server in &config.server {
                self.simulation_log.push(format!("Sending flood request to server {}", server.id));
                // Call server flood request function
            }

            for client in &config.client {
                self.simulation_log.push(format!("Sending flood request to client {}", client.id));
                // Call client flood request function
            }

            self.simulation_log.push("Network flood completed".to_string());
        }
    }

    // This function is kept for the UI but delegates to NetworkRenderer if possible
    fn crash_drone(&mut self, drone_id: NodeId) {
        if let Some(ctrl_arc) = &self.simulation_controller {
            let mut ctrl = ctrl_arc.lock().unwrap();

            match ctrl.crash_drone(drone_id) {
                Ok(_) => {
                    self.simulation_log.push(format!("✅ Drone {} crashed successfully", drone_id));

                    // ✅ Only now update the visual state
                    if let Some(renderer) = &mut self.network_renderer {
                        if let Some(&idx) = renderer.node_id_to_index.get(&drone_id) {
                            renderer.nodes[idx].active = false;
                            renderer.remove_edges_of_crashed_node(idx);
                        }
                        renderer.sync_connections_with_config(); // Optional
                    }
                }
                Err(e) => {
                    self.simulation_log.push(format!("SC refused to crash {}: {}", drone_id, e));

                    // 🚫 Do NOT update the renderer here!
                }
            }
        }
    }

    // This function is kept for the UI but delegates to NetworkRenderer if possible
    fn set_packet_drop_rate(&mut self, drone_id: NodeId, rate: f32) {
        // 1) tell the SC
        if let Some(ctrl_arc) = &self.simulation_controller {
            let mut ctrl = ctrl_arc.lock().unwrap();
            if let Err(e) = ctrl.set_packet_drop_rate(drone_id, rate) {
                self.simulation_log.push(format!("Failed to set PDR {}→{}: {}", drone_id, rate, e));
                return;
            }
            self.simulation_log.push(format!("Set PDR for drone {} to {}", drone_id, rate));
        }

        // 2) persist in config
        if let Some(cfg_arc) = &self.network_config {
            let mut cfg = cfg_arc.lock().unwrap();
            cfg.set_drone_pdr(drone_id, rate);
        }

        // 3) no need for full rebuild—just sync positions & statuses
        if let Some(renderer) = &mut self.network_renderer {
            renderer.sync_with_simulation_controller();
        }
    }

    fn add_connection(&mut self, a: NodeId, b: NodeId) {
        // 1) ask the SC to wire up channels both ways
        if let Some(ctrl_arc) = &self.simulation_controller {
            let mut ctrl = ctrl_arc.lock().unwrap();
            // You’ll need to expose a method on SC like `connect(a, b)`
            if let Err(e) = ctrl.add_link(a, b) {
                self.simulation_log.push(format!("SC refused link {}↔{}: {}", a, b, e));
                return;
            }
        }

        // 2) update config
        if let Some(cfg_arc) = &self.network_config {
            let mut cfg = cfg_arc.lock().unwrap();
            // add b to a’s list
            if let Some(dr) = cfg.drone.iter_mut().find(|d| d.id == a) {
                if !dr.connected_node_ids.contains(&b) {
                    dr.connected_node_ids.push(b);
                }
            }
            // and a to b’s list
            if let Some(dr) = cfg.drone.iter_mut().find(|d| d.id == b) {
                if !dr.connected_node_ids.contains(&a) {
                    dr.connected_node_ids.push(a);
                }
            }
        }

        // 3) rebuild GUI
        if let (Some(r), Some(cfg_arc)) = (&mut self.network_renderer, &self.network_config) {
            r.build_from_config(cfg_arc.clone());
        }

        self.simulation_log.push(format!("🔗 Connected {} ↔ {}", a, b));
    }
    // Add function to spawn a new drone
    // in your GUI-side spawn_drone
    fn spawn_drone(&mut self, id: NodeId, pdr: f32, connections: Vec<NodeId>) {
        // 1) SC must approve & create channels
        if let Some(ctrl_arc) = &self.simulation_controller {
            let mut ctrl = ctrl_arc.lock().unwrap();
            if let Err(e) = ctrl.spawn_drone(id, pdr, connections.clone()) {
                self.simulation_log.push(format!("SC refused spawn: {}", e));
                return;
            }
        }

        // 2) update the shared TOML config
        if let Some(cfg_arc) = &self.network_config {
            let mut cfg = cfg_arc.lock().unwrap();
            cfg.add_drone(id);
            cfg.set_drone_pdr(id, pdr);
            cfg.set_drone_connections(id, connections.clone());
            // make each peer bidirectional:
            for &peer in &connections {
                if let Some(dr) = cfg.drone.iter_mut().find(|d| d.id == peer) {
                    if !dr.connected_node_ids.contains(&id) {
                        dr.connected_node_ids.push(id);
                    }
                }
            }
        }

        // 3) rebuild the entire renderer
        if let (Some(renderer), Some(cfg_arc)) = (&mut self.network_renderer, &self.network_config) {
            renderer.build_from_config(cfg_arc.clone());
        }

        self.simulation_log.push(format!("🎉 Spawned drone {}", id));

    }
    fn render_simulation_tabs(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("tabs_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Network View").clicked() {
                    self.current_tab = Tab::NetworkView;
                }
                if ui.button("Chat").clicked() {
                    self.current_tab = Tab::Chat;
                }
            });
        });

        match self.current_tab {
            Tab::NetworkView => self.render_network_view(ctx),
            Tab::Chat => self.render_chat_view(ctx),
        }
    }

    fn render_network_view(&mut self, ctx: &egui::Context) {
        // Bottom panel for simulation log with increased height
        egui::TopBottomPanel::bottom("simulation_log")
            .resizable(true)
            .default_height(120.0)
            .min_height(60.0)
            .max_height(150.0)
            .show(ctx, |ui| {
                ui.heading("Simulation Log");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (idx, log) in self.simulation_log.iter().enumerate() {
                        let color = if idx % 2 == 0 { Color32::LIGHT_GRAY } else { Color32::GRAY };
                        ui.colored_label(color, log);
                    }
                });
            });

        // Central panel for network rendering
        egui::CentralPanel::default().show(ctx, |ui| {
            // Frame for network view with border
            egui::Frame::default()
                .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                .show(ui, |ui| {
                    // Display current topology and zoom controls
                    ui.horizontal(|ui| {
                        if let Some(ref topology) = self.selected_topology {
                            ui.label(format!("Current Topology: {}", topology));
                        }
                        ui.add_space(20.0);
                        if ui.button("Zoom In").clicked() {
                            self.zoom_level = (self.zoom_level * 1.2).min(3.0);
                            if let Some(renderer) = &mut self.network_renderer {
                                renderer.scale = self.zoom_level;
                            }
                        }
                        if ui.button("Zoom Out").clicked() {
                            self.zoom_level = (self.zoom_level / 1.2).max(0.5);
                            if let Some(renderer) = &mut self.network_renderer {
                                renderer.scale = self.zoom_level;
                            }
                        }
                        ui.label(format!("Zoom: {:.1}x", self.zoom_level));
                    });

                    // Control panel for drone operations
                    ui.horizontal(|ui| {
                        if ui.button("Sync Network").clicked() {
                            if let Some(renderer) = &mut self.network_renderer {
                                renderer.sync_with_simulation_controller();
                                self.simulation_log.push("Network synchronized with controller".into());
                            }
                        }
                        ui.add_space(10.0);
                        if ui.button("Flood Network").clicked() {
                            self.flood_network();
                        }
                        ui.add_space(10.0);
                        ui.label("(Click on nodes to modify properties)");
                        ui.add_space(10.0);
                        if ui.button("Spawn Drone").clicked() {
                            self.show_spawn_drone_popup = true;
                        }
                    });

                    // Network view with zoom and pan
                    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::drag());
                    painter.add(Shape::rect_stroke(
                        response.rect,
                        2.0,
                        Stroke::new(1.0, Color32::DARK_GRAY),
                        StrokeKind::Inside,
                    ));
                    if response.dragged() {
                        self.pan_offset += response.drag_delta();
                    }
                    if let Some(renderer) = &mut self.network_renderer {
                        painter.clip_rect();
                        let center = response.rect.center();
                        let scaled_size = response.rect.size() * self.zoom_level;
                        let scaled_rect = Rect::from_center_size(center + self.pan_offset, scaled_size);
                        painter.add(Shape::rect_filled(scaled_rect, 0.0, Color32::TRANSPARENT));
                        renderer.render(ui);
                        renderer.render_node_details(ctx);
                    }
                });
        });

        // Spawn drone popup
        if self.show_spawn_drone_popup {
            egui::Window::new("Spawn New Drone")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Drone ID:");
                    ui.add(egui::DragValue::new(&mut self.new_drone_id).range(0..=100));

                    ui.label("Packet Drop Rate:");
                    ui.add(egui::Slider::new(&mut self.new_drone_pdr, 0.0..=1.0));

                    ui.label("Connections (comma separated):");
                    ui.text_edit_singleline(&mut self.new_drone_connections_str);

                    // available‐nodes foldout…
                    if let Some(renderer) = &self.network_renderer {
                        ui.collapsing("Available Nodes", |ui| {
                            ui.label(format!("Drone IDs: {:?}", renderer.get_drone_ids()));
                            ui.label(format!("Client IDs: {:?}", renderer.get_client_ids()));
                            ui.label(format!("Server IDs: {:?}", renderer.get_server_ids()));
                        });
                    }

                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_spawn_drone_popup = false;
                        }
                        if ui.button("Spawn").clicked() {
                            // 1) parse
                            let connections: Vec<NodeId> = self
                                .new_drone_connections_str
                                .split(',')
                                .filter_map(|s| s.trim().parse().ok())
                                .collect();

                            // 2) validate
                            let already_exists = self
                                .network_renderer
                                .as_ref()
                                .and_then(|r| r.node_id_to_index.get(&self.new_drone_id).cloned())
                                .is_some();
                            let bad_conn = connections.iter().any(|&id| {
                                self.network_renderer
                                    .as_ref()
                                    .map(|r| !r.node_id_to_index.contains_key(&id))
                                    .unwrap_or(true)
                            });

                            if already_exists {
                                self.simulation_log
                                    .push(format!("Cannot spawn: ID {} already in use", self.new_drone_id));
                            } else if connections.is_empty() {
                                self.simulation_log
                                    .push("Cannot spawn: need at least one connection".into());
                            } else if bad_conn {
                                self.simulation_log
                                    .push("Cannot spawn: invalid connection IDs".into());
                            } else {
                                // 3) spawn + rebuild
                                self.spawn_drone(self.new_drone_id, self.new_drone_pdr, connections.clone());
                                self.show_spawn_drone_popup = false;
                                self.new_drone_connections_str.clear();
                                ctx.request_repaint();
                            }
                        }
                    });
                });
        }

    }

    fn render_welcome_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(100.0);

                // Club Name (Large, Prominent)
                ui.label(RichText::new("Drone Network Simulation Club")
                    .color(Color32::BLUE)
                    .size(30.0));

                ui.add_space(50.0);

                // Buttons with some spacing and styling
                ui.vertical(|ui| {
                    if ui.button("Start Simulation").clicked() {
                        self.state = AppState::Topology;
                    }

                    ui.add_space(20.0);

                    if ui.button("Close Application").clicked() {
                        // TODO: Implement proper application closure
                        std::process::exit(0);
                    }
                });
            });
        });
    }

    fn render_topology_selection(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Select Network Topology");
                ui.separator();

                ui.add_space(50.0);

                ui.horizontal(|ui| {
                    // Star Topology Button
                    if ui.button("Star Topology").clicked() {
                        self.set_topology("star");
                    }

                    // Double Line Topology Button
                    if ui.button("Double Line Topology").clicked() {
                        self.set_topology("double_line");
                    }

                    // Butterfly Topology Button
                    if ui.button("Butterfly Topology").clicked() {
                        self.set_topology("butterfly");
                    }
                });

                ui.add_space(20.0);

                // Back to Welcome Button
                if ui.button("Back to Welcome").clicked() {
                    self.state = AppState::Welcome;
                }
            });
        });
    }

    fn render_chat_view(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // Chat messages display
            egui::ScrollArea::vertical().show(ui, |ui| {
                for message in &self.chat_messages {
                    ui.label(message);
                }
            });

            // Chat input
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.chat_input);
                if ui.button("Send").clicked() && !self.chat_input.is_empty() {
                    self.chat_messages.push(self.chat_input.clone());
                    self.chat_input.clear();
                }
            });
        });
    }
    pub fn new_with_network(
        cc: &eframe::CreationContext<'_>,
        controller_send: Sender<DroneEvent>,
        config: Arc<Mutex<ParsedConfig>>,
        arc: Arc<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>, Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32) -> Box<dyn Drone> + Send + Sync>,
    ) -> Self {
        let mut app = Self::new(cc);

        app.network_config = Some(config.clone());
        app.controller_send = Some(controller_send.clone());

        // Create drone_factory closure
        let drone_factory = Arc::new(
            |id, controller_send, controller_recv, packet_recv, packet_send, pdr| {
                Box::new(MyDrone::new(id, controller_send, controller_recv, packet_recv, packet_send, pdr)) as Box<dyn Drone>
            }
        );

        // Create Simulation Controller
        let (event_tx, event_rx) = unbounded::<DroneEvent>(); // GUI won't use this receiver directly
        let controller = SimulationController::new(config.clone(),controller_send.clone(), event_rx, drone_factory);
        let controller = Arc::new(Mutex::new(controller));
        app.simulation_controller = Some(controller.clone());

        // Start controller in background thread
        let controller_clone = controller.clone();
        let controller_thread = thread::spawn(move || {
            let mut ctrl = controller_clone.lock().unwrap();
            ctrl.run();
        });

        app.controller_thread = Some(controller_thread);

        // Init GUI renderer
        app.network_renderer = Some(NetworkRenderer::new_from_config("custom", 50.0, 50.0, config.clone()));
        if let Some(renderer) = &mut app.network_renderer {
            renderer.set_controller_sender(controller_send);
            renderer.set_simulation_controller(controller.clone());
            app.simulation_log.push("Controller connected to network renderer".to_string());
        }

        app
    }


}

impl Default for NetworkApp {
    fn default() -> Self {
        Self {
            state: AppState::Welcome,
            current_tab: Tab::NetworkView,
            network_renderer: None,
            topology_selected: false,
            selected_topology: None,
            simulation_log: Vec::new(),
            chat_messages: Vec::new(),
            chat_input: String::new(),
            is_simulation_running: false,
            zoom_level: 1.0,
            pan_offset: Vec2::ZERO,
            available_topologies: Vec::new(),
            network_config: None,
            controller_send: None,
            simulation_controller: None,
            controller_thread: None,
            selected_drone_id: 0,
            pdr_drone_id: 0,
            pdr_value: 0.0,
            show_spawn_drone_popup: false,
            new_drone_id: 0,
            new_drone_pdr: 0.0,
            new_drone_connections_str: String::new(),
        }
    }
}

impl eframe::App for NetworkApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        match self.state {
            AppState::Welcome => self.render_welcome_screen(ctx),
            AppState::Topology => self.render_topology_selection(ctx),
            AppState::Simulation => self.render_simulation_tabs(ctx),
        }
    }
}