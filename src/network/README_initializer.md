# Initializer Module (`initializer.rs`)

This file defines the network initialization logic for a peer-to-peer drone networked chat system.  
It reads the parsed configuration, instantiates nodes (clients, servers, drones), builds their communication channels, and launches them in threads.

The initializer is crucial for setting up the network topology, verifying bidirectional links, and dynamically assigning drone implementations.

---

## 📦 Imports and Crate Dependencies

Includes:
- `crossbeam_channel`: channel-based message passing between nodes.
- `wg_2024`: project-wide network types, packet formats, and drone traits.
- `std::thread`: thread management for spawning each node.
- `log`: for diagnostic logging.
- `once_cell`, `rand`: for singleton/global state and randomized behavior.

---

## ⚙️ `STRUCTURES USED:`
### `ParsedConfig`
Deserialized from a TOML config file, this struct holds all node definitions.

- **Fields:**
    - `drone: Vec<DroneConfig>` –-> list of all drones.
    - `client: Vec<Client>` –--> list of all clients.
    - `server: Vec<Server>` –--> list of all servers.

- **Purpose:** Is the structure that includes entire network topology elements.

- **Methods:**
    - `add_drone` --> Create a new drone configuration with default values and adds it to self.drone
    - `set_drone_connections` --> method to set the connection for a drone
      -`set_drone_pdr` –-> modify drone PDR (probability to drop a packet when received).
    - `remove_client/server/drone_connections` –-> 3 separate function that remove all connections references to/from a client/server/drone.
    - `append_drone/client/server_connection` –-> add a single connection to a node.
    - `detect_topology()` –-> This function is designed to analyze the current network configuration of 10 drones and determine whether it matches a known topology pattern such as a tree, star, butterfly, subnet, or double chain.
        1) Only runs if there are exactly 10 drones.
        2) Drones are sorted and copied into a Vec and a HashSet.
        3) It's then created an HashMap including only drone-to-drone connections
        4) Network evaluation from previous elements.
---

### `DroneConfig`
Contains the metadata for each drone node.

- **Fields:**
    - `id: NodeId` –-> drone's unique ID.
    - `pdr: f32` –-> packet drop rate of the drone.
    - `connected_node_ids: Vec<NodeId>` –-> list of connected node IDs.

---

### `MyDrone`
MyDrone is your fallback implementation, used only when a group’s
implementation is missing or fails to register.
```rust
#[derive(Debug)]
pub struct MyDrone {
    pub id: NodeId,
    controller_send: Sender<DroneEvent>,
    controller_recv: Receiver<DroneCommand>,
    packet_recv: Receiver<Packet>,
    packet_send: HashMap<NodeId, Sender<Packet>>,
    pdr: f32,
}
```
The trait "DroneImplementation" is implemented for MyDrone.
- ***purpose:***

  Defines how to adapt multiple drone implementations to a common interface (DroneImplementation) so they can be launched dynamically in a uniform way.
  A fallback implementation of the `Drone` trait. Only used when no group-specific drone is matched.
   ```rust
   use wg_2024::drone::Drone as DroneTrait;

   impl DroneImplementation for MyDrone {
      fn run(&mut self) {
         <Self as DroneTrait>::run(self);
      }
      //The DroneImplementation trait (your internal abstraction for launching drones) requires a run() method. This implementation simply delegates to the existing logic in the Drone trait’s run() function.
   }

   macro_rules! impl_drone_adapter {
      ($name:ty) => {
         impl DroneImplementation for $name {
               fn run(&mut self) {
                     <Self as wg_2024::drone::Drone>::run(self);

                  }
                  //It generates a DroneImplementation implementation for any drone type ($name) that already implements the wg_2024::drone::Drone trait.
         }
      };
   }

   // === Macro Calls for All 10 Drones ===
   impl_drone_adapter!(BagelBomber);
   impl_drone_adapter!(FungiDrone);
   impl_drone_adapter!(RustDrone);
   impl_drone_adapter!(RustafarianDrone);
   impl_drone_adapter!(RollingDrone);
   impl_drone_adapter!(LeDron_JamesDrone);
   impl_drone_adapter!(DrOnesDrone);
   impl_drone_adapter!(SkyLinkDrone);
   impl_drone_adapter!(RustasticDrone);
   impl_drone_adapter!(CppEnjoyersDrone);

   ```
---

### `DroneWithId`
Wraps a drone implementation together with its ID in a `Box<dyn DroneImplementation>`.

---

### `NetworkInitializer`
Main orchestrator for launching the simulation. Reads configuration, validates it, assigns drone logic, and spawns all nodes.

```rust
pub struct NetworkInitializer {
    config: Config,
    pub(crate) drone_impls: Vec<DroneWithId>,
    pub(crate) packet_senders: Arc<Mutex<HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>>>,
    pub(crate) packet_receivers: Arc<Mutex<HashMap<NodeId, Receiver<Packet>>>>,
    pub(crate) command_senders: Arc<Mutex<HashMap<NodeId, Sender<DroneCommand>>>>,
    pub(crate) command_receivers: HashMap<NodeId, Receiver<DroneCommand>>, // ✅ Add this
    pub(crate) controller_event_receiver: Option<Receiver<DroneEvent>>,
    pub(crate) event_sender: Option<Sender<DroneEvent>>, // ✅ Also added previously
    controller_tx: Sender<DroneEvent>,
    controller_rx: Receiver<DroneCommand>,
    simulation_controller: Option<Arc<Mutex<SimulationController>>>,
    simulation_log: Arc<Mutex<Vec<String>>>,
    pub(crate) shared_senders: Option<Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>>,
}
```
### `NetworkInitializer` Methods

#### 🛠 `new(config_path, drone_impls, simulation_controller)`

#### Purpose:
Constructs a new `NetworkInitializer` by:
- Reading a TOML file from `config_path`
- Parsing it into a `Config` object (enabled with `serialize` feature)
- Setting up empty structures and controller communication channels

   ---

### ⚙️ `initialize(gui_input_queue)`

#### Purpose:
Bootstraps the simulation:
1. Validates the network configuration.
2. Initializes communication channels.
3. Distribute drone implementations and spawn drone threads
3. Spawns client, and server threads.
   ---

### ✅ `validate_config()`

#### Validations Performed:
- No duplicate node IDs
- Clients:
    - Must connect to 1 or 2 drones
    - Must not connect to themselves
- Servers:
    - Must connect to at least 2 drones
    - Must not connect to themselves
- Each client could be connected to all servers in the network
- Each server could be connected to all clients in the network
- Connections must be bidirectional
- The graph must be connected
- Clients and servers must sit at the edges of the graph

   ---

### 🔄 `check_bidirectional_connections()`
#### Purpose:
Verifies that all declared connections (from clients/servers to drones and vice versa) are **symmetric**.  
If a client connects to a drone, the drone must also connect back.
A map of all nodes and thei connections is created :
   ```rust
   let mut node_connections: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();
   ```
The we check for bidirectionality.

   ---

### 🔗 `check_connected_graph()`

#### Purpose:
Verifies that the entire graph (clients, servers, drones) forms a **single connected component** using BFS.

   ---

### 🌿 `check_edges_property()`
1. Builds a graph without clients and servers
2. Extract drone-to-drone connections
3. Check if the drone-only graph is connected using BFS

### Purpose:
Ensures that only drones form the core graph, while clients and servers sit at the **edges**.  
Runs a BFS using **only drone-to-drone links** to ensure the core is connected.

   ---

### 📨 `setup_channels()`

#### Purpose:
Initializes:
- One `Receiver<Packet>` per node
- A per-node `Sender<Packet>` map containing neighbor senders
  Populates:
- `packet_senders`
- `packet_receivers`

   ---

### 🚁 `initialize_drones()`
Responsible for spawning drone threads.
#### Purpose:
Launches each drone implementation in a new thread by calling `run()` on the boxed trait object.
Used drones come from the `drone_impls` vector.

```rust
 for DroneWithId { id, mut instance } in self.drone_impls.drain(..) {
            //let mut drone = drone_impl; // Box<dyn DroneImplementation>
            //let id = drone.get_id();

            // Spawn a thread that just runs the drone's run() method
            thread::spawn(move || {
                instance.run(); // <- Each group's own logic
            });
            println!("Spawned drone {}", id);
        }

        println!("printing neighbors from config:");
        for node in self.config.drone.iter().map(|d| (d.connected_node_ids).clone()){
            println!("🌱🌱🌱🌱🌱🌱\t{:?}",node );
        }

```

       
---

### 👥 `initialize_clients(gui_input,log,host_receivers)`

#### Purpose:
Start threads dedicated to each client in the simulation.
- it iterates through client configuration (self.config.client)
- for each client, it retrives the Sender map for its neighbors and its Receiver<Packet> from the packet_senders and packet_receivers maps previously created by setup_channels
- it creates a new thread for each client, instantiating the type client1::MyClient (or client2::MyClient, depending on the selection logic) and calling its run() method, passing the SharedGuiInput to allow interaction with the GUI

   ---

### 🖥 `initialize_servers(gui_input,log,host_receivers)`

#### Purpose:
Start threads dedicated to each server in the simulation.
- similar to client initialization, itera through server configuration (self.config.server)
- it retrieves the sender of the server’s neighbors and the Receiver<Packet> of the server from the maps packet_senders and packet_receivers
- it spawns a new thread for each server, instantiating server::server::new and calling its run() method, also with access to SharedGuiInput for managing events from the GUI

   ---

### 🎮 `spawn_controller()`

#### Purpose:
Starts the main thread of the Simulation Controller, which is responsible for supervising and coordinating the entire simulation.
- the controller thread manages the central logic of the simulation, including node logging, event monitoring (DroneEvent::PacketSent, PacketDropped, ControllerShortcut) sent from nodes, and sending commands (DroneCommand) to nodes
- the controller operates in a selection loop (select!) to receive events and commands, processing them to influence the state of the simulation and the network


   ---

### 🤖 `create_drone_implementations(...)`

#### Purpose:
The implementations are loaded through 'load_group_implementations()'
```rust
let group_implementations = Self::load_group_implementations();
```
and after that each of the drones present in the configuration_name.toml file gets assigned one of the group implementations or the fallback implementation if a registration error occurs. When each drone has been created based on an implementation with return all of them as DroneWithId.
```rust
pub fn create_drone_implementations(
        config: &ParsedConfig,
        controller_send: Sender<DroneEvent>,
        controller_recv: Receiver<DroneCommand>,
        packet_receivers: &HashMap<NodeId, Receiver<Packet>>,
        packet_senders: &HashMap<NodeId, HashMap<NodeId, Sender<Packet>>>,
    ) -> Vec<DroneWithId>
```
   ---

### 🧩 `load_group_implementations()`

#### Purpose:
Helps in having a cleaner `create_drone_implementations(...)` body. In here all group implementations are assigned to a DroneImplementation struct and Boxed, all following the same pattern.
```rust
group_implementations.insert(
            "group_1".to_string(),
            Box::new(|id: NodeId, sim_contr_send: Sender<DroneEvent>, sim_contr_recv: Receiver<DroneCommand>,
                      packet_recv: Receiver<Packet>, packet_send: HashMap<NodeId, Sender<Packet>>, pdr: f32|
                      -> Box<dyn DroneImplementation> {
                Box::new(LeDron_JamesDrone::new(
                    id,
                    sim_contr_send,
                    sim_contr_recv,
                    packet_recv,
                    packet_send,
                    pdr
                ))
            }) as GroupImplFactory
        );

```
Each time a GroupImplFactory is created we insert it in an HashMap as the value associated with the key 'group_n.to_string()' (n is the order in which they were added) , that is returned to be used for initialization.
```rust
fn load_group_implementations() -> HashMap<String, GroupImplFactory>
```

---
---




## 📌 Notes

- The initializer does not control simulation flow after launch.
- It does not perform runtime checks on packet delivery—this is left to individual node implementations.
- It’s assumed that drone implementations honor the protocol defined in `wg_2024`.
