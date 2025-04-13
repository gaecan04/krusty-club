use std::sync::{Arc, Mutex};
use eframe::{egui, App, CreationContext};
use eframe::egui::{Stroke, StrokeKind};
use egui::{Color32, RichText, Vec2, Rect, Sense, Shape};
use crate::network::initializer::ParsedConfig;
use crate::simulation_controller::network_designer::NetworkRenderer;
use std::thread;
use crossbeam_channel::{unbounded, Sender};
use crate::simulation_controller::SC_backend::SimulationController;
use wg_2024::controller::DroneEvent;
use wg_2024::network::NodeId;

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
                self.simulation_log.push(format!("Network has {} drones, {} clients, {} servers",
                                                 config.drone.len(), config.client.len(), config.server.len()));

                // Initialize the network renderer with the config
                self.network_renderer = Some(NetworkRenderer::new(topology, 50.0, 50.0));

                // Pass controller_send to NetworkRenderer if available
                if let (Some(renderer), Some(sender)) = (&mut self.network_renderer, &self.controller_send) {
                    renderer.set_controller_sender(sender.clone());
                    self.simulation_log.push("Controller sender connected to network renderer".to_string());
                }

                self.selected_topology = Some(topology.to_string());
                self.network_config = Some(Arc::new(Mutex::new(config)));
                self.topology_selected = true;
                self.state = AppState::Simulation;
            },
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
        // Use the simulation controller directly since we don't have a Crashed event
        if let Some(controller) = &self.simulation_controller {
            let mut controller = controller.lock().unwrap();

            match controller.crash_drone(drone_id) {
                Ok(_) => {
                    self.simulation_log.push(format!("Drone {} crashed successfully", drone_id));

                    // Update network renderer if needed
                    if let Some(renderer) = &mut self.network_renderer {
                        renderer.update_network();
                    }
                },
                Err(e) => {
                    self.simulation_log.push(format!("Failed to crash drone {}: {}", drone_id, e));
                }
            }
        }
    }

    // This function is kept for the UI but delegates to NetworkRenderer if possible
    fn set_packet_drop_rate(&mut self, drone_id: NodeId, rate: f32) {
        // Use the simulation controller directly since we don't have a PacketDropRateChanged event
        if let Some(controller) = &self.simulation_controller {
            let mut controller = controller.lock().unwrap();

            match controller.set_packet_drop_rate(drone_id, rate) {
                Ok(_) => {
                    self.simulation_log.push(format!("Set packet drop rate for drone {} to {}", drone_id, rate));

                    // Update network renderer if needed
                    if let Some(renderer) = &mut self.network_renderer {
                        renderer.update_network();
                    }
                },
                Err(e) => {
                    self.simulation_log.push(format!("Failed to set packet drop rate for drone {}: {}", drone_id, e));
                }
            }
        }
    }
    // Add function to spawn a new drone
    fn spawn_drone(&mut self, id: NodeId, pdr: f32, connections: Vec<NodeId>) {
        if let Some(controller) = &self.simulation_controller {
            let mut controller = controller.lock().unwrap();

            match controller.spawn_drone(id, pdr, connections) {
                Ok(_) => {
                    self.simulation_log.push(format!("Spawned new drone {} with PDR {}", id, pdr));

                    // Update network renderer if needed
                    if let Some(renderer) = &mut self.network_renderer {
                        renderer.update_network();
                    }
                },
                Err(e) => {
                    self.simulation_log.push(format!("Failed to spawn drone: {}", e));
                }
            }
        }
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
            .default_height(150.0)
            .show(ctx, |ui| {
                ui.heading("Simulation Log");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (index, log) in self.simulation_log.iter().enumerate() {
                        let color = if index % 2 == 0 { Color32::LIGHT_GRAY } else { Color32::GRAY };
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
                            // Update scale in network renderer
                            if let Some(renderer) = &mut self.network_renderer {
                                renderer.scale = self.zoom_level;
                            }
                        }
                        if ui.button("Zoom Out").clicked() {
                            self.zoom_level = (self.zoom_level / 1.2).max(0.5);
                            // Update scale in network renderer
                            if let Some(renderer) = &mut self.network_renderer {
                                renderer.scale = self.zoom_level;
                            }
                        }
                        ui.label(format!("Zoom: {:.1}x", self.zoom_level));
                    });

                    // Control panel for drone operations
                    ui.horizontal(|ui| {
                        // Flood Network Button
                        if ui.button("Flood Network").clicked() {
                            self.flood_network();
                        }

                        ui.add_space(10.0);

                        // Note added to inform about node clicking functionality
                        ui.label("(Click on nodes to modify properties)");

                        ui.add_space(10.0);

                        // Spawn drone button
                        if ui.button("Spawn Drone").clicked() {
                            self.show_spawn_drone_popup = true;
                        }
                    });

                    // Network view with zoom and pan
                    let (response, painter) = ui.allocate_painter(
                        ui.available_size(), // Use full available space
                        Sense::drag()
                    );

                    // Draw a border around the network view
                    painter.add(Shape::rect_stroke(response.rect, 2.0, Stroke::new(1.0, Color32::DARK_GRAY), StrokeKind::Inside));
                    // Handle panning
                    if response.dragged() {
                        self.pan_offset += response.drag_delta();
                    }

                    // Render network inside the allocated rectangle
                    if let Some(renderer) = &mut self.network_renderer {
                        // Create a clipping region
                        painter.clip_rect();

                        // Create a new rect for rendering, adjusted by zoom and pan
                        let center = response.rect.center();
                        let scaled_size = response.rect.size() * self.zoom_level;

                        let scaled_rect = Rect::from_center_size(
                            center + self.pan_offset,
                            scaled_size
                        );

                        // Temporary custom rendering (replace with actual renderer)
                        painter.add(Shape::rect_filled(
                            scaled_rect,
                            0.0,
                            Color32::TRANSPARENT
                        ));

                        // Render the actual network
                        renderer.render(ui);

                        // Render node details window (where PDR and crash functionality is)
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
                    ui.add(egui::DragValue::new(&mut self.new_drone_id)
                        .range(0..=100));

                    ui.label("Packet Drop Rate:");
                    ui.add(egui::Slider::new(&mut self.new_drone_pdr, 0.0..=1.0));

                    ui.label("Connections (comma separated):");
                    ui.text_edit_singleline(&mut self.new_drone_connections_str);

                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_spawn_drone_popup = false;
                        }

                        if ui.button("Spawn").clicked() {
                            // Parse connections string
                            let connections: Vec<NodeId> = self.new_drone_connections_str
                                .split(',')
                                .filter_map(|s| s.trim().parse().ok())
                                .collect();

                            self.spawn_drone(self.new_drone_id, self.new_drone_pdr, connections);
                            self.show_spawn_drone_popup = false;
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
    ) -> Self {
        // Create basic app using the main constructor
        let mut app = Self::new(cc);

        // Store the network configuration and controller sender
        app.network_config = Some(config.clone());
        app.controller_send = Some(controller_send.clone());

        // Create and store the simulation controller
        let (cmd_tx, cmd_rx) = unbounded();
        let controller = SimulationController::new(config.clone(), cmd_rx);
        let controller = Arc::new(Mutex::new(controller));
        app.simulation_controller = Some(controller.clone());

        // Start controller in a separate thread
        let controller_thread = thread::spawn(move || {
            let mut controller = controller.lock().unwrap();
            controller.run();
        });

        app.controller_thread = Some(controller_thread);

        // Initialize network renderer if applicable
        if let Some(topology) = app.selected_topology.clone() {
            app.network_renderer = Some(NetworkRenderer::new(&topology, 50.0, 50.0));

            // Pass controller_send to NetworkRenderer
            if let Some(renderer) = &mut app.network_renderer {
                renderer.set_controller_sender(controller_send);
                app.simulation_log.push("Controller sender connected to network renderer".to_string());
            }
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