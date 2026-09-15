//! UDP transport, reliability framing, tracing, and split-packet assembly.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use super::events::{MtpEvent, TracePacket};
use super::inbound::parse_to_client;
use super::wire::{read_u8, read_u16, read_u32};
use super::protocol::{
    CHANNEL_DEFAULT,
    CONTROLTYPE_ACK,
    CONTROLTYPE_DISCO,
    CONTROLTYPE_PING,
    CONTROLTYPE_SET_PEER_ID,
    PACKET_TYPE_CONTROL,
    PACKET_TYPE_ORIGINAL,
    PACKET_TYPE_RELIABLE,
    PACKET_TYPE_SPLIT,
    PEER_ID_INEXISTENT,
    PROTOCOL_ID,
    SEQNUM_INITIAL,
    TOCLIENT_ANNOUNCE_MEDIA,
    TOCLIENT_ITEMDEF,
    TOCLIENT_NODEDEF,
};

pub struct MtpConnection {
    socket: UdpSocket,
    addr: SocketAddr,
    pub peer_id: u16,
    send_seq: u16,
    split_buffers: HashMap<SplitKey, SplitBuffer>,
    unknown_cmds_logged: u8,
    debug_packets: bool,
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
        self.send_reliable(Vec::new(), "send dummy reliable")
    }

    /// Wrap a client command in a reliable packet and advance the outgoing sequence.
    pub(super) fn send_reliable(&mut self, payload: Vec<u8>, context: &'static str) -> Result<()> {
        let pkt = build_reliable_packet(self.peer_id, &mut self.send_seq, payload);
        self.socket.send_to(&pkt, self.addr).context(context)?;
        Ok(())
    }

    pub(super) fn send_unreliable(&self, payload: Vec<u8>, context: &'static str) -> Result<()> {
        let pkt = build_unreliable_packet(self.peer_id, payload);
        self.socket.send_to(&pkt, self.addr).context(context)?;
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

fn build_unreliable_packet(peer_id: u16, payload: Vec<u8>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + payload.len());
    buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
    buf.extend_from_slice(&peer_id.to_be_bytes());
    buf.push(CHANNEL_DEFAULT);
    buf.push(PACKET_TYPE_ORIGINAL);
    buf.extend_from_slice(&payload);
    buf
}

fn build_reliable_packet(peer_id: u16, send_seq: &mut u16, payload: Vec<u8>) -> Vec<u8> {
    let mut buf = Vec::with_capacity(11 + payload.len());
    buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
    buf.extend_from_slice(&peer_id.to_be_bytes());
    buf.push(CHANNEL_DEFAULT);
    buf.push(PACKET_TYPE_RELIABLE);
    buf.extend_from_slice(&send_seq.to_be_bytes());
    buf.push(PACKET_TYPE_ORIGINAL);
    buf.extend_from_slice(&payload);
    *send_seq = send_seq.wrapping_add(1);
    buf
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::protocol::{TOSERVER_INIT2, TOSERVER_PLAYERITEM, TOSERVER_PLAYERPOS};

    fn header(packet_type: u8) -> Vec<u8> {
        let mut expected = PROTOCOL_ID.to_be_bytes().to_vec();
        expected.extend_from_slice(&0x2345u16.to_be_bytes());
        expected.extend_from_slice(&[CHANNEL_DEFAULT, packet_type]);
        expected
    }

    #[test]
    fn reliable_commands_share_framing_and_wrap_sequence() {
        let mut sequence = u16::MAX;
        let packet = build_reliable_packet(0x2345, &mut sequence, TOSERVER_INIT2.to_be_bytes().to_vec());
        let mut expected = header(PACKET_TYPE_RELIABLE);
        expected.extend_from_slice(&u16::MAX.to_be_bytes());
        expected.push(PACKET_TYPE_ORIGINAL);
        expected.extend_from_slice(&TOSERVER_INIT2.to_be_bytes());
        assert_eq!(packet, expected);
        assert_eq!(sequence, 0);

        let mut payload = TOSERVER_PLAYERITEM.to_be_bytes().to_vec();
        payload.extend_from_slice(&7u16.to_be_bytes());
        let packet = build_reliable_packet(0x2345, &mut sequence, payload);
        let mut expected = header(PACKET_TYPE_RELIABLE);
        expected.extend_from_slice(&0u16.to_be_bytes());
        expected.push(PACKET_TYPE_ORIGINAL);
        expected.extend_from_slice(&TOSERVER_PLAYERITEM.to_be_bytes());
        expected.extend_from_slice(&7u16.to_be_bytes());
        assert_eq!(packet, expected);
        assert_eq!(sequence, 1);
    }

    #[test]
    fn unreliable_packets_have_no_reliable_envelope() {
        let payload = TOSERVER_PLAYERPOS.to_be_bytes().to_vec();
        let packet = build_unreliable_packet(0x2345, payload.clone());
        let mut expected = header(PACKET_TYPE_ORIGINAL);
        expected.extend_from_slice(&payload);
        assert_eq!(packet, expected);
    }

    #[test]
    fn dummy_reliable_packet_contains_an_empty_original_payload() {
        let mut sequence = SEQNUM_INITIAL;
        let packet = build_reliable_packet(0x2345, &mut sequence, Vec::new());
        let mut expected = header(PACKET_TYPE_RELIABLE);
        expected.extend_from_slice(&SEQNUM_INITIAL.to_be_bytes());
        expected.push(PACKET_TYPE_ORIGINAL);
        assert_eq!(packet, expected);
        assert_eq!(sequence, SEQNUM_INITIAL.wrapping_add(1));
    }

    #[test]
    fn split_buffer_reassembles_out_of_order_chunks_without_duplicates() {
        let mut buffer = SplitBuffer::new(3);
        buffer.insert(2, vec![3]);
        buffer.insert(0, vec![1]);
        buffer.insert(2, vec![99]);
        buffer.insert(3, vec![99]);
        assert!(!buffer.is_complete());
        buffer.insert(1, vec![2]);
        assert!(buffer.is_complete());
        assert_eq!(buffer.assemble(), vec![1, 2, 3]);
    }
}
