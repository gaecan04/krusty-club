use eframe::{egui, App};
use egui::{Color32, RichText, Vec2, Rect, Sense, Shape};
use crate::simulation_controller::network_designer::NetworkRenderer;
//hello
// Existing enums remain the same
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
}

impl NetworkApp {
    fn set_topology(&mut self, topology: &str) {
        self.network_renderer = Some(NetworkRenderer::new(topology, 50.0, 50.0));
        self.selected_topology = Some(topology.to_string());
        self.topology_selected = true;
        self.state = AppState::Simulation;
    }

    fn flood_network(&mut self) {
        // TODO: Implement actual flood network logic
        self.simulation_log.push("Flood Network initiated".to_string());
        self.simulation_log.push("Simulating network flood...".to_string());
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
                        }
                        if ui.button("Zoom Out").clicked() {
                            self.zoom_level = (self.zoom_level / 1.2).max(0.5);
                        }
                        ui.label(format!("Zoom: {:.1}x", self.zoom_level));
                    });

                    // Flood Network Button
                    ui.horizontal(|ui| {
                        if ui.button("Flood Network").clicked() {
                            self.flood_network();
                        }
                    });

                    // Network view with zoom and pan
                    let (response, painter) = ui.allocate_painter(
                        ui.available_size(), // Use full available space
                        Sense::drag()
                    );

                    // Draw a border around the network view
                    painter.add(Shape::rect_stroke(
                        response.rect,
                        2.0,
                        egui::Stroke::new(1.0, Color32::DARK_GRAY)
                    ));

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
                        // TODO: Implement proper rendering method in NetworkRenderer
                        painter.add(Shape::rect_filled(
                            scaled_rect,
                            0.0,
                            Color32::TRANSPARENT
                        ));

                        // Render the actual network
                        renderer.render(ui);

                        // Render node details window
                        renderer.render_node_details(ctx);
                    }
                });
        });
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

                    /*
                    if ui.button("Flood Simulation").clicked() {
                        // TODO: Implement flood simulation logic
                        self.simulation_log.push("Flood Simulation initiated".to_string());
                    }*/

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

                /* Back to Welcome Button
                if ui.button("Back to Welcome").clicked() {
                    self.state = AppState::Welcome;

                 */
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