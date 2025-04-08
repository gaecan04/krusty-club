use std::fs;
use std::error::Error;

// Import necessary dependencies
#[cfg(feature = "serialize")]
use serde::Deserialize;
use wg_2024::network::NodeId;
use crate::network::initializer::ParsedConfig;

// These struct definitions were provided in your paste
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Drone {
    pub id: NodeId,
    pub connected_node_ids: Vec<NodeId>,
    pub pdr: f32,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Client {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
pub struct Server {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialize", derive(Deserialize))]
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