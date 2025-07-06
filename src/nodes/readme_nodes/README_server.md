# Server Module (`server.rs`)

This file defines the behavior of a server in a peer-to-peer drone networked chat system.
The server acts as a hybrid of a chat server and a media server.
It handles packet transmission, message reassembly, client registration, media storing, media broadcasting and dynamic topology updates.
The server is the element of the network that ensures the right client-to-client communication. It takes commands also from GUI.

---

## 🌐 `STRUCT NETWORKGRAPH`
The NetworkGraph struct plays a central role in maintaining a live view of the network topology and ensuring reliable routing between nodes.
``` rust 
pub struct NetworkGraph {
    graph: DiGraph<NodeId, usize>, // contains the graph itself
    node_indices: HashMap<NodeId, NodeIndex>, //used to map each NodeId -> NodeIndex
    node_types: HashMap<NodeId, NodeType>, //used to map NodeId -> NodeType
    shared_senders: Option<Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>>, //used to know the senders involved in Node-to-Node link
}
```
***Purpose:*** <br>
Represents the network as a bidirectional graph where nodes are clients, drones, or servers. The graph will be used for the computation
of the best path between nodes.
### Methods:
- `new()`: Initialize an empty graph.
- `add_node(id, type)`: Insert a node if missing or update type.
- `add_link(a, a_type, b, b_type)`: Creates a bidirectional edge with weight 1.
- `remove_node(id)`: Remove node and clean mappings.
- `remove_link(NodeId, NodeId)`: Remove an edge in the graphs but preserving nodes' integrity, used when we do a "RemoveSender" change in topology.
- `increment_drop(a, b)`: Increases weight of edge after drop --> this is how the path gets penalized, promoting rerouting with "cheaper" links.
- `best_path(src, tgt)`: his method computes the shortest valid path from a given source node to a target node in the current dynamic network graph.
  It is used by the server or any node to route messages using source routing, ensuring the path only includes operational and allowed links.
  Features:
    1. Dynamic path validation: Before computing the path, the function removes any edges that are no longer valid by checking the shared_senders map.
       This ensures packets won't be routed through links that no longer exist or are broken due to a drone crash or link removal.
    2. Dijkstra's algorithm with constraint: intermediate nodes must be drones, with clients and server allowed only as start or end nodes
    3. Cycle prevention: Source node is never allowed to be an intermediate node --> avoiding loops.
    4. Detailed error handling
- `set_node_type(id, type)`: Associate node ID with a type, allowing the system to differentiate routing behavior (e.g., avoiding servers when relaying messages).
- `get_node_type(id)`: Retrieve the node type.
- `print_graph()`: Log current graph state with edge weights --> used testing phase.

---

## 📡`STRUCT SERVER`

***Purpose:*** <br>
Handles incoming and outgoing packets, client interaction, flooding for discovery, and media/chat server functionalities.
```rust

#[derive(Debug, Clone)]
pub struct server {
pub id: u8, // Server ID
pub received_fragments: HashMap<(u64, NodeId), Vec<Option<[u8; 128]>>>, // Maps (session_id, src_id) to fragment data
pub fragment_lengths: HashMap<(u64, NodeId), u8>, // Maps (session_id, src_id) to length of the last fragment
pub packet_sender: HashMap<NodeId, Sender<Packet>>, // Hashmap containing each sender channel to the neighbors (channels to send packets to clients)
pub packet_receiver: Receiver<Packet>, // Channel to receive packets from clients/drones

    seen_floods:HashSet<(u64, NodeId)>, //Avoids re-processing old FloodRequests.
    registered_clients: Vec<NodeId>, //List of clients connected to server.
    network_graph: NetworkGraph, //Contains `NetworkGraph` logic --> each server has its own knowledge of the network.
    sent_fragments: HashMap<(u64,u64), (Fragment, NodeId)>, //For retransmission in case of NACKs.
    chat_history: HashMap<(NodeId,NodeId), VecDeque<String>>, // Stores client-to-client chat logs ( feature: it's available also if retrieved in a different server).
    media_storage: HashMap<String, (NodeId,String)>, // media name --> (uploader_id, associated base64 encoding as String)
    simulation_log: Arc<Mutex<Vec<String>>>, //used for connection server log --> GUI log interface
    pub shared_senders: Option<Arc<Mutex<HashMap<(NodeId, NodeId), Sender<Packet>>>>>, //knowledge of the whole network node-to-node links
    shortcut_receiver: Option<Receiver<Packet>>, // added to receive packets from sc (shortcut)
}
```
---

## 🔁 Core Method: `run(gui_buffer_input)`
Main event loop:
First thing done: analyze the network. --> self.initiate_network_discovery()
- Polls GUI messages and drains from the server's buffer the messages which are handled with the `process_gui_messages(...)`

- Listens for packets and dispatches to handlers:
    - `MsgFragment` → sends ack and calls self.handle_fragment(...)
    - `Ack` → no action is taken
    - `Nack` → calls self.handle_nack(...)
    - `FloodRequest` →  calls self.handle_flood_request(...)
    - `FloodResponse` → calls self.handle_flood_response(...)
- Takes packets passed by the simulation controller if there are problems with the incoming packets routing.<br>
  Note that the shortcut_receiver can only pass : Ack, Nack, FloodResponse packets. <br>
  All of them are treated with the logic od when they are received normally form the network.
- `process_gui_messages(...)`: this function takes the message passed by the GUI and strips it.<br>
  There are 2 main cases:

       
      "[MediaBroadcast]::"
      
  
- For which the server performs a Broadcast of the media passed to all the clients registered in the server.
    
        
      "[FloodRequired]::"
       
- This is the message which alarms the server that there were some changes in the topology of the network. <br>
       There are 4 sub-cases : <br>
       
       
---
### Connection thread on Log Gui:
#### `fn attach_log()`
Used to link self.simulation_log to the log thread.
 ``` rust 
    fn log(&self, message: impl ToString) {
        if let Ok(mut log) = self.simulation_log.lock() {
            log.push(message.to_string());
        }
    }
   ```
function to lock the thread when there is a self.log(...) method call.

## ✉️ Fragment and Message Handling

### `handle_fragment(session_id, fragment, routing_header)`
This method is responsible for:
- Storing incoming packet fragments into "received_fragments"
- If a fragment is already received --> warn!() is triggered
- If not, stores it as value: Some(fragment.data)
- Last fragment received --> update "fragment_lengths" hashmaps
- Check if all fragment have been received
- On full reassembly, calls `handle_complete_message`.

### `handle_complete_message((session_id, src_id), routing_header)`
1 step: Remove and retrieve the full list of fragments for the session_id and sender_id key.<br>
2 step: Calculates the true total length of the complete message. <br>
3 step: Initializes a buffer message with enough space for the full payload. <br>
4 step: Iterates over all fragments, unwrapping and appending their bytes to the message buffer. <br>
5 step: Converts the reassembled binary message into a UTF-8 String. <br>
6 step: Splits the string using "::" as a delimiter --> (format convention used) <br>

Parses command-based messages:
- `[Login]::server_id` : registers client_id into server.registered_clients and sends a  format!("[LoginAck]::session_id") as a response to client
  Example login from console: <br>
  ![img.png](imgs_terminal_server%2Fimg.png)
  ![img_1.png](imgs_terminal_server%2Fimg_1.png)
  <br>
- `[ClientListRequest]`: sends a  format!("[ClientListResponse]::{}",clients)
  Example from console:
  ![img_2.png](imgs_terminal_server%2Fimg_2.png)
  <br>
    Example from console:
  ![img_2.png](imgs_terminal_server%2Fimg_2.png)
 <br>

- `[MessageTo]::target_id::msg`: sends to target_id  format!("[MessageFrom]::{}::{}", client_id, msg). It then update self.chat_history inserting at the end of the queue the msg to the client1 - client2 entry.
  
- `[ChatRequest]::target_id` : triggers a  format!("[ChatStart]::{}",success) message to client

- `[ChatRequest]::target_id` : triggers a  format!("[ChatStart]::{}",success) message to client

- `[HistoryRequest]::src_id::tgt_id`: when clients wants to see chronology sends  format!("[HistoryResponse]::{}", response) <br>
  Output from client:
- `[HistoryRequest]::src_id::tgt_id`: when clients wants to see chronology sends  format!("[HistoryResponse]::{}", response) <br>
  Output from client: 
  ![img_6.png](imgs_terminal_server%2Fimg_6.png)
- `[ChatHistoryUpdate]::src_server::serialized_entry`: used to take updates of chat histories from other servers 🚨🚨🚨🚨
  ![img_3.png](imgs_terminal_server%2Fimg_3.png)
  ![img_4.png](imgs_terminal_server%2Fimg_4.png)
- `[MediaUpload]::media_name::base64`: insert in media_storage a media --> sends format!("[MediaUploadAck]::{}", media_name)
- `[MediaListRequest]`: sends format("[MediaListResponse]::{}", list)  where list contains all medias available on server.
  ![img_7.png](imgs_terminal_server%2Fimg_7.png)

   ![img_7.png](imgs_terminal_server%2Fimg_7.png)
 
- `[MediaDownloadRequest]::media_name`:  sends format!("[MediaDownloadResponse]::{}::{}", media_name, base64_data)

- `[MediaBroadcast]::media_name::base64_data`: sends to all clients in self.registered_clients a format!("[MediaDownloadResponse]::{}::{}", media_name, base64_data),
  then it sends an acknowledgement format!("[MediaBroadcastAck]::{}::Broadcasted", media_name)

- `[ChatFinish]::target_client`: when a ChatFinish is received the server takes the chronology between target_client e client.id and sends to every server in the topology a format!("[ChatHistoryUpdate]::{}::{}", self.id, serialized), where serialized is the complete chat history between the pair of clients.

- `[Logout]`: removes from registered_clients hashmap the client.id 👀👀👀👀
  ![img_8.png](imgs_terminal_server%2Fimg_8.png)

---
### `send_chat_message(session_id, target_id, msg)`

Sends a message from the server to a client by splitting it into fragments, computing the best route, and transmitting the fragments via the appropriate neighbors.
<br> ***Purpose:*** <br>
Messages that are too large to fit in a single packet are fragmented into 128-byte chunks, routed to the target using the shortest path, and sent one-by-one through the network.

- Fragments the message.
- Computes best path.
- Sends to next hop. ️✈️️✈️️✈️️✈️
- Records in `sent_fragments` for potential retransmit due to Nack .

### `send_ack(packet, fragment)`
- Builds an ACK packet and sends it along reversed route.

---

## 🚨 Error Recovery

### `handle_nack(session_id, nack, packet, header)`
- For `NackType::Dropped`: increase graph weight calling self.network_graph.increment_drop( from, to) and resend the packet using new best path ⌚⌚⌚⌚.
  Example from console log:
  ![img_9.png](imgs_terminal_server%2Fimg_9.png)
- For `NackType::ErrorInRouting`: remove crashed drone by calling self.network_graph.remove_node(crashed_node_id) if the drone was crashed,
- otherwise it removes the link in the network_graph and in the server's packet_sender if it is involved in the Nacked link.
- For other types: send new `FloodRequest`.

---

## 🌊 Network Discovery

### `initiate_network_discovery()`
- Broadcasts a `FloodRequest` to every neighbour is self.packet_sender with a random `flood_id`.

### `handle_flood_request(session_id, flood_request, source_routing_header)`
- If seen before → sends `FloodResponse` with server now pushed in response.path_trace
- If new → forwards `FloodRequest` to neighbors.

### `handle_flood_response(session_id, flood_response, header)`
- Integrates new links into `network_graph` by calling the `self.add_link(...)` function.


