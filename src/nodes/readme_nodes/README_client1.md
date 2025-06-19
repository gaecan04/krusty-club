💻Client💻
==========

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

### 🆕`new` function

The `new` function is the constructor for `MyClient`. It initializes all fields of the struct, including communication channels, maps for messages and network graph. This is where the initial state of the client is established before its operation begins.

### 🏃🏻‍♀`️run` function

The `run` method is the heart of the client, containing its operational life cycle.

**Discovery start**: at startup, if the client's `network_graph` is empty (that is, it does not yet have a knowledge of the topology), the client immediately starts a flood discovery process to populate its network graph. This is critical because clients and servers use Source Routing, which means they need to know the entire path to destination since drones do not maintain routing tables.

**Main loop**: the client enters an infinite loop that handles 2 main types of events:

1.  **commands from the GUI**: processes messages received from the Graphical User Interface (GUI). The client processes only one GUI message per iteration to avoid system crashes or delays

2.  **incoming packets**: listen to incoming packages on the `packet_recv` channel

`Flood discovery timeout`: each iteration of the loop also checks for active flood discoveries. If a timeout expires (2000 ms), the responses collected for that flood are processed to finalize the topology update.

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

-   the client interprets this as a crash of the problem node and removes it from its `network_graph` using `remove_node_from_graph`

-   the route for that session is marked for recalculation (`route_needs_recalculation = true`) and the route stored in the `route_cache` for the destination is invalidated

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

**Cache search**: before calculating a new route, the client checks the `route_cache` to see if there is already a valid and recent route for the specified destination. If found, the route is returned immediately.

**Astar algorithm**: if the route is not cached or has been disabled, the function implements the Astar algorithm (which with a heuristic of 0 becomes equivalent to Dijkstra) to find the path at minimum cost.

**Temporary graph construction**: a DiGraph temporary graph is constructed for the path calculation, copying nodes and edges from the client network_graph. Edge weights are based on "drop" counts, which means that links with multiple errors or bounces are considered more "expensive".

**Route reconstruction**: once Dijkstra has calculated the minimum costs, the route is reconstructed backwards from destination to origin using predecessors.

**Validation and error handling**: checks are performed to make sure that the starting and arrival nodes are present in the graph and that the calculated path is valid (not empty, starts and ends correctly).

**Trigger Flood Discovery**: if `best_path` fails to find a route (for example, if the graph is empty or the destination is unreachable), the client starts a new Flood Discovery process to update its knowledge of the network.

* * * * *

### ⌨️`process_gui_command` function

This function is the entry point for commands sent from the GUI to the client. It analyzes the received `command_string`, which represents a high-level message, and activates the appropriate logic:

-   **parsing commands**: the command string is divided into tokens to identify the type of command and its parameters

-   **Login/Logout**: manages login requests to a server, updating `connected_server_id` and logout requests

-   **service requests**: triggers various types of requests to servers, such as `ClientListRequest`, `MediaDownloadRequest`, `MediaListRequest`, `HistoryRequest` and `MediaBroadcast`

-   **chat communications**: manages the sending of specific messages (`MessageTo`, `ChatRequest`, `ChatFinish`)

-   **trigger flood**: command is received (often in response to topology changes from the Simulation Controller), the function calls `start_flood_discovery`

-   **sending high_level messages**: after determining the GUI command type, the route (`best_path`) to `connected_server_id` is attempted. If a route is found, the message is fragmented and sent to the first hop of the route. If the route fails, a Flood Discovery is initiated

* * * * *

### 🤝`send_to_neighbor` function

This function is a helper to send a packet to a specific neighbor. Gets the appropriate sender from the client's `packet_send` map and attempts to send the packet. It also handles sending errors.

### 🌐Global variables: `SESSION_COUNTER`

`SESSION_COUNTER` is a global variable of type `Lazy<Mutex<u64>>>`. It is used to generate unique and progressive `session_id` for packets, including `flood_id`.

### ⚡Competition management: `select_biased!`

The client `run` method uses the crossbeam-channel's `select_biased!` macro. This is a mechanism to efficiently listen to multiple channels at the same time. In the client, it is used to give priority to receiving packets (from `packet_recv`) or processing GUI commands (via `gui_input`), although the exact priority might be affected by the order of `select_biased!`.