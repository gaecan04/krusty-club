use crossbeam_channel::Sender;
use wg_network::NodeId;
use wg_packet::Packet;

/// From controller to drone
#[derive(Debug, Clone)]
pub enum DroneCommand {
    AddSender(NodeId, Sender<Packet>),
    SetPacketDropRate(f32),
    Crash,
}

#[cfg(feature = "partial_eq")]
impl PartialEq for DroneCommand {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (DroneCommand::AddSender(node1, sender1), DroneCommand::AddSender(node2, sender2)) => {
                node1 == node2 && sender1.same_channel(sender2)
            }
            (DroneCommand::SetPacketDropRate(rate1), DroneCommand::SetPacketDropRate(rate2)) => {
                rate1 == rate2
            }
            (DroneCommand::Crash, DroneCommand::Crash) => true,
            _ => false,
        }
    }
}

/// From drone to controller
#[derive(Debug, Clone)]
#[cfg_attr(feature = "partial_eq", derive(PartialEq))]
pub enum NodeEvent {
    PacketSent(Packet),
    PacketDropped(Packet),
}
