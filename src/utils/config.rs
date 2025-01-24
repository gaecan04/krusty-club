//an ex


use serde::Deserialize;
use std::fs;

pub type NodeId = u8;

/// Represents the entire network configuration loaded from the `.toml` file.
#[derive(Debug, Deserialize)]
pub struct NetworkConfig {
    pub drones: Vec<DroneConfig>,
    pub clients: Vec<ClientConfig>,
    pub servers: Vec<ServerConfig>,
}

/// Represents a single drone's configuration.
#[derive(Debug, Deserialize)]
pub struct DroneConfig {
    pub id: NodeId,
    pub connected_node_ids: Vec<NodeId>,
    pub pdr: f32, // Packet Drop Rate
}

/// Represents a single client's configuration.
#[derive(Debug, Deserialize)]
pub struct ClientConfig {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,
}

/// Represents a single server's configuration.
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,
}

impl NetworkConfig {
    /// Parses the `.toml` file into a `NetworkConfig` struct.
    pub fn from_file(file_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(file_path)?;
        let config: NetworkConfig = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// Validates the network configuration to ensure it meets the requirements.
    fn validate(&self) -> Result<(), String> {
        // Example validations:
        // 1. Ensure unique `id` for all nodes
        let mut ids = self.drones.iter().map(|d| d.id)
            .chain(self.clients.iter().map(|c| c.id))
            .chain(self.servers.iter().map(|s| s.id))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1]) {
            return Err("Duplicate node IDs found".into());
        }

        // 2. Validate connectivity rules (e.g., no client connects to servers)
        for client in &self.clients {
            if client.connected_drone_ids.len() > 2 {
                return Err(format!("Client {} is connected to more than 2 drones", client.id));
            }
        }

        for server in &self.servers {
            if server.connected_drone_ids.len() < 2 {
                return Err(format!("Server {} must connect to at least 2 drones", server.id));
            }
        }

        // Additional checks as needed (e.g., connected graph, no self-connections)

        Ok(())
    }
}