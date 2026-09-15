//! Events and values exchanged between the transport and bot runtime.

use crate::game::MovementSettings;
use crate::types::{BlockPos, IVec3, Vec3};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryLocation {
    CurrentPlayer,
    NodeMeta(IVec3),
}

pub enum MtpEvent {
    SetPeerId(u16),
    ToClientHello {
        auth_mechs: u32,
        proto_ver: u16,
        ser_ver: u8,
    },
    AuthAccept {
        recommended_send_interval: f32,
    },
    Movement(MovementSettings),
    MovePlayer {
        pos: Vec3,
        pitch: f32,
        yaw: f32,
    },
    SrpBytesSB {
        salt: Vec<u8>,
        b: Vec<u8>,
    },
    NodeDef {
        data: Vec<u8>,
    },
    ItemDef,
    MediaAnnounce,
    BlockData {
        pos: BlockPos,
        data: Vec<u8>,
    },
    ActiveObjectRemoveAdd {
        removed: Vec<u16>,
        added: Vec<ActiveObjectInit>,
    },
    ActiveObjectMessages {
        messages: Vec<ActiveObjectMessage>,
    },
    ChatMessage {
        message_type: u8,
        sender: String,
        message: String,
    },
    AccessDenied {
        reason: u8,
    },
}

#[derive(Clone, Debug)]
pub struct ActiveObjectInit {
    pub id: u16,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ActiveObjectMessage {
    pub id: u16,
    pub data: Vec<u8>,
}

pub struct TracePacket {
    pub channel: u8,
    pub packet_type: u8,
    pub reliable_seq: Option<u16>,
    pub control_type: Option<u8>,
    pub split_seq: Option<u16>,
    pub split_chunk: Option<u16>,
    pub split_count: Option<u16>,
    pub cmd: Option<u16>,
    pub payload_len: Option<usize>,
}
