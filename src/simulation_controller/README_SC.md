# 🧠 Simulation Controller - Module Overview

This module is responsible for managing the core logic, GUI communication, and network configuration of the drone simulation project. It integrates the graphical user interface with the simulation backend and dynamically handles user-defined topologies.

---

## 📁 File Structure and Responsibilities




### 📱🎨 app.rs 🎨📱

**Purpose:** Main application glue that launches the simulation GUI and backend.

**Key Responsibilities:**

* Establishes the graphical interface of the application
* Connects simulation backend to the front-end interface.

**Notable Functions:**

* `new(...)`: Sets up GUI state.
* `interactions with network`: crash_drone(..) , set_packet_drop_rate(..), add_connection(..), spawn_drone(..)..
* `front end`: render_simulation_tabs(..), render_welcome_screen(..), render_network_view(..), auto_fit_and_center_graph(..), render_chat_view(..)
*  `new_with_network(...)`: Connect controller (SC) to network renderer (design).


---

### 🏗️🛠️  `network_designer.rs`🛠️🏗️

**Purpose:** GUI-based tool for creating and modifying drone network topologies graphically.

**Key Responsibilities:**

* Allows user to add nodes (drones, clients, servers) and define edges.
* Provides functionality to save/load topology configurations as TOML.

**Notable Functions:**

* `Renderer Building`: which allows to build the network (nodes and edges) from a configuration given.
    new(..), new_from_config(...), build_from_config(...), rebuild_preserving_topology(...)
* `State updates and Synchronization`: responsible for syncing the visualization after updating the configuration (by crashing, spawning, adding new connections ecc..)
    update_network(..), sync_connections_with_config(..), sync_with_simulation_controller(...)
* `Topology setup & building`: permets the correct visualization of the different topologies
     build_topology_layout(...), setup_star(..), setup_tree(..) ...
* `Node and Edge management`: consists in adding and removing of nodes and their connections
     add_new_node(..), add_connection(..), remove_edges_of_crashed_node(..), reposition_hosts(..)
![image](https://github.com/user-attachments/assets/f5b28981-2faf-4e7b-b2bb-9de4d4654ae9)


---
### 🕹️💻 `SC_backend.rs`💻🕹️

**Purpose:** Core backend logic that drives the entire simulation engine.

**Key Responsibilities:**

* Handles drone events  (e.g., `PacketSent`, `PacketDropped`,`ControllerShortcut`) and sets up the sending of drone commands (e.g., `Set PDR`, `Remove Sender`, `Crash, `Spawn`).
* Manages topology validation and connection consistency.
* Initializes and supervises all nodes in the network.

**Notable Functions:**

* `Construction and Init`: handles creation and setup of the controller. It initializes the internal graph from ParsedConfig and adds the channels for each node.And eventually spawns a thread to process drone events
      initialize_network_graph(..), register_packet_sender(..), start_background_thread(..)
* `Event Handling`: process_event(..)
      ![image](https://github.com/user-attachments/assets/d3927f08-4338-4d90-ac5a-1aa8c941c183)

* `Command Handling`: crash_drone(..), set_packet_drop_rate(..), add_link(..) while doing the necessary checks not to violate network connectivity; is_crash_allowed(...), is_removal_allowed(...), validate_new_drone(...)
    ![image](https://github.com/user-attachments/assets/46380d2b-c7a4-41c6-8d13-7f4c293bfdbc)

* `Node State & Type Access`: get_node_state(...), get_all_drone_ids(...), get_all_server_ids(...), registered_nodes(...) ...

---

### 📮📬 `gui_input_queue.rs`📬📮

**Purpose:** Thread-safe communication buffer between GUI and hosts (clients and servers).

**Key Responsibilities:**

* Provides shared input queues for GUI-to-client messages.

**Notable Functions:**

* `new_gui_input_queue()`: Initializes the shared input buffer.
* `push_gui_message(...)`: Inserts commands into the appropriate node’s queue.
* `broadcast_topology_change()`: Notifies the hosts about a change in network (FloodRequired) 
    ![Immagine WhatsApp 2025-06-17 ore 22 02 16_51078c8e](https://github.com/user-attachments/assets/57cab2dc-0283-4752-bf58-3fef31e10d86)

---
### 💭🌐 `chatUI.rs`🌐💭

**Purpose:** Manages the GUI component responsible for client-server chat features.

**Key Responsibilities:**

* Renders UI for chatting, login, client selection, and message display.
* Maintains state such as current chat input, active chat pairs, and status.
* Sends messages to clients via `gui_input_queue`.

**Notable Functions:**

* `new(..)`: Initializes a new ChatUIState with default values and loaded media files.
* `load_media_files()` & `load_broadcast_files()` Loading media files from media/ and broadcast media/ directories.
* `render_server_info(..)`: Provides an overview on servers, connected clients, active chat status and type and a Gui input buffer inspection
   ![image](https://github.com/user-attachments/assets/cf5a2d52-091a-41a0-84c4-cf355bb905da)

* `render(..)`:The core GUI function rendering all parts of the chat UI, including: hosts selection & interaction, chat history box & media sharing

  📡 Client–Server Interaction
Login/Logout
Media upload
Broadcast media
Request media list
Download media
Request client list

   👥 Client–Client Interaction
Start chat with another client
Accept/decline chat request (pending_chat_request)
End chat session (pending_chat_termination)
Switch sender between two chatting clients
Send message and update chat history

  📚 Chat History Access
    show_history_popup:
    Pop-up dialog for users to retrieve history using a 6-digit verification code.
---



> Work by: **\[Kasraoui Oussema]**
> Module: `simulation_controller/`
> Project: Drone Network Simulation
> Language: Rust
