💻Client 1💻
=============

The client is responsible for a number of key functions that enable it to communicate with servers, discover the topology of the network via drones and manage the transmission and reception of messages. Clients are considered "on the edge" of the network, which means that they connect directly only to drones and not to each other or other servers, if not via drones.

### 🛠️`MyClient` structure

The  `MyClient` struct encapsulates the state and capabilities of a client in the simulation. Its main fields include:

-   `id`: the unique identifier of the client

-   `packet_recv`: a receiving channel for incoming packets from other nodes

-   `packet_send`: an HashMap that maps the ID of a node near a sending channel to send packets to neighbors. This allows the client to communicate with its direct neighbors

-   `sent_messages`: an HashMap that tracks sent messages, mapping a session_id to SentMessageInfo (this includes message fragments, the original routing header, received ACK indexes and a flag indicating if the route needs to be recalculated)

-   `received_messages`: an HashMap that handles incoming message fragments for reassembly

-   `network_graph`: a StableGraph representing the client's understanding of the network topology. This graph is built and updated through the flood discovery process and NACK management

-   `node_id_to_index`: an HashMap that maps NodeId to internal NodeIndex of network_graph, facilitating the search in the graph

-   `active_flood_discoveries`: an HashMap that keeps track of active flood discovery operations, useful for managing timeouts and responses

-   `connected_server_id`: an Option that stores the ID of the server to which the client is currently connected, if any

-   `seend_flood_ids`: an HashSet that stores a tuple `(flood_id, initiator_id)` to prevent reprocessing of flood requests already seen and infinite cycling

-   `route_cache`: an HashMap to store routes calculated for destinations, to avoid costly recalculations and repeated flood discovery processes

-   `simualtion_log`: allows the client to record information messages, warnings and errors that occur during its execution. Messages are added to a Vec<String> protected by a Mutex and Arc, allowing secure access and editing by multiple threads, ensuring detailed tracking of the client’s operations

-   `shared_senders`: this `Option<Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>>` contains a reference (shared and mutually exclusive) to a global map of Senders. Each sender in this map represents an active communication channel between 2 specific nodes `(NodeId, NodeId)` in the network. It permits the client to determine if a node is crashed

-   `shortcut_receiver`: a dedicated receiving channel for shortcut packets coming directly from the Simulation Controller

-   `pending_messages_after_flood`: stores high-level messages (in the form of GUI commands) that the client failed to send immediately

### 🆕`new` function

The `new` function is the constructor for `MyClient`. It initializes all fields of the struct, including communication channels, maps for messages and network graph. This is where the initial state of the client is established before its operation begins.

### 🏃🏻‍♀`️run` function

The `run` method is the heart of the client, containing its operational life cycle.

Processes GUI commands: Handles external inputs, such as user requests (e.g. Login, Logout, send messages, upload/download media) or commands from the simulation (e.g. AddSender, RemoveSender, SpawnDrone, Crash).

Check network discovery timeout: Periodically check if flood discovery operations have expired and, if so, finalize the topology construction and attempt to send waiting messages.

Process incoming packages: Use select_biased! to prioritise the packets of the shortcut_receiver (used by the Simulation Controller for non-droppable packets) and then handles the other packets received from nearby nodes.

**Discovery start**: at startup, if the client's `network_graph` is empty (that is, it does not yet have a knowledge of the topology), the client immediately starts a flood discovery process to populate its network graph. This is critical because clients and servers use Source Routing, which means they need to know the entire path to destination since drones do not maintain routing tables.

**Main loop**: the client enters an infinite loop that handles 2 main types of events:

1.  **commands from the GUI**: processes messages received from the Graphical User Interface (GUI). The client processes only one GUI message per iteration to avoid system crashes or delays

2.  **incoming packets**: listen to incoming packages on the `packet_recv` channel

`Flood discovery timeout`: each iteration of the loop also checks for active flood discoveries. If a timeout expires (2000 ms), the responses collected for that flood are processed to finalize the topology update.

* * * * *

### 📝Logging functions

The `attach_log` and `log` functions within the `MyClient` structure are dedicated to the management and recording of client events and operational status, based on the `simulation_log` property.

### `attach_log` function

This feature allows you to connect an external instance of a shared logger.

### `log` function

This function is the main method used by the client to write messages into its internal log (`simulation_log`).

* * * * *

### 🗺️Network Discovery Protocol (Flood Discovery)

The Network Discovery Protocol is used by clients and servers to gain and maintain an understanding of the network topology.

### `start_flood_discovery` function

This function starts the discovery process.

**Generation flood_id**: a unique `new_flood_id` is generated, using a global `SESSION_COUNTER` to ensure progressivity.

**Creating FloodRequest**: a `FloodRequest` package is created containing the flood ID, the initiator ID (the client itself) and a `path_trace` initialized with the client ID and type.

**Send to neighbors**: the `FloodRequest` package is sent to all direct neighbors of the client.

**Status tracking**: the flood status is recorded in `active_flood_discoveries`, including the initiator ID, start time and a list of responses received.

### `process_flood_request` function

When a client receives a `FloodRequest`:

-   **flood already seen**: if the flood ID is already present in `seen_flood_ids` or if the client has only one neighbor (which means that the flood has returned from the sender), the client does not propagate the request further. The `FloodResponse`  `routing_header` is built by reversing the `path_trace` and setting the `hop_index` to 1

-   **new flood**: if the flood ID has not yet been seen, the client adds it to `seen_flood_ids`, adds its own ID to `path_trace` and forwards the `FloodRequest` to all its neighbors except the one from which it received the request

### `process_flood_response` function

When the client receives a `FloodResponse`:

-   the client adds the response to the `received_responses` list for that specific `flood_id` in `active_flood_discoveries`

-   however, if the `flood_id` is not active or known, the client still processes the path to create the topology, which suggests that even late or unexpected responses contribute to network knowledge

### `create_topology` function

This function is responsible for building or updating the client `network_graph` based on the `path_trace` contained in a `FloodResponse`.

**Adding nodes**: the nodes in the `path_trace` (including initiator and intermediate nodes) are added to the `network_graph` if they are not already present. A `node_id_to_index` map is maintained to correlate the node IDs with the graph indexes.

**Adding edges** (bidirectional): for each pair of consecutive nodes in the `path_trace`, a bidirectional arc is added between them in the `network_graph`. Initially, the weight of these arcs is set to 0. It is checked that no duplicate edges are added.

This function allows the initiator to reconstruct the entire topology of the graph by accumulating information from various flood responses.

### `check_flood_discoveries_timeouts` and `finalize_flood_discovery_topology` functions

These functions handle the termination of Flood Discovery operations.

The first one is called regularly to identify the `flood_id` for which the timeout has expired (2000 ms). Once an expired flood has been identified, the second function removes the flood status from `active_flood_discoveries` and processes all collected `FloodResponse`s for that `flood_id` by calling `create_topology`.

* * * * *

### 📦Packet processing

The `process_packet` method is the central dispatch for incoming packets, delegating management to specific functions depending on the `PacketType`.

### 🚫NACK management

NACKs are crucial messages that indicate transmission errors and are used to dynamically update the client's understanding of link quality and node status. The Source Routing Header of a NACK contains the reverse path from the drone that encountered the problem to the original sender.

### `process_nack` function

This function handles the different types of NACK.

**`NackType::ErrorInRouting`** (`problem_node_id`):

-   indicates that a drone could not forward a packet because the next hop in the route was not its neighbor

-   if the node is crashed: The client removes the problematic node from its internal map node_id_to_index and its network_graph (the representation of the network topology)

-   if the node is still active (connection failure): The client removes the link (arc) from its network_graph that connects the previous node in the route to the problem_node_id, in both directions. It also removes any senders directed to that node from its packet_send map

In both cases, the route for the session involved is marked for recalculation (route_needs_recalculation = true), and any cache route to the final destination is invalidated, making it necessary to find a new route in the future.

**`NackType::DestinationIsDrone`**:

-   this NACK is sent when a packet, although successfully forwarded, reaches a drone that is the final destination of the packet, but drones should not be final destinations for high-level messages

-   the client invalidates the route in the `route_cache` for that destination and marks the route for recalculation. The drone is not removed from the graph

**`NackType::UnexpectedRecipient`** (`received_at_node_id`):

-   this NACK indicates that a packet arrived at a drone which was not the intended recipient (its ID did not match the current `hop_index` in the routing header)

-   the client penalizes the link that led to the unexpected node, increasing its "drop" count in both directions using `increment_drop`

-   the route for the session is marked for recalculation and the `route_cache` is invalidated

**`NackType::Dropped`**:

-   this NACK is sent when a drone decides to discard a packet because of its Packet Drop Rate (`PDR`)

-   the client penalizes the link between the node that sent the packet and the node that discarded it, increasing its "drop" count in both directions

-   the client retransmits the lost fragment through the original route

### `increment_drop` function

This function increases the "drop" count (`weight`) of a specific edge in the `network_graph`. Edges with a higher weight are penalized by the `best_path` algorithm.

### `remove_node_from_graph` function

This function removes a node and all its associated edges from the client `network_graph`. This is called in the case of `NackType::ErrorInRouting`.

### `remove_link_from_graph` function

This function is specifically designed to remove links (edges) between two specific nodes within the client `network_graph`.

### ✅ACK management

The ACK is sent by a client/server after receiving a packet to confirm receipt. ACK and NACK cannot be lost (since they will pass directly to the Simulation Controller in case of errors).

### `send_ack` function

This function builds and sends an ACK packet to the sender of the received fragment. The ACK `routing_header` is created by reversing the hops of the original packet and setting `hop_index` to 1 for the return path.

**ACK processing**: when a client receives an ACK, it updates the set `received_ack_indices` for the corresponding session. If all fragments have been recognized, the message is considered to be successfully sent.

* * * * *

### 📝Fragmentation and reassembly of messages

High-level messages (such as chat messages or files) are serialized and fragmented if they exceed a certain size (128 bytes) before being sent.

**Fragment send**: when a GUI command triggers the sending of a message (via `process_gui_command`), the message is fragmented into `MsgFragment`. Each fragment is then sent individually to the first hop of the calculated route. Information on sent messages (fragments, original header, received ACK) are stored in `sent_messages`.

### `reassemble_packet` function

This function is responsible for reassembling the received fragments.

**Storage fragments**: the fragments are stored in a buffer (`received_messages`) organized by `session_id`. If a duplicate fragment is received, it is ignored.

**Complete reassembly**: once all the fragments for a given session have been received, the complete message is reassembled.

**High-level message processing**: the reassembled message is passed to `process_received_high_level_message` for its processing.

### `process_received_high_level_message` function

This function analyzes the reassembled high-level messages and implements their application logic:

-   `[MessageFrom]`: receiving a chat message

-   `[ClientListResponse]`: receiving the list of available clients from the server

-   `[ChatStart]`: confirmation that a chat request has been started or declined

-   `[ChatFinish]`: chat termination signal

-   `[ChatRequest]`: receiving an incoming chat request

-   `[HistoryResponse]`: receiving chat history

-   `[MediaUploadAck]`: confirmation that a media has been loaded

-   `[MediaDownloadResponse]`: receiving data from a requested media, with logic to save and open the image

-   `[MediaListResponse]`: receive the list of available media from the server

-   `[LoginAck]`: confirmation of successful login to the server

* * * * *

### 🛣️`best_path` function

The function is critical for network routing and the client's ability to find the most efficient path for packets.

**Path search algorithm**: best_path implements a cost-based shortest path algorithm (similar to Dijkstra). This algorithm explores the graph by assigning a cumulative "cost" or "weight" to each path and selecting the one with the lowest total cost.

**Dynamic graph synchronization**: before starting the route calculation, the function ensures that the client’s internal `network_graph` is synchronized with the current state of shared communication channels (`shared_senders`). This process is essential to ensure that the calculated paths are valid compared to the real network connectivity, removing any connections from the graph if the corresponding sending channels are no longer active or do not exist.

**Cost and drop management**: the weights of the arcs in the graph (initially 0) can be increased by the `increment_drop` function. This happens, for example, when a package is dropped or arrives at an unexpected recipient. Increasing the weight of a problematic connection discourages the router from using it in future routes, favouring more reliable routes.

**Intermediate node restrictions**: when calculating the route, it is important to note that only drones can act as intermediate nodes. Clients and servers can only be the starting or finishing nodes of the path, they cannot be part of the intermediate path.

**Output**: the function returns an `Option<Vec<NodeId>>`. If a path is found, returns `Some(path)` where path is an ordered list of `NodeId` that represents the path from the source node to the destination node. If no valid path is found, returns `None`.

* * * * *

### ⌨️`process_gui_command` function

This function is the entry point for commands sent from the GUI to the client. It analyzes the received `command_string`, which represents a high-level message, and activates the appropriate logic:

-   `[FloodRequired]`: this command is indicator of a change in the simulated network topology and require an update of client knowledge
    `AddSender`: adds a new send channel to a node and updates the client's `network_graph` to include the new bidirectional link
    `RemoveSender`: removes a send channel to a node and invokes `remove_link_from_graph` to update the internal graph, reflecting the disconnection
    `SpawnDrone`: if the client is connected (or can connect) to the new drone, this operation adds the drone to the `network_graph` of the client and establishes the appropriate communication channles
    `Crash`: indicates the crash of a specific node. The client removes the node and all its links from the `network_graph` and `packet_send_map`, reflecting the unavailability state

-   `[Login]`: tries to authenticate the client with the specified server, updating the `connected_server_id` variable of the client

-   `[Logout]`: disconnects the client from the connected server, setting `connected_server_id` to `None`

-   `ClientListRequest`: requests to the server a list of the connected clients

-   `[MessageTo]`: sends a chat message to a specific client through server

-   `[ChatRequest]`: sends a request to chat to a specific client through server

-   `[ChatFinish]`: flags the ending of a chat session with a specific peer

-   `[MediaUpload]`: uploads a multimedia file (encoded in `Base64`) on server

-   `[MediaDownloadRequest]`: request the download of a multimedia file to a server

-   `[HistoryRequest]`: request to the server the chronology of the chat between 2 specific clients

-   `[MediaListRequest]`: request to the server a list of the available multimedia file

-   `[MediaBroadcast]`: sends a multimedia file to all the connected clients through a server

If the `best_path` function is not able to find a valid route, the client queue the message in `pending_messages_after_flood`. Then, the client start a flood discovery.

* * * * *

### 🤝`send_to_neighbor` function

This function is a helper to send a packet to a specific neighbor. Gets the appropriate sender from the client's `packet_send` map and attempts to send the packet. It also handles sending errors.

### 🌐Global variables: `SESSION_COUNTER`

`SESSION_COUNTER` is a global variable of type `Lazy<Mutex<u64>>>`. It is used to generate unique and progressive `session_id` for packets, including `flood_id`.

### ⚡Competition management: `select_biased!`

The client `run` method uses the crossbeam-channel's `select_biased!` macro. This is a mechanism to efficiently listen to multiple channels at the same time. In the client, it is used to give priority to receiving packets (from `packet_recv`) or processing GUI commands (via `gui_input`), although the exact priority might be affected by the order of `select_biased!`.