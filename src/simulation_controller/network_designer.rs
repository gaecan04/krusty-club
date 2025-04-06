use eframe::egui;
use egui::{Color32, Painter, Pos2, Response};

#[derive(Clone, Copy)]
pub enum NodeType {
    Server,
    Client,
    Drone,
}

pub struct Node {
    pub id: usize,
    pub node_type: NodeType,
    pub pdr: f32,
    pub active: bool,
    pub position: (f32, f32),
}

impl Node {
    pub fn drone(id: usize, x: f32, y: f32) -> Self {
        Self {
            id,
            node_type: NodeType::Drone,
            pdr: 1.0,
            active: true,
            position: (x, y),
        }
    }

    pub fn server(id: usize, x: f32, y: f32) -> Self {
        Self {
            id,
            node_type: NodeType::Server,
            pdr: 1.0,
            active: true,
            position: (x, y),
        }
    }

    pub fn client(id: usize, x: f32, y: f32) -> Self {
        Self {
            id,
            node_type: NodeType::Client,
            pdr: 1.0,
            active: true,
            position: (x, y),
        }
    }
}

enum Topology {
    Star,
    DoubleLine,
    Butterfly,
}
impl Topology {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "star" => Some(Topology::Star),
            "double_line" => Some(Topology::DoubleLine),
            "butterfly" => Some(Topology::Butterfly),
            _ => None,
        }
    }
}

pub(crate) struct NetworkRenderer{
    nodes: Vec<Node>,
    edges: Vec<(usize, usize)>,
    selected_node: Option<usize>,
    pub scale: f32,
}

impl NetworkRenderer {
    pub(crate) fn new(topology: &str, x_offset: f32, y_offset: f32) -> Self {
        let mut network = NetworkRenderer {
            nodes: Vec::new(),
            edges: Vec::new(),
            selected_node: None,
            scale: 1.0,
        };

        match Topology::from_str(topology) {
            Some(Topology::Star) => network.setup_star(x_offset, y_offset),
            Some(Topology::DoubleLine) => network.setup_double_line(x_offset, y_offset),
            Some(Topology::Butterfly) => network.setup_butterfly(x_offset, y_offset),
            None => println!("Invalid topology"),
        }

        network
    }

    fn setup_star(&mut self, x_offset: f32, y_offset: f32) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let num_drones = 10;
        let center_x = WINDOW_WIDTH / 2.0;
        let center_y = WINDOW_HEIGHT / 2.0;
        let radius = 100.0; // Reduced radius to fit within window

        // Create drones in a circular pattern
        for i in 0..num_drones {
            let angle = i as f32 * (std::f32::consts::PI * 2.0 / num_drones as f32);
            let x = center_x + radius * angle.cos();
            let y = center_y + radius * angle.sin();
            self.nodes.push(Node::drone(i, x, y));
        }

        // Connect drones with some edges
        for i in 0..num_drones {
            let target = (i + 3) % num_drones;
            self.edges.push((i, target));
        }

        // Place server outside the drone circle
        let server_id = num_drones;
        let server_x = center_x + radius + 50.0;
        let server_y = center_y;
        self.nodes.push(Node::server(server_id, server_x, server_y));

        // Connect server to specific drones
        self.edges.push((server_id, 0)); // Connect to first drone
        self.edges.push((server_id, 5)); // Connect to mid-range drone

        // Place clients outside the drone circle
        let client_1_id = num_drones + 1;
        let client_2_id = num_drones + 2;

        let client_1_x = center_x - radius - 50.0;
        let client_1_y = center_y - 50.0;

        let client_2_x = center_x - radius - 50.0;
        let client_2_y = center_y + 50.0;

        self.nodes.push(Node::client(client_1_id, client_1_x, client_1_y));
        self.nodes.push(Node::client(client_2_id, client_2_x, client_2_y));

        // Connect clients to their respective drones
        self.edges.push((client_1_id, 2));
        self.edges.push((client_2_id, 7));
    }

    fn setup_double_line(&mut self, x_offset: f32, y_offset: f32) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let line_spacing = (WINDOW_WIDTH - 100.0) / 4.0; // Distribute across window width
        let mut node_count = 0;

        // Create two lines of drones
        for i in 0..5 {
            let top_drone_x = 50.0 + i as f32 * line_spacing;
            let bottom_drone_x = 50.0 + i as f32 * line_spacing;

            let top_drone_y = WINDOW_HEIGHT / 3.0;
            let bottom_drone_y = 2.0 * WINDOW_HEIGHT / 3.0;

            self.nodes.push(Node::drone(node_count, top_drone_x, top_drone_y));
            node_count += 1;
            self.nodes.push(Node::drone(node_count, bottom_drone_x, bottom_drone_y));
            node_count += 1;

            // Connect vertical drones
            if i > 0 {
                self.edges.push((node_count - 2, node_count - 1));
            }

            // Connect horizontal drones
            if i > 0 {
                self.edges.push((node_count - 2, node_count - 4));
                self.edges.push((node_count - 1, node_count - 3));
            }
        }

        // Add server outside the drone lines
        let server_id = node_count;
        let server_x = WINDOW_WIDTH - 50.0;
        let server_y = WINDOW_HEIGHT / 6.0;
        self.nodes.push(Node::server(server_id, server_x, server_y));
        node_count += 1;

        // Connect server to nearby drones
        self.edges.push((server_id, 0)); // Connect to first top drone
        self.edges.push((server_id, 1)); // Connect to first bottom drone

        // Add clients outside the drone lines
        let client_1_id = node_count;
        let client_2_id = node_count + 1;

        // Client 1 below bottom line
        let client_1_x = 50.0;
        let client_1_y = WINDOW_HEIGHT - 50.0;

        // Client 2 above top line
        let client_2_x = WINDOW_WIDTH - 50.0;
        let client_2_y = 50.0;

        self.nodes.push(Node::client(client_1_id, client_1_x, client_1_y));
        self.nodes.push(Node::client(client_2_id, client_2_x, client_2_y));

        // Connect clients to their respective drones
        self.edges.push((client_1_id, 1));  // Connect to bottom line
        self.edges.push((client_2_id, 0));  // Connect to top line
    }

    fn setup_butterfly(&mut self, x_offset: f32, y_offset: f32) {
        const WINDOW_WIDTH: f32 = 600.0;
        const WINDOW_HEIGHT: f32 = 400.0;

        let line_spacing = (WINDOW_WIDTH - 100.0) / 4.0; // Distribute across window width
        let mut node_count = 0;

        // Create two lines of drones with a 'wing' effect
        for i in 0..5 {
            let left_drone_x = 50.0 + i as f32 * line_spacing;
            let right_drone_x = WINDOW_WIDTH - 50.0 - i as f32 * line_spacing;

            let left_drone_y = WINDOW_HEIGHT / 3.0;
            let right_drone_y = 2.0 * WINDOW_HEIGHT / 3.0;

            self.nodes.push(Node::drone(node_count, left_drone_x, left_drone_y));
            node_count += 1;
            self.nodes.push(Node::drone(node_count, right_drone_x, right_drone_y));
            node_count += 1;

            // Connect vertical drones
            if i > 0 {
                self.edges.push((node_count - 2, node_count - 1));
            }

            // Connect horizontal drones
            if i > 0 {
                self.edges.push((node_count - 2, node_count - 4));
                self.edges.push((node_count - 1, node_count - 3));
            }
        }

        // Add server outside the drone lines
        let server_id = node_count;
        let server_x = WINDOW_WIDTH / 2.0;
        let server_y = 50.0;
        self.nodes.push(Node::server(server_id, server_x, server_y));
        node_count += 1;

        // Connect server to nearby drones
        self.edges.push((server_id, 0)); // Connect to first top drone
        self.edges.push((server_id, 1)); // Connect to first bottom drone

        // Add clients outside the drone lines
        let client_1_id = node_count;
        let client_2_id = node_count + 1;

        // Client 1 below bottom line
        let client_1_x = 50.0;
        let client_1_y = WINDOW_HEIGHT - 50.0;

        // Client 2 above top line
        let client_2_x = WINDOW_WIDTH - 50.0;
        let client_2_y = 50.0;

        self.nodes.push(Node::client(client_1_id, client_1_x, client_1_y));
        self.nodes.push(Node::client(client_2_id, client_2_x, client_2_y));

        // Connect clients to their respective drones
        self.edges.push((client_1_id, 1));  // Connect to bottom line
        self.edges.push((client_2_id, 0));  // Connect to top line
    }

    fn remove_edges_of_crashed_node(&mut self, node_id: usize) {
        self.edges.retain(|&(a, b)| a != node_id && b != node_id);
    }

    pub fn render(&mut self, ui: &mut egui::Ui) {
        // Draw edges first
        {
            let painter = ui.painter();
            for &(a, b) in &self.edges {
                let pos_a = Pos2::new(self.nodes[a].position.0, self.nodes[a].position.1);
                let pos_b = Pos2::new(self.nodes[b].position.0, self.nodes[b].position.1);
                painter.line_segment([pos_a, pos_b], (2.0, Color32::GRAY));
            }
        }

        // Handle node interaction
        for node in &mut self.nodes {
            let pos = Pos2::new(node.position.0, node.position.1);
            let color = match node.node_type {
                NodeType::Server => Color32::BLUE,
                NodeType::Client => Color32::YELLOW,
                NodeType::Drone => if node.active { Color32::GREEN } else { Color32::RED },
            };

            // Handle interaction (allocate_rect) first, before using painter
            let response = ui.allocate_rect(
                egui::Rect::from_center_size(pos, egui::vec2(20.0, 20.0)),
                egui::Sense::click()
            );

            // Now we can safely draw the node circle using the painter
            {
                let painter = ui.painter();
                painter.circle(pos, 10.0, color, (1.0, Color32::BLACK));
            }

            // Handle click interaction
            if response.clicked() {
                self.selected_node = Some(node.id);
            }


        }
    }

    pub fn render_node_details(&mut self, ctx: &egui::Context) {
        // Display selected node info in a window
        if let Some(id) = self.selected_node {
            egui::Window::new("Node Information")
                .collapsible(false)
                .open(&mut self.selected_node.is_some())
                .show(ctx, |ui| {
                    let node = &mut self.nodes[id];
                    ui.label(format!("Node {}", node.id));
                    ui.label(match node.node_type {
                        NodeType::Server => "Type: Server".to_string(),
                        NodeType::Client => "Type: Client".to_string(),
                        NodeType::Drone => "Type: Drone".to_string(),
                    });

                    // If the node is a Drone, allow PDR modification and crash option
                    if let NodeType::Drone = node.node_type {
                        ui.add(egui::Slider::new(&mut node.pdr, 0.0..=1.0).text("PDR"));
                        if ui.button("Crash Node").clicked() {
                            node.active = false;
                            self.remove_edges_of_crashed_node(id);
                        }
                    }

                    // Close button
                    if ui.button("Close").clicked() {
                        self.selected_node = None;
                    }
                });
        }
    }
}