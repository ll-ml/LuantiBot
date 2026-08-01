use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::protocol::{
    AUTH_MECHANISM_FIRST_SRP, AUTH_MECHANISM_SRP, CHANNEL_DEFAULT, CLIENT_PROTOCOL_VERSION_MIN,
    CONTROLTYPE_ACK, CONTROLTYPE_DISCO, CONTROLTYPE_PING, CONTROLTYPE_SET_PEER_ID,
    LATEST_PROTOCOL_VERSION, PACKET_TYPE_CONTROL, PACKET_TYPE_ORIGINAL, PACKET_TYPE_RELIABLE,
    PACKET_TYPE_SPLIT, PEER_ID_INEXISTENT, PROTOCOL_ID, SEQNUM_INITIAL, SER_FMT_VER_HIGHEST_READ,
    TOCLIENT_ACCESS_DENIED, TOCLIENT_ACTIVE_OBJECT_MESSAGES, TOCLIENT_ACTIVE_OBJECT_REMOVE_ADD,
    TOCLIENT_ANNOUNCE_MEDIA, TOCLIENT_AUTH_ACCEPT, TOCLIENT_BLOCKDATA, TOCLIENT_CHAT_MESSAGE,
    TOCLIENT_HELLO, TOCLIENT_ITEMDEF, TOCLIENT_MOVE_PLAYER, TOCLIENT_NODEDEF,
    TOCLIENT_MOVEMENT, TOCLIENT_SRP_BYTES_S_B, TOSERVER_CHAT_MESSAGE, TOSERVER_CLIENT_READY, TOSERVER_FIRST_SRP,
    TOSERVER_GOTBLOCKS, TOSERVER_HAVE_MEDIA, TOSERVER_INIT, TOSERVER_INIT2, TOSERVER_INTERACT,
    TOSERVER_PLAYERITEM, TOSERVER_PLAYERPOS, TOSERVER_SRP_BYTES_A, TOSERVER_SRP_BYTES_M,
    VERSION_MAJOR, VERSION_MINOR, VERSION_PATCH,
};
use crate::types::BlockPos;

#[derive(Clone, Copy, Debug, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Debug)]
pub struct PlayerState {
    pub pos: Vec3,
    pub speed: Vec3,
    pub pitch: f32,
    pub yaw: f32,
    pub movement_speed: f32,
    pub movement_dir: f32,
    pub key_pressed: u32,
    pub fov: f32,
    pub wanted_range: f32,
    pub camera_inverted: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct MovementSettings {
    pub acceleration_default: f32,
    pub acceleration_air: f32,
    pub acceleration_fast: f32,
    pub speed_walk: f32,
    pub speed_crouch: f32,
    pub speed_fast: f32,
    pub speed_climb: f32,
    pub speed_jump: f32,
    pub liquid_fluidity: f32,
    pub liquid_fluidity_smooth: f32,
    pub liquid_sink: f32,
    pub gravity: f32,
}

impl Default for MovementSettings {
    fn default() -> Self {
        Self {
            acceleration_default: 3.0,
            acceleration_air: 2.0,
            acceleration_fast: 10.0,
            speed_walk: 4.0,
            speed_crouch: 1.35,
            speed_fast: 20.0,
            speed_climb: 3.0,
            speed_jump: 6.5,
            liquid_fluidity: 1.0,
            liquid_fluidity_smooth: 0.5,
            liquid_sink: 10.0,
            gravity: 9.81,
        }
    }
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            pos: Vec3::default(),
            speed: Vec3::default(),
            pitch: 0.0,
            yaw: 0.0,
            movement_speed: 0.0,
            movement_dir: 0.0,
            key_pressed: 0,
            fov: 1.2,
            wanted_range: 128.0,
            camera_inverted: false,
        }
    }
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
    pub ao_type: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ActiveObjectMessage {
    pub id: u16,
    pub data: Vec<u8>,
}

pub struct MtpConnection {
    socket: UdpSocket,
    addr: SocketAddr,
    pub peer_id: u16,
    send_seq: u16,
    split_buffers: HashMap<SplitKey, SplitBuffer>,
    unknown_cmds_logged: u8,
    debug_packets: bool,
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

impl MtpConnection {
    pub fn new(socket: UdpSocket, addr: SocketAddr) -> Self {
        Self {
            socket,
            addr,
            peer_id: PEER_ID_INEXISTENT,
            send_seq: SEQNUM_INITIAL,
            split_buffers: HashMap::new(),
            unknown_cmds_logged: 0,
            debug_packets: false,
        }
    }

    pub fn set_debug_packets(&mut self, enabled: bool) {
        self.debug_packets = enabled;
    }

    pub fn send_dummy_reliable(&mut self) -> Result<()> {
        let payload = vec![PACKET_TYPE_ORIGINAL];
        let pkt = self.build_reliable_packet(payload, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send dummy reliable")?;
        Ok(())
    }

    pub fn send_init(&mut self, player: &str) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_INIT.to_be_bytes());
        payload.push(SER_FMT_VER_HIGHEST_READ);
        payload.extend_from_slice(&0u16.to_be_bytes());
        payload.extend_from_slice(&CLIENT_PROTOCOL_VERSION_MIN.to_be_bytes());
        payload.extend_from_slice(&LATEST_PROTOCOL_VERSION.to_be_bytes());
        write_string(&mut payload, player);

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket.send_to(&pkt, self.addr).context("send init")?;
        Ok(())
    }

    pub fn send_first_srp(&mut self, player: &str, password: &str) -> Result<()> {
        let (salt, verifier) = crate::srp::generate_srp_verifier_and_salt(player, password)?;
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_FIRST_SRP.to_be_bytes());
        write_bytes(&mut payload, &salt);
        write_bytes(&mut payload, &verifier);
        payload.push(if password.is_empty() { 1 } else { 0 });

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send first_srp")?;
        Ok(())
    }

    pub fn send_srp_a(&mut self, a_bytes: &[u8]) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_SRP_BYTES_A.to_be_bytes());
        write_bytes(&mut payload, a_bytes);
        payload.push(1u8);

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket.send_to(&pkt, self.addr).context("send srp_a")?;
        Ok(())
    }

    pub fn send_srp_m(&mut self, m: &[u8]) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_SRP_BYTES_M.to_be_bytes());
        write_bytes(&mut payload, m);

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket.send_to(&pkt, self.addr).context("send srp_m")?;
        Ok(())
    }

    pub fn send_init2(&mut self) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_INIT2.to_be_bytes());

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket.send_to(&pkt, self.addr).context("send init2")?;
        Ok(())
    }

    pub fn send_client_ready(&mut self) -> Result<()> {
        let full_ver = "luanti-proto-bot";
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_CLIENT_READY.to_be_bytes());
        payload.push(VERSION_MAJOR);
        payload.push(VERSION_MINOR);
        payload.push(VERSION_PATCH);
        payload.push(0u8);
        write_string(&mut payload, full_ver);

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send client_ready")?;
        Ok(())
    }

    pub fn send_have_media(&mut self) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_HAVE_MEDIA.to_be_bytes());
        payload.push(0u8);

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send have_media")?;
        Ok(())
    }

    pub fn send_playerpos(&mut self, state: &PlayerState) -> Result<()> {
        validate_outbound_player_state(state)?;
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_PLAYERPOS.to_be_bytes());

        let scale = 100.0;
        let debug_playerpos = std::env::var("LUANTI_DEBUG_PLAYERPOS")
            .map(|v| v == "1")
            .unwrap_or(false);
        if debug_playerpos {
            let px = (state.pos.x * scale).round() as i32;
            let py = (state.pos.y * scale).round() as i32;
            let pz = (state.pos.z * scale).round() as i32;
            let sx = (state.speed.x * scale).round() as i32;
            let sy = (state.speed.y * scale).round() as i32;
            let sz = (state.speed.z * scale).round() as i32;
            let pitch = (state.pitch.to_degrees() * 100.0).round() as i32;
            let yaw = (state.yaw.to_degrees() * 100.0).round() as i32;
            println!(
                "playerpos ints pos=({}, {}, {}) speed=({}, {}, {}) pitch={} yaw={} keys={} fov={} range={} caminv={} ms={} md={}",
                px,
                py,
                pz,
                sx,
                sy,
                sz,
                pitch,
                yaw,
                state.key_pressed,
                (state.fov * 80.0).round() as u8,
                ((state.wanted_range / 16.0).ceil().min(255.0)) as u8,
                state.camera_inverted as u8,
                state.movement_speed,
                state.movement_dir
            );
        }
        write_v3s32(&mut payload, state.pos, scale);
        write_v3s32(&mut payload, state.speed, scale);
        write_s32(&mut payload, state.pitch.to_degrees() * 100.0);
        write_s32(&mut payload, state.yaw.to_degrees() * 100.0);
        write_u32(&mut payload, state.key_pressed);
        let fov_scaled = (state.fov * 80.0).round().clamp(0.0, 255.0) as u8;
        payload.push(fov_scaled);
        let wanted = (state.wanted_range / 16.0).ceil().clamp(1.0, 255.0) as u8;
        payload.push(wanted);
        payload.push(if state.camera_inverted { 1 } else { 0 });
        write_f32(&mut payload, state.movement_speed);
        write_f32(&mut payload, state.movement_dir);

        if debug_playerpos {
            let hex = payload
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            println!("playerpos bytes: {}", hex);
        }

        let pkt = self.build_unreliable_packet(payload);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send playerpos")?;
        Ok(())
    }

    pub fn send_interact_object(
        &mut self,
        action: u8,
        object_id: u16,
        state: &PlayerState,
    ) -> Result<()> {
        validate_outbound_player_state(state)?;
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_INTERACT.to_be_bytes());
        payload.push(action);
        payload.extend_from_slice(&0u16.to_be_bytes());

        let mut pointed = Vec::new();
        pointed.push(0u8);
        pointed.push(2u8);
        pointed.extend_from_slice(&object_id.to_be_bytes());
        payload.extend_from_slice(&(pointed.len() as u32).to_be_bytes());
        payload.extend_from_slice(&pointed);

        write_playerpos_fields(&mut payload, state);

        if std::env::var("LUANTI_DEBUG_INTERACT")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            let hex = payload
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "interact action={} object_id={} bytes={}",
                action, object_id, hex
            );
        }

        let pkt = self.build_reliable_packet(payload, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send interact")?;
        Ok(())
    }

    pub fn send_gotblocks(&mut self, blocks: &[BlockPos]) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let count = blocks.len().min(u8::MAX as usize) as u8;
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_GOTBLOCKS.to_be_bytes());
        payload.push(count);
        for pos in blocks.iter().take(count as usize) {
            write_v3s16(&mut payload, *pos);
        }

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send gotblocks")?;
        Ok(())
    }

    pub fn send_playeritem(&mut self, item: u16) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_PLAYERITEM.to_be_bytes());
        payload.extend_from_slice(&item.to_be_bytes());

        let pkt = self.build_reliable_packet(payload, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send playeritem")?;
        Ok(())
    }

    pub fn send_chat_message(&mut self, message: &str) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_CHAT_MESSAGE.to_be_bytes());
        write_wstring(&mut payload, message);

        let mut inner = Vec::with_capacity(1 + payload.len());
        inner.push(PACKET_TYPE_ORIGINAL);
        inner.extend_from_slice(&payload);

        let pkt = self.build_reliable_packet(inner, self.send_seq);
        self.send_seq = self.send_seq.wrapping_add(1);
        self.socket
            .send_to(&pkt, self.addr)
            .context("send chat_message")?;
        Ok(())
    }

    pub fn send_control_ping(&self) -> Result<()> {
        let mut buf = Vec::with_capacity(7 + 2);
        buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
        buf.extend_from_slice(&self.peer_id.to_be_bytes());
        buf.push(CHANNEL_DEFAULT);
        buf.push(PACKET_TYPE_CONTROL);
        buf.push(CONTROLTYPE_PING);
        self.socket.send_to(&buf, self.addr).context("send ping")?;
        Ok(())
    }

    pub fn send_control_disco(&self) -> Result<()> {
        let mut buf = Vec::with_capacity(7 + 2);
        buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
        buf.extend_from_slice(&self.peer_id.to_be_bytes());
        buf.push(CHANNEL_DEFAULT);
        buf.push(PACKET_TYPE_CONTROL);
        buf.push(CONTROLTYPE_DISCO);
        self.socket.send_to(&buf, self.addr).context("send disco")?;
        Ok(())
    }

    pub fn recv_packet(&mut self) -> Result<Option<MtpEvent>> {
        let mut recv = vec![0u8; 65535];
        let (len, source) = match self.socket.recv_from(&mut recv) {
            Ok(v) => v,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => return Ok(None),
            Err(err) => return Err(err).context("recv packet"),
        };
        if source != self.addr {
            return Ok(None);
        }

        if len < 8 {
            return Ok(None);
        }
        let mut offset = 0;
        let proto = read_u32(&recv, &mut offset)?;
        if proto != PROTOCOL_ID {
            return Ok(None);
        }
        let _sender_peer = read_u16(&recv, &mut offset)?;
        let channel = read_u8(&recv, &mut offset)?;
        let pkt_type = read_u8(&recv, &mut offset)?;

        match pkt_type {
            PACKET_TYPE_CONTROL => {
                let control = read_u8(&recv, &mut offset)?;
                if control == CONTROLTYPE_SET_PEER_ID {
                    let peer_id = read_u16(&recv, &mut offset)?;
                    return Ok(Some(MtpEvent::SetPeerId(peer_id)));
                }
            }
            PACKET_TYPE_SPLIT => {
                if offset + 6 > len {
                    return Ok(None);
                }
                let seqnum = read_u16(&recv, &mut offset)?;
                let chunk_count = read_u16(&recv, &mut offset)?;
                let chunk_num = read_u16(&recv, &mut offset)?;
                let data = recv[offset..len].to_vec();
                if let Some(assembled) =
                    self.handle_split(channel, false, seqnum, chunk_count, chunk_num, data)
                {
                    return self.process_split_payload(&assembled);
                }
            }
            PACKET_TYPE_RELIABLE => {
                let seqnum = read_u16(&recv, &mut offset)?;
                self.send_ack(channel, seqnum)?;
                let inner_type = read_u8(&recv, &mut offset)?;
                match inner_type {
                    PACKET_TYPE_CONTROL => {
                        let control = read_u8(&recv, &mut offset)?;
                        if control == CONTROLTYPE_SET_PEER_ID {
                            let peer_id = read_u16(&recv, &mut offset)?;
                            return Ok(Some(MtpEvent::SetPeerId(peer_id)));
                        }
                    }
                    PACKET_TYPE_SPLIT => {
                        if offset + 6 > len {
                            return Ok(None);
                        }
                        let seqnum = read_u16(&recv, &mut offset)?;
                        let chunk_count = read_u16(&recv, &mut offset)?;
                        let chunk_num = read_u16(&recv, &mut offset)?;
                        let data = recv[offset..len].to_vec();
                        if let Some(assembled) =
                            self.handle_split(channel, true, seqnum, chunk_count, chunk_num, data)
                        {
                            return self.process_split_payload(&assembled);
                        }
                    }
                    PACKET_TYPE_ORIGINAL => {
                        if offset + 2 > len {
                            return Ok(None);
                        }
                        let cmd = read_u16(&recv, &mut offset)?;
                        self.log_cmd(cmd, len - offset);
                        let payload = &recv[offset..len];
                        return Ok(parse_to_client(cmd, payload));
                    }
                    _ => {}
                }
            }
            PACKET_TYPE_ORIGINAL => {
                let cmd = read_u16(&recv, &mut offset)?;
                self.log_cmd(cmd, len - offset);
                let payload = &recv[offset..len];
                return Ok(parse_to_client(cmd, payload));
            }
            _ => {}
        }

        Ok(None)
    }

    pub fn recv_packet_trace(&mut self) -> Result<Option<(TracePacket, Option<MtpEvent>)>> {
        let mut recv = vec![0u8; 65535];
        let (len, source) = match self.socket.recv_from(&mut recv) {
            Ok(v) => v,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => return Ok(None),
            Err(err) => return Err(err).context("recv packet"),
        };
        if source != self.addr {
            return Ok(None);
        }

        if len < 8 {
            return Ok(None);
        }
        let mut offset = 0;
        let proto = read_u32(&recv, &mut offset)?;
        if proto != PROTOCOL_ID {
            return Ok(None);
        }
        let _sender_peer = read_u16(&recv, &mut offset)?;
        let channel = read_u8(&recv, &mut offset)?;
        let pkt_type = read_u8(&recv, &mut offset)?;

        let mut trace = TracePacket {
            channel,
            packet_type: pkt_type,
            reliable_seq: None,
            control_type: None,
            split_seq: None,
            split_chunk: None,
            split_count: None,
            cmd: None,
            payload_len: None,
        };

        match pkt_type {
            PACKET_TYPE_CONTROL => {
                let control = read_u8(&recv, &mut offset)?;
                trace.control_type = Some(control);
                if control == CONTROLTYPE_SET_PEER_ID {
                    let peer_id = read_u16(&recv, &mut offset)?;
                    return Ok(Some((trace, Some(MtpEvent::SetPeerId(peer_id)))));
                }
                return Ok(Some((trace, None)));
            }
            PACKET_TYPE_SPLIT => {
                if offset + 6 > len {
                    return Ok(Some((trace, None)));
                }
                let seqnum = read_u16(&recv, &mut offset)?;
                let chunk_count = read_u16(&recv, &mut offset)?;
                let chunk_num = read_u16(&recv, &mut offset)?;
                trace.split_seq = Some(seqnum);
                trace.split_count = Some(chunk_count);
                trace.split_chunk = Some(chunk_num);
                let data = recv[offset..len].to_vec();
                if let Some(assembled) =
                    self.handle_split(channel, false, seqnum, chunk_count, chunk_num, data)
                {
                    return self.process_split_trace(channel, assembled);
                }
                return Ok(Some((trace, None)));
            }
            PACKET_TYPE_RELIABLE => {
                let seqnum = read_u16(&recv, &mut offset)?;
                trace.reliable_seq = Some(seqnum);
                self.send_ack(channel, seqnum)?;
                let inner_type = read_u8(&recv, &mut offset)?;
                trace.packet_type = inner_type;
                match inner_type {
                    PACKET_TYPE_CONTROL => {
                        let control = read_u8(&recv, &mut offset)?;
                        trace.control_type = Some(control);
                        if control == CONTROLTYPE_SET_PEER_ID {
                            let peer_id = read_u16(&recv, &mut offset)?;
                            return Ok(Some((trace, Some(MtpEvent::SetPeerId(peer_id)))));
                        }
                        return Ok(Some((trace, None)));
                    }
                    PACKET_TYPE_SPLIT => {
                        if offset + 6 > len {
                            return Ok(Some((trace, None)));
                        }
                        let split_seq = read_u16(&recv, &mut offset)?;
                        let chunk_count = read_u16(&recv, &mut offset)?;
                        let chunk_num = read_u16(&recv, &mut offset)?;
                        trace.split_seq = Some(split_seq);
                        trace.split_count = Some(chunk_count);
                        trace.split_chunk = Some(chunk_num);
                        let data = recv[offset..len].to_vec();
                        if let Some(assembled) = self.handle_split(
                            channel,
                            true,
                            split_seq,
                            chunk_count,
                            chunk_num,
                            data,
                        ) {
                            return self.process_split_trace(channel, assembled);
                        }
                        return Ok(Some((trace, None)));
                    }
                    PACKET_TYPE_ORIGINAL => {
                        if offset + 2 > len {
                            return Ok(Some((trace, None)));
                        }
                        let cmd = read_u16(&recv, &mut offset)?;
                        trace.cmd = Some(cmd);
                        trace.payload_len = Some(len - offset);
                        let payload = &recv[offset..len];
                        return Ok(Some((trace, parse_to_client(cmd, payload))));
                    }
                    _ => return Ok(Some((trace, None))),
                }
            }
            PACKET_TYPE_ORIGINAL => {
                if offset + 2 > len {
                    return Ok(Some((trace, None)));
                }
                let cmd = read_u16(&recv, &mut offset)?;
                trace.cmd = Some(cmd);
                trace.payload_len = Some(len - offset);
                let payload = &recv[offset..len];
                return Ok(Some((trace, parse_to_client(cmd, payload))));
            }
            _ => {}
        }

        Ok(Some((trace, None)))
    }

    fn process_split_trace(
        &mut self,
        channel: u8,
        data: Vec<u8>,
    ) -> Result<Option<(TracePacket, Option<MtpEvent>)>> {
        if data.is_empty() {
            return Ok(None);
        }
        let mut offset = 0usize;
        if data[0] == PACKET_TYPE_ORIGINAL {
            offset = 1;
        }
        if offset + 2 > data.len() {
            return Ok(None);
        }
        let cmd = read_u16(&data, &mut offset)?;
        let payload_len = data.len().saturating_sub(offset);
        let trace = TracePacket {
            channel,
            packet_type: PACKET_TYPE_ORIGINAL,
            reliable_seq: None,
            control_type: None,
            split_seq: None,
            split_chunk: None,
            split_count: None,
            cmd: Some(cmd),
            payload_len: Some(payload_len),
        };
        let payload = &data[offset..];
        Ok(Some((trace, parse_to_client(cmd, payload))))
    }

    fn send_ack(&self, channel: u8, seqnum: u16) -> Result<()> {
        let mut buf = Vec::with_capacity(7 + 2 + 2);
        buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
        buf.extend_from_slice(&self.peer_id.to_be_bytes());
        buf.push(channel);
        buf.push(PACKET_TYPE_CONTROL);
        buf.push(CONTROLTYPE_ACK);
        buf.extend_from_slice(&seqnum.to_be_bytes());
        self.socket.send_to(&buf, self.addr).context("send ack")?;
        Ok(())
    }

    fn build_unreliable_packet(&self, payload: Vec<u8>) -> Vec<u8> {
        let mut buf = Vec::with_capacity(7 + payload.len());
        buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
        buf.extend_from_slice(&self.peer_id.to_be_bytes());
        buf.push(CHANNEL_DEFAULT);
        buf.push(PACKET_TYPE_ORIGINAL);
        buf.extend_from_slice(&payload);
        buf
    }

    fn build_reliable_packet(&self, inner: Vec<u8>, seqnum: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(7 + 3 + inner.len());
        buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
        buf.extend_from_slice(&self.peer_id.to_be_bytes());
        buf.push(CHANNEL_DEFAULT);
        buf.push(PACKET_TYPE_RELIABLE);
        buf.extend_from_slice(&seqnum.to_be_bytes());
        buf.extend_from_slice(&inner);
        buf
    }

    fn process_split_payload(&mut self, data: &[u8]) -> Result<Option<MtpEvent>> {
        if data.is_empty() {
            return Ok(None);
        }
        if self.debug_packets && data.len() >= 4 {
            println!(
                "split payload bytes: {:02x} {:02x} {:02x} {:02x}",
                data[0], data[1], data[2], data[3]
            );
        }
        let mut offset = 0usize;
        if data[0] == PACKET_TYPE_ORIGINAL {
            offset = 1;
        }
        if offset + 2 > data.len() {
            return Ok(None);
        }
        let cmd = read_u16(data, &mut offset)?;
        self.log_cmd(cmd, data.len() - offset);
        let payload = &data[offset..];
        Ok(parse_to_client(cmd, payload))
    }

    fn log_cmd(&mut self, cmd: u16, payload_len: usize) {
        if !self.debug_packets {
            return;
        }
        match cmd {
            TOCLIENT_NODEDEF | TOCLIENT_ITEMDEF | TOCLIENT_ANNOUNCE_MEDIA => {
                println!("recv cmd=0x{:02x} len={}", cmd, payload_len);
            }
            _ => {
                if self.unknown_cmds_logged < 5 {
                    println!("recv cmd=0x{:02x} len={}", cmd, payload_len);
                    self.unknown_cmds_logged += 1;
                }
            }
        }
    }

    fn handle_split(
        &mut self,
        channel: u8,
        reliable: bool,
        seqnum: u16,
        chunk_count: u16,
        chunk_num: u16,
        data: Vec<u8>,
    ) -> Option<Vec<u8>> {
        let now = Instant::now();
        self.split_buffers
            .retain(|_, buf| now.duration_since(buf.last_update) < Duration::from_secs(10));

        let key = SplitKey {
            channel,
            reliable,
            seqnum,
        };
        let entry = self
            .split_buffers
            .entry(key)
            .or_insert_with(|| SplitBuffer::new(chunk_count));
        entry.last_update = now;
        if entry.chunk_count != chunk_count {
            *entry = SplitBuffer::new(chunk_count);
        }
        if self.debug_packets && chunk_num == 0 {
            println!(
                "split seq={} chunks={} ch={} rel={}",
                seqnum, chunk_count, channel, reliable
            );
        }
        entry.insert(chunk_num, data);
        if entry.is_complete() {
            if self.debug_packets {
                println!(
                    "split seq={} complete ch={} rel={}",
                    seqnum, channel, reliable
                );
            }
            let assembled = entry.assemble();
            self.split_buffers.remove(&key);
            return Some(assembled);
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct SplitKey {
    channel: u8,
    reliable: bool,
    seqnum: u16,
}

struct SplitBuffer {
    chunk_count: u16,
    chunks: Vec<Option<Vec<u8>>>,
    received: usize,
    last_update: Instant,
}

impl SplitBuffer {
    fn new(chunk_count: u16) -> Self {
        Self {
            chunk_count,
            chunks: vec![None; chunk_count as usize],
            received: 0,
            last_update: Instant::now(),
        }
    }

    fn insert(&mut self, chunk_num: u16, data: Vec<u8>) {
        let idx = chunk_num as usize;
        if idx >= self.chunks.len() {
            return;
        }
        if self.chunks[idx].is_none() {
            self.chunks[idx] = Some(data);
            self.received += 1;
        }
    }

    fn is_complete(&self) -> bool {
        self.received == self.chunks.len()
    }

    fn assemble(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in &self.chunks {
            if let Some(data) = chunk {
                out.extend_from_slice(data);
            }
        }
        out
    }
}

fn parse_to_client(cmd: u16, payload: &[u8]) -> Option<MtpEvent> {
    match cmd {
        TOCLIENT_HELLO => {
            let mut offset = 0;
            let ser_ver = read_u8_slice(payload, &mut offset).ok()?;
            let _unused = read_u16_slice(payload, &mut offset).ok()?;
            let proto_ver = read_u16_slice(payload, &mut offset).ok()?;
            let auth_mechs = read_u32_slice(payload, &mut offset).ok()?;
            let _unused_str = read_string_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::ToClientHello {
                auth_mechs,
                proto_ver,
                ser_ver,
            })
        }
        TOCLIENT_AUTH_ACCEPT => {
            let mut offset = 0;
            let _unused_pos = read_v3f32_slice(payload, &mut offset).ok()?;
            if offset + 8 > payload.len() {
                return None;
            }
            offset += 8; // map seed
            let recommended_send_interval = read_f32_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::AuthAccept {
                recommended_send_interval,
            })
        }
        TOCLIENT_MOVE_PLAYER => {
            let mut offset = 0;
            let pos = read_v3f32_slice(payload, &mut offset).ok()?;
            let pitch = read_f32_slice(payload, &mut offset).ok()?.to_radians();
            let yaw = read_f32_slice(payload, &mut offset).ok()?.to_radians();
            if std::env::var("LUANTI_DEBUG_MOVEPLAYER")
                .map(|v| v == "1")
                .unwrap_or(false)
            {
                let hex = payload
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "moveplayer pos=({:.3},{:.3},{:.3}) pitch={:.3} yaw={:.3} bytes={}",
                    pos.x, pos.y, pos.z, pitch, yaw, hex
                );
            }
            Some(MtpEvent::MovePlayer { pos, pitch, yaw })
        }
        TOCLIENT_MOVEMENT => {
            let mut offset = 0;
            let mut next = || read_f32_slice(payload, &mut offset).ok();
            Some(MtpEvent::Movement(MovementSettings {
                acceleration_default: next()?,
                acceleration_air: next()?,
                acceleration_fast: next()?,
                speed_walk: next()?,
                speed_crouch: next()?,
                speed_fast: next()?,
                speed_climb: next()?,
                speed_jump: next()?,
                liquid_fluidity: next()?,
                liquid_fluidity_smooth: next()?,
                liquid_sink: next()?,
                gravity: next()?,
            }))
        }
        TOCLIENT_BLOCKDATA => {
            let mut offset = 0;
            let pos = read_v3s16_slice(payload, &mut offset).ok()?;
            if offset > payload.len() {
                return None;
            }
            Some(MtpEvent::BlockData {
                pos,
                data: payload[offset..].to_vec(),
            })
        }
        TOCLIENT_ACTIVE_OBJECT_REMOVE_ADD => {
            let mut offset = 0;
            let remove_count = read_u16_slice(payload, &mut offset).ok()? as usize;
            let mut removed = Vec::with_capacity(remove_count);
            for _ in 0..remove_count {
                removed.push(read_u16_slice(payload, &mut offset).ok()?);
            }
            let add_count = read_u16_slice(payload, &mut offset).ok()? as usize;
            let mut added = Vec::with_capacity(add_count);
            for _ in 0..add_count {
                let id = read_u16_slice(payload, &mut offset).ok()?;
                let ao_type = read_u8_slice(payload, &mut offset).ok()?;
                let len = read_u32_slice(payload, &mut offset).ok()? as usize;
                if offset + len > payload.len() {
                    return None;
                }
                let data = payload[offset..offset + len].to_vec();
                offset += len;
                added.push(ActiveObjectInit { id, ao_type, data });
            }
            Some(MtpEvent::ActiveObjectRemoveAdd { removed, added })
        }
        TOCLIENT_ACTIVE_OBJECT_MESSAGES => {
            let mut offset = 0;
            let mut messages = Vec::new();
            while offset < payload.len() {
                let id = read_u16_slice(payload, &mut offset).ok()?;
                let len = read_u16_slice(payload, &mut offset).ok()? as usize;
                if offset + len > payload.len() {
                    return None;
                }
                let data = payload[offset..offset + len].to_vec();
                offset += len;
                messages.push(ActiveObjectMessage { id, data });
            }
            Some(MtpEvent::ActiveObjectMessages { messages })
        }
        TOCLIENT_SRP_BYTES_S_B => {
            let mut offset = 0;
            let salt = read_bytes_slice(payload, &mut offset).ok()?;
            let b = read_bytes_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::SrpBytesSB { salt, b })
        }
        TOCLIENT_NODEDEF => {
            let mut offset = 0;
            let len = read_u32_slice(payload, &mut offset).ok()? as usize;
            if offset + len > payload.len() {
                return None;
            }
            let data = payload[offset..offset + len].to_vec();
            Some(MtpEvent::NodeDef { data })
        }
        TOCLIENT_ITEMDEF => Some(MtpEvent::ItemDef),
        TOCLIENT_ANNOUNCE_MEDIA => Some(MtpEvent::MediaAnnounce),
        TOCLIENT_CHAT_MESSAGE => {
            let mut offset = 0;
            let _version = read_u8_slice(payload, &mut offset).ok()?;
            let message_type = read_u8_slice(payload, &mut offset).ok()?;
            let sender = read_wstring_slice(payload, &mut offset).ok()?;
            let message = read_wstring_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::ChatMessage {
                message_type,
                sender,
                message,
            })
        }
        TOCLIENT_ACCESS_DENIED => {
            let mut offset = 0;
            let reason = read_u8_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::AccessDenied { reason })
        }
        _ => None,
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    let mut len = s.len().min(u16::MAX as usize);
    while !s.is_char_boundary(len) {
        len -= 1;
    }
    let len = len as u16;
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&s.as_bytes()[..len as usize]);
}

fn write_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    let len = data.len().min(u16::MAX as usize) as u16;
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&data[..len as usize]);
}

fn write_wstring(buf: &mut Vec<u8>, s: &str) {
    let mut wide = Vec::new();
    for ch in s.chars() {
        let needed = ch.len_utf16();
        if wide.len() + needed > u16::MAX as usize {
            break;
        }
        let mut encoded = [0; 2];
        wide.extend_from_slice(ch.encode_utf16(&mut encoded));
    }
    let len = wide.len() as u16;
    buf.extend_from_slice(&len.to_be_bytes());
    for ch in wide {
        buf.extend_from_slice(&ch.to_be_bytes());
    }
}

fn read_u8(buf: &[u8], offset: &mut usize) -> Result<u8> {
    if *offset + 1 > buf.len() {
        bail!("read_u8 out of bounds");
    }
    let v = buf[*offset];
    *offset += 1;
    Ok(v)
}

fn read_u16(buf: &[u8], offset: &mut usize) -> Result<u16> {
    if *offset + 2 > buf.len() {
        bail!("read_u16 out of bounds");
    }
    let v = u16::from_be_bytes([buf[*offset], buf[*offset + 1]]);
    *offset += 2;
    Ok(v)
}

fn read_u32(buf: &[u8], offset: &mut usize) -> Result<u32> {
    if *offset + 4 > buf.len() {
        bail!("read_u32 out of bounds");
    }
    let v = u32::from_be_bytes([
        buf[*offset],
        buf[*offset + 1],
        buf[*offset + 2],
        buf[*offset + 3],
    ]);
    *offset += 4;
    Ok(v)
}

fn read_u8_slice(buf: &[u8], offset: &mut usize) -> Result<u8> {
    if *offset + 1 > buf.len() {
        bail!("read_u8_slice out of bounds");
    }
    let v = buf[*offset];
    *offset += 1;
    Ok(v)
}

fn read_u16_slice(buf: &[u8], offset: &mut usize) -> Result<u16> {
    if *offset + 2 > buf.len() {
        bail!("read_u16_slice out of bounds");
    }
    let v = u16::from_be_bytes([buf[*offset], buf[*offset + 1]]);
    *offset += 2;
    Ok(v)
}

fn read_u32_slice(buf: &[u8], offset: &mut usize) -> Result<u32> {
    if *offset + 4 > buf.len() {
        bail!("read_u32_slice out of bounds");
    }
    let v = u32::from_be_bytes([
        buf[*offset],
        buf[*offset + 1],
        buf[*offset + 2],
        buf[*offset + 3],
    ]);
    *offset += 4;
    Ok(v)
}

fn read_f32_slice(buf: &[u8], offset: &mut usize) -> Result<f32> {
    let raw = read_u32_slice(buf, offset)?;
    Ok(f32::from_bits(raw))
}

fn read_v3f32_slice(buf: &[u8], offset: &mut usize) -> Result<Vec3> {
    let x = read_f32_slice(buf, offset)?;
    let y = read_f32_slice(buf, offset)?;
    let z = read_f32_slice(buf, offset)?;
    Ok(Vec3 { x, y, z })
}

fn read_i16_slice(buf: &[u8], offset: &mut usize) -> Result<i16> {
    if *offset + 2 > buf.len() {
        bail!("read_i16_slice out of bounds");
    }
    let v = i16::from_be_bytes([buf[*offset], buf[*offset + 1]]);
    *offset += 2;
    Ok(v)
}

fn read_v3s16_slice(buf: &[u8], offset: &mut usize) -> Result<BlockPos> {
    let x = read_i16_slice(buf, offset)?;
    let y = read_i16_slice(buf, offset)?;
    let z = read_i16_slice(buf, offset)?;
    Ok(BlockPos { x, y, z })
}

fn write_s32(buf: &mut Vec<u8>, value: f32) {
    let v = value.round() as i32;
    buf.extend_from_slice(&v.to_be_bytes());
}

fn write_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn write_f32(buf: &mut Vec<u8>, value: f32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn write_i16(buf: &mut Vec<u8>, value: i16) {
    buf.extend_from_slice(&value.to_be_bytes());
}

fn write_v3s16(buf: &mut Vec<u8>, v: BlockPos) {
    write_i16(buf, v.x);
    write_i16(buf, v.y);
    write_i16(buf, v.z);
}

fn write_v3s32(buf: &mut Vec<u8>, v: Vec3, scale: f32) {
    let x = (v.x * scale).round() as i32;
    let y = (v.y * scale).round() as i32;
    let z = (v.z * scale).round() as i32;
    buf.extend_from_slice(&x.to_be_bytes());
    buf.extend_from_slice(&y.to_be_bytes());
    buf.extend_from_slice(&z.to_be_bytes());
}

fn validate_outbound_player_state(state: &PlayerState) -> Result<()> {
    const MAX_POSITION_BS: f32 = 500_000.0;
    const MAX_SPEED_BS: f32 = 10_000.0;
    let position_valid = [state.pos.x, state.pos.y, state.pos.z]
        .into_iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_POSITION_BS);
    let speed_valid = [state.speed.x, state.speed.y, state.speed.z]
        .into_iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_SPEED_BS);
    if !position_valid || !speed_valid || !state.pitch.is_finite() || !state.yaw.is_finite() {
        bail!(
            "refusing unsafe player state: pos=({:.3},{:.3},{:.3}) speed=({:.3},{:.3},{:.3}) pitch={:.3} yaw={:.3}",
            state.pos.x,
            state.pos.y,
            state.pos.z,
            state.speed.x,
            state.speed.y,
            state.speed.z,
            state.pitch,
            state.yaw
        );
    }
    Ok(())
}

fn write_playerpos_fields(buf: &mut Vec<u8>, state: &PlayerState) {
    let scale = 100.0;
    write_v3s32(buf, state.pos, scale);
    write_v3s32(buf, state.speed, scale);
    write_s32(buf, state.pitch.to_degrees() * 100.0);
    write_s32(buf, state.yaw.to_degrees() * 100.0);
    write_u32(buf, state.key_pressed);
    let fov_scaled = (state.fov * 80.0).round().clamp(0.0, 255.0) as u8;
    buf.push(fov_scaled);
    let wanted = (state.wanted_range / 16.0).ceil().clamp(1.0, 255.0) as u8;
    buf.push(wanted);
    buf.push(if state.camera_inverted { 1 } else { 0 });
    write_f32(buf, state.movement_speed);
    write_f32(buf, state.movement_dir);
}

fn read_string_slice(buf: &[u8], offset: &mut usize) -> Result<String> {
    let len = read_u16_slice(buf, offset)? as usize;
    if *offset + len > buf.len() {
        bail!("read_string_slice out of bounds");
    }
    let s = std::str::from_utf8(&buf[*offset..*offset + len])
        .context("utf8 string")?
        .to_string();
    *offset += len;
    Ok(s)
}

fn read_bytes_slice(buf: &[u8], offset: &mut usize) -> Result<Vec<u8>> {
    let len = read_u16_slice(buf, offset)? as usize;
    if *offset + len > buf.len() {
        bail!("read_bytes_slice out of bounds");
    }
    let out = buf[*offset..*offset + len].to_vec();
    *offset += len;
    Ok(out)
}

fn read_wstring_slice(buf: &[u8], offset: &mut usize) -> Result<String> {
    let len = read_u16_slice(buf, offset)? as usize;
    let bytes_len = len * 2;
    if *offset + bytes_len > buf.len() {
        bail!("read_wstring_slice out of bounds");
    }
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let hi = buf[*offset];
        let lo = buf[*offset + 1];
        out.push(u16::from_be_bytes([hi, lo]));
        *offset += 2;
    }
    String::from_utf16(&out).context("utf16 string")
}

pub fn auth_mechanism_supported(auth_mechs: u32) -> bool {
    auth_mechs & AUTH_MECHANISM_FIRST_SRP != 0 || auth_mechs & AUTH_MECHANISM_SRP != 0
}

pub fn auth_mechanism_choice(auth_mechs: u32) -> AuthChoice {
    if auth_mechs & AUTH_MECHANISM_FIRST_SRP != 0 {
        AuthChoice::FirstSrp
    } else {
        AuthChoice::Srp
    }
}

pub enum AuthChoice {
    FirstSrp,
    Srp,
}

pub fn should_send_client_ready(got_itemdef: bool, got_nodedef: bool) -> bool {
    got_itemdef && got_nodedef
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_f32(buf: &mut Vec<u8>, value: f32) {
        buf.extend_from_slice(&value.to_be_bytes());
    }

    #[test]
    fn move_player_uses_network_f32_values() {
        let mut payload = Vec::new();
        for value in [7000.0, 250.0, 3500.0, -15.0, 90.0] {
            push_f32(&mut payload, value);
        }

        let Some(MtpEvent::MovePlayer { pos, pitch, yaw }) =
            parse_to_client(TOCLIENT_MOVE_PLAYER, &payload)
        else {
            panic!("MOVE_PLAYER did not parse");
        };
        assert_eq!(pos.x, 7000.0);
        assert_eq!(pos.y, 250.0);
        assert_eq!(pos.z, 3500.0);
        assert!((pitch.to_degrees() + 15.0).abs() < 0.001);
        assert!((yaw.to_degrees() - 90.0).abs() < 0.001);
    }

    #[test]
    fn movement_settings_use_network_f32_values() {
        let values = [3.0, 2.0, 10.0, 4.0, 1.35, 20.0, 3.0, 6.5, 1.0, 0.5, 10.0, 9.81];
        let mut payload = Vec::new();
        for value in values {
            push_f32(&mut payload, value);
        }

        let Some(MtpEvent::Movement(settings)) = parse_to_client(TOCLIENT_MOVEMENT, &payload)
        else {
            panic!("MOVEMENT did not parse");
        };
        assert_eq!(settings.acceleration_default, 3.0);
        assert_eq!(settings.speed_walk, 4.0);
        assert_eq!(settings.speed_fast, 20.0);
        assert!((settings.gravity - 9.81).abs() < 0.001);
    }

    #[test]
    fn auth_accept_interval_uses_network_f32_value() {
        let mut payload = Vec::new();
        for value in [0.0, 0.0, 0.0] {
            push_f32(&mut payload, value);
        }
        payload.extend_from_slice(&123_u64.to_be_bytes());
        push_f32(&mut payload, 0.1);

        let Some(MtpEvent::AuthAccept {
            recommended_send_interval,
        }) = parse_to_client(TOCLIENT_AUTH_ACCEPT, &payload)
        else {
            panic!("AUTH_ACCEPT did not parse");
        };
        assert!((recommended_send_interval - 0.1).abs() < 0.001);
    }

    #[test]
    fn outbound_state_rejects_million_scale_coordinates() {
        let state = PlayerState {
            pos: Vec3 {
                x: 1_200_000.0,
                y: 250.0,
                z: 3500.0,
            },
            ..PlayerState::default()
        };
        assert!(validate_outbound_player_state(&state).is_err());
    }
}
