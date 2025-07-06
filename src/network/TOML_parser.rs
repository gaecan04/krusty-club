use std::fs;
use serde::{ Serialize,Deserialize};
use wg_2024::network::NodeId;
use crate::network::initializer::ParsedConfig;

#[derive(Debug, Clone,Serialize,Deserialize)]
pub struct Drone {
    pub id: NodeId,
    pub connected_node_ids: Vec<NodeId>,
    pub pdr: f32,
}

#[derive(Debug, Clone,Serialize,Deserialize)]
pub struct Client {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,
}

#[derive(Debug, Clone,Serialize,Deserialize)]
pub struct Server {
    pub id: NodeId,
    pub connected_drone_ids: Vec<NodeId>,

}

#[derive(Debug, Clone,Serialize,Deserialize)]
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

