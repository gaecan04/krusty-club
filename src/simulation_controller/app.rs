use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use eframe::{egui, CreationContext};
use eframe::egui::{Stroke, StrokeKind};
use egui::{Color32, RichText, Vec2, Rect, Sense, Shape, Pos2};
use crate::network::initializer::{NetworkInitializer, ParsedConfig};
use crate::simulation_controller::network_designer::NetworkRenderer;
use std::thread;
use crossbeam_channel::{ Receiver, Sender};
use petgraph::visit::Walker;
use crate::simulation_controller::SC_backend::SimulationController;
use wg_2024::controller::{DroneCommand, DroneEvent};
use wg_2024::drone::Drone;
use wg_2024::network::NodeId;
use wg_2024::packet::Packet;
use crate::simulation_controller::chatUI::{ ChatUIState, ClientStatus};
use crate::simulation_controller::gui_input_queue::{ SharedGuiInput};

enum AppState {
    Welcome,
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
    simulation_log: Arc<Mutex<Vec<String>>>,
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

    selected_drone_id: NodeId,
    pdr_drone_id: NodeId,
    pdr_value: f32,
    show_spawn_drone_popup: bool,
    new_drone_id: NodeId,
    new_drone_pdr: f32,
    new_drone_connections_str: String,
    chat_ui:ChatUIState,
    packet_senders: HashMap<NodeId, Sender<Packet>>,

    show_shared_senders_popup:bool,
    shared_senders: Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>,
    host_senders: HashMap<NodeId, Sender<Packet>>, // ✅ the sc-hosts hashmap


}

impl NetworkApp {
    pub fn new( _cc: &CreationContext) -> Self {
        let mut app = Self::default();

        // we have to put the toml file inside topologies folder
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

        app.log("Application started".to_string());
        app.log(format!("Found {} topology configurations", app.available_topologies.len()));

        app
    }

    fn log(&self, message: impl ToString) {

        if let Ok(mut log) = self.simulation_log.lock() {
            log.push(message.to_string());
        }
    }



    // This function is kept for the UI but delegates to NetworkRenderer if possible
    fn crash_drone(&mut self, drone_id: NodeId) {
        if let Some(ctrl_arc) = &self.simulation_controller {
            let mut ctrl = ctrl_arc.lock().unwrap();
            match ctrl.crash_drone(drone_id) {
                Ok(_) => {
                    self.log(format!("✅ Drone {} crashed successfully", drone_id));

                    if let Some(renderer) = &mut self.network_renderer {
                        renderer.sync_with_simulation_controller();
                        if let Some(cfg_arc) = &self.network_config {
                            renderer.build_from_config(cfg_arc.clone());
                        }
                    }
                }
                Err(e) => {
                    self.log(format!("SC refused to crash {}: {}", drone_id, e));

                }
            }
        }
    }

    fn set_packet_drop_rate(&mut self, drone_id: NodeId, rate: f32) {
        // 1) tell the SC
        if let Some(ctrl_arc) = &self.simulation_controller {
            let mut ctrl = ctrl_arc.lock().unwrap();
            if let Err(e) = ctrl.set_packet_drop_rate(drone_id, rate) {
               self.log(format!("Failed to set PDR {}→{}: {}", drone_id, rate, e));
                return;
            }
           self.log(format!("Set PDR for drone {} to {}", drone_id, rate));
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

    fn spawn_drone(&mut self, id: NodeId, pdr: f32, connections: Vec<NodeId>) {
        // 1. Ask the Simulation Controller to spawn (MUST happen first)
        if let Some(ctrl_arc) = &self.simulation_controller {
            let mut ctrl = ctrl_arc.lock().unwrap();
            if let Err(e) = ctrl.spawn_drone(id, pdr, connections.clone()) {
               self.log(format!("SC refused spawn: {}", e));
                return;
            }
        }

        // 2. Update Config
        if let Some(cfg_arc) = &self.network_config {
            let mut cfg = cfg_arc.lock().unwrap();

            if cfg.drone.iter().any(|d| d.id == id) {
               self.log(format!("⚠️ Warning: Drone {} already present in config, updating.", id));
            } else {
                cfg.add_drone(id);
            }

            cfg.set_drone_pdr(id, pdr);
            cfg.set_drone_connections(id, connections.clone());

            for &peer in &connections {
                if let Some(dr) = cfg.drone.iter_mut().find(|d| d.id == peer) {
                    if !dr.connected_node_ids.contains(&id) {
                        dr.connected_node_ids.push(id);
                    }
                }
            }
        }

        // 3. Rebuild the renderer
        if let (Some(renderer), Some(cfg_arc)) = (&mut self.network_renderer, &self.network_config) {
            renderer.rebuild_preserving_topology(cfg_arc.clone());
        }

       self.log(format!("🎉 Spawned drone {}", id));
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

    fn render_welcome_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let available_height = ui.available_height();
            let available_width = ui.available_width();

            // Center everything
            ui.allocate_ui_at_rect(
                egui::Rect::from_center_size(
                    egui::Pos2::new(available_width / 2.0, available_height / 2.0),
                    egui::Vec2::new(300.0, 300.0),
                ),
                |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);

                        ui.label(
                            RichText::new("Krusty Club")
                                .color(Color32::DARK_GRAY)
                                .size(32.0)
                                .strong(),
                        );
                        ui.add_space(40.0);

                        ui.label(
                            RichText::new("Drone Network Simulation")
                                .color(Color32::DARK_BLUE)
                                .size(32.0)
                                .strong(),
                        );

                        ui.add_space(40.0);

                        if ui
                            .add_sized([200.0, 40.0], egui::Button::new("▶ Start Simulation"))
                            .clicked()
                        {
                            self.state = AppState::Simulation;
                        }

                        ui.add_space(20.0);

                        if ui
                            .add_sized([200.0, 40.0], egui::Button::new("⛔ Close Application"))
                            .clicked()
                        {
                            std::process::exit(0);
                        }
                    });
                },
            );
        });
    }


    fn render_network_view(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("simulation_log")
            .resizable(true)
            .default_height(120.0)
            .min_height(60.0)
            .max_height(250.0)
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(Color32::from_gray(240))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.heading("Simulation Log");
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let logs = self.simulation_log.lock().unwrap();
                            for (idx, log) in logs.iter().enumerate() {
                                let color = if idx % 2 == 0 { Color32::DARK_GRAY } else { Color32::DARK_BLUE };
                                ui.colored_label(color, log);
                            }
                        });
                    });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::Frame::default()
                .stroke(egui::Stroke::new(1.0, Color32::GRAY))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if let Some(ref topology) = self.selected_topology {
                            ui.add_space(10.0);
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
                        ui.add_space(10.0);
                        if ui.button("Recenter Graph").clicked() {
                            self.pan_offset = Vec2::ZERO;
                        }
                    });

                    ui.horizontal(|ui| {

                        ui.add_space(10.0);
                        if ui.button("Spawn Drone").clicked() {
                            self.show_spawn_drone_popup = true;
                        }
                    });
                    ui.horizontal(|ui|{
                        ui.add_space(10.0);
                        ui.label("Click on nodes to modify properties");

                    });
                    ui.horizontal(|ui| {

                        ui.add_space(10.0);
                        if ui.button("Shared senders").clicked() {
                            self.show_shared_senders_popup = true;
                        }
                    });

                    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::drag());

                    if self.pan_offset == Vec2::ZERO {
                        self.auto_fit_and_center_graph(response.rect);
                    }

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
                        renderer.scale = self.zoom_level;
                        painter.clip_rect();
                        let center = response.rect.center();
                        let scaled_size = response.rect.size() * self.zoom_level;
                        let scaled_rect = Rect::from_center_size(center + self.pan_offset, scaled_size);
                        painter.add(Shape::rect_filled(scaled_rect, 0.0, Color32::TRANSPARENT));
                        renderer.render(ui, self.pan_offset);
                        renderer.render_node_details(ctx);
                    }
                });
        });

        if self.show_shared_senders_popup {
            egui::Window::new("Shared Senders Table")
                .collapsible(false)
                .resizable(true)
                .show(ctx, |ui| {
                    if let Ok(map) = self.shared_senders.lock() {
                        if map.is_empty() {
                            ui.label("No shared senders available.");
                        } else {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                for ((src, dst), _) in map.iter() {
                                    ui.label(format!("({src}, {dst})"));
                                }
                            });
                        }
                    } else {
                        ui.label("⚠ Failed to acquire lock on shared_senders.");
                    }

                    if ui.button("Close").clicked() {
                        self.show_shared_senders_popup = false;
                    }
                });
        }

        //the spawn popup window
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
                                self.log(format!("Cannot spawn: ID {} already in use", self.new_drone_id));
                            } else if connections.is_empty() {
                                self.log("Cannot spawn: need at least one connection");
                            } else if bad_conn {
                                self.log("Cannot spawn: invalid connection IDs");
                            } else {
                                // 3) spawn + rebuild
                                self.show_spawn_drone_popup = false;

                                // 3a) spawn in the simulation controller
                                if let Some(ctrl) = &self.simulation_controller {
                                    let mut lock = ctrl.lock().unwrap();
                                    if let Err(e) = lock.spawn_drone(self.new_drone_id, self.new_drone_pdr, connections.clone()) {
                                       self.log(format!("Failed to spawn drone: {}", e));

                                        return;
                                    }
                                }

                                // 3b) immediately update the UI renderer
                                if let Some(renderer) = self.network_renderer.as_mut() {
                                    renderer.add_drone(self.new_drone_id, self.new_drone_pdr, connections.clone());

                                }

                                self.new_drone_connections_str.clear();
                                ctx.request_repaint();
                            }
                        }
                    });
                });
        }




    }


    fn auto_fit_and_center_graph(&mut self, canvas_rect: Rect) {
        if let Some(renderer) = &self.network_renderer {
            if renderer.nodes.is_empty() {
                return;
            }

            let mut min = Pos2::new(f32::INFINITY, f32::INFINITY);
            let mut max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);

            for node in &renderer.nodes {
                let pos = Pos2::new(node.position.0, node.position.1);
                min = min.min(pos);
                max = max.max(pos);
            }

            let graph_size = max - min;
            let canvas_size = canvas_rect.size();

            let padding = 0.8;

            let zoom_x = canvas_size.x * padding / graph_size.x;
            let zoom_y = canvas_size.y * padding / graph_size.y;
            let optimal_zoom = zoom_x.min(zoom_y).clamp(0.6, 3.0);

            self.zoom_level = 1.0;
            if let Some(r) = &mut self.network_renderer {
                r.scale = optimal_zoom;
            }

            let graph_center = Pos2::new((min.x + max.x) / 2.0, (min.y + max.y) / 2.0);
            let canvas_center = canvas_rect.center();

            self.pan_offset = (canvas_center.to_vec2() - graph_center.to_vec2()) * optimal_zoom;
        }
    }

    fn render_chat_view(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.chat_ui.client_status.is_empty() || self.chat_ui.servers.is_empty() {
                if let Some(ctrl) = &self.simulation_controller {
                    let ctrl = ctrl.lock().unwrap();
                    let client_ids = ctrl.get_all_client_ids();
                    let server_ids = ctrl.get_all_server_ids();
                    drop(ctrl); // Explicitly release the lock

                    for client_id in client_ids {
                        self.chat_ui.client_status.insert(client_id, ClientStatus::Offline);
                    }
                    self.chat_ui.servers = server_ids;
                }
            }

            self.chat_ui.render(ui, &mut |from: NodeId, to: NodeId, msg: String| {
                if let Some(ctrl) = &self.simulation_controller {
                    let mut ctrl = ctrl.lock().unwrap();
                    if let Some(sender) = ctrl.get_packet_sender(from, to) {
                        let data = msg.clone().into_bytes();
                        let mut buf = [0u8; 128];
                        let len = data.len().min(128);
                        buf[..len].copy_from_slice(&data[..len]);
                        let packet = wg_2024::packet::Packet::new_fragment(
                            wg_2024::network::SourceRoutingHeader {
                                hop_index: 1,
                                hops: vec![from, to],
                            },
                            0,
                            wg_2024::packet::Fragment {
                                fragment_index: 0,
                                total_n_fragments: 1,
                                length: len as u8,
                                data: buf,
                            },
                        );
                        let _ = sender.send(packet);
                    }
                }
            });
        });
    }


    pub fn new_with_network(
        cc: &eframe::CreationContext<'_>,
        event_sender: Sender<DroneEvent>,
        command_sender: Sender<DroneCommand>,
        command_receiver: Receiver<DroneCommand>,
        config: Arc<Mutex<ParsedConfig>>,
        drone_factory: Arc<dyn Fn(NodeId, Sender<DroneEvent>, Receiver<DroneCommand>, Receiver<Packet>, HashMap<NodeId, Sender<Packet>>, f32) -> Box<dyn wg_2024::drone::Drone> + Send + Sync>,
        config_path: &str,
        gui_input: SharedGuiInput,
        simulation_log: Arc<Mutex<Vec<String>>>,
        packet_senders: Arc<Mutex<HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>>>,
        packet_receivers: Arc<Mutex<HashMap<NodeId, Receiver<Packet>>>>,
        command_senders: Arc<Mutex<HashMap<NodeId, Sender<DroneCommand>>>>,
        shared_senders: Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>,
        host_senders: HashMap<NodeId, Sender<Packet>>,
        inbox_senders: Arc<Mutex<HashMap<NodeId, Sender<Packet>>>>,


    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::light());

        let mut app = Self::new(cc);

        app.controller_send = Some(event_sender.clone());

        let initializer = Arc::new(Mutex::new(
            NetworkInitializer::new(
                config_path,
                vec![],
                simulation_log.clone(),
                shared_senders.clone(),
            )
                .expect("Failed to create initializer"),
        ));

        let controller = SimulationController::new(config.clone(), event_sender.clone(), command_sender.clone(), drone_factory.clone(), gui_input.clone(), initializer.clone(), packet_senders.clone(), packet_receivers.clone(), command_senders.clone(), host_senders.clone(), shared_senders.clone(),inbox_senders.clone() );

        let controller = Arc::new(Mutex::new(controller));
        app.simulation_controller = Some(controller.clone());

        app.simulation_log = simulation_log.clone();

        app.network_renderer = Some(NetworkRenderer::new_from_config(
            "custom",
            50.0,
            50.0,
            config.clone(),
            &cc.egui_ctx,
            gui_input.clone(),
        ));

        if let Some(renderer) = &mut app.network_renderer {
            renderer.set_controller_sender(event_sender);
            renderer.set_simulation_controller(controller.clone());
            app.log("Controller connected to network renderer".to_string());
        }

        app.chat_ui = ChatUIState::new(gui_input.clone());
        app.network_config = Some(config.clone());
        app.detect_and_log_topology(config_path, config.clone());
        app.topology_selected = true;
        app.state = AppState::Welcome;
        app.shared_senders=shared_senders.clone();
        app
    }

    pub fn detect_and_log_topology(&mut self, path: &str, config: Arc<Mutex<ParsedConfig>>) {
        if let Some(topology_name) = config.lock().unwrap().detect_topology() {
           self.log(format!("Loaded topology from '{}'", path));
           self.log(format!("Detected topology: {}", topology_name));
            self.selected_topology = Some(topology_name);
        } else {
           self.log(format!("Could not detect known topology from '{}'", path));
        }

        self.topology_selected = true;
    }
    pub fn get_packet_sender(&self, id: NodeId) -> Option<&Sender<Packet>> {
        self.packet_senders.get(&id)
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
            simulation_log: Arc::new(Mutex::new(Vec::new())),
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
            chat_ui: ChatUIState::new(Arc::new(Default::default())),
            packet_senders: HashMap::new(),
            show_shared_senders_popup:false,
            shared_senders: Arc::new(Mutex::new(HashMap::new())),
            host_senders: HashMap::new(),

        }
    }
}

impl eframe::App for NetworkApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        match self.state {
            AppState::Welcome => self.render_welcome_screen(ctx),
            AppState::Simulation => self.render_simulation_tabs(ctx),
            _ => {}
        }
    }
}