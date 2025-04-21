use std::fs;
use std::error::Error;
use serde::{ Serialize,Deserialize};
use wg_2024::network::NodeId;
use crate::network::initializer::ParsedConfig;

#[derive(Debug, Clone,Serialize,Deserialize)]
//#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Drone {
    pub id: NodeId,
    pub connected_node_ids: Vec<NodeId>,
    pub pdr: f32,
}

#[derive(Debug, Clone,Serialize,Deserialize)]
//#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Client {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,
}

#[derive(Debug, Clone,Serialize,Deserialize)]
//#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Server {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,

}

#[derive(Debug, Clone,Serialize,Deserialize)]
//#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Config {
    pub drone: Vec<Drone>,
    pub client: Vec<Client>,
    pub server: Vec<Server>,
}

pub fn parse_config(path: &str) -> Result<ParsedConfig, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let parsed: ParsedConfig = toml::de::from_str(&content)?;
    Ok(parsed)
}

// Example usage
fn main() -> Result<(), Box<dyn Error>> {
    let config = parse_config("topologies/double_chain.toml")?;

    println!("Parsed config: {:?}", config);

    // Print some information about the network
    println!("Network configuration:");
    println!("- {} drones", config.drone.len());
    println!("- {} clients", config.client.len());
    println!("- {} servers", config.server.len());

    // Example of accessing drone data
    for (i, drone) in config.drone.iter().enumerate() {
        println!("Drone #{}: ID={}, PDR={}, Connected to {} nodes",
                 i+1, drone.id, drone.pdr, drone.connected_node_ids.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use toml;

    #[test]
    fn test_config_parsing() {
        // Path to a test TOML file
        let config_path = "topologies/tree.toml";

        // Read the file content
        let config_str = fs::read_to_string(config_path).expect("Failed to read config file");

        // Parse the TOML content
        let config: Config = toml::from_str(&config_str).expect("Failed to parse config");

        // Check if the config has the expected structure
        assert!(config.drone.len() > 0, "Expected drones in the config");
        assert!(config.client.len() > 0, "Expected clients in the config");
        assert!(config.server.len() > 0, "Expected servers in the config");

        // Add more specific assertions based on your test configuration
    }
}
