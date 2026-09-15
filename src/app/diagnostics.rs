//! Short-lived protocol diagnostics exposed by the CLI.

use anyhow::{bail, Context, Result};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::bot::json_escape;
use crate::game::PlayerState;
use crate::network::protocol;
use crate::network::{should_send_client_ready, MtpEvent, SrpClient, TracePacket};
use super::session::{answer_srp_challenge, open_connection, start_authentication};

pub(super) fn ping_server(address: &str) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .context("set read timeout")?;

    let mut buf = Vec::with_capacity(8);
    buf.extend_from_slice(&protocol::PROTOCOL_ID.to_be_bytes());
    buf.extend_from_slice(&protocol::PEER_ID_INEXISTENT.to_be_bytes());
    buf.push(protocol::CHANNEL_DEFAULT);
    buf.push(protocol::PACKET_TYPE_ORIGINAL);

    socket.send_to(&buf, addr).context("send ping packet")?;

    let mut recv = [0u8; 1024];
    let (len, _) = socket.recv_from(&mut recv).context("recv ping response")?;
    if len < 14 {
        bail!("short response: {len} bytes");
    }

    let peer_id = u16::from_be_bytes([recv[12], recv[13]]);
    println!("server up; peer_id={peer_id}");
    Ok(())
}

pub(super) fn handshake(address: &str, player: &str) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(500))?;

    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello {
                    auth_mechs,
                    proto_ver,
                    ser_ver,
                } => {
                    println!(
                        "hello: ser_ver={} proto_ver={} auth_mechs=0x{:08x}",
                        ser_ver, proto_ver, auth_mechs
                    );
                    conn.send_control_disco()?;
                    return Ok(());
                }
                MtpEvent::AuthAccept { .. } => {
                    conn.send_control_disco()?;
                    return Ok(());
                }
                MtpEvent::NodeDef { .. }
                | MtpEvent::ItemDef
                | MtpEvent::MediaAnnounce
                | MtpEvent::Movement(_)
                | MtpEvent::ActiveObjectRemoveAdd { .. }
                | MtpEvent::ActiveObjectMessages { .. } => {}
                MtpEvent::SrpBytesSB { .. } => {}
                MtpEvent::MovePlayer { .. } => {}
                MtpEvent::BlockData { .. } => {}
                MtpEvent::AccessDenied { reason } => {
                    bail!("access denied: {reason}");
                }
                MtpEvent::ChatMessage {
                    message_type,
                    sender,
                    message,
                } => {
                    println!("chat type={} from={} msg={}", message_type, sender, message);
                }
            }
        }
    }

    bail!("handshake timed out");
}

pub(super) fn login(address: &str, player: &str, password: &str) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(500))?;

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_hello = false;
    let mut srp: Option<SrpClient> = None;

    while Instant::now() < deadline {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello { auth_mechs, .. } => {
                    saw_hello = true;
                    start_authentication(
                        &mut conn,
                        player,
                        password,
                        auth_mechs,
                        &mut srp,
                        true,
                    )?;
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    answer_srp_challenge(&mut conn, srp.as_ref(), &salt, &b, true)?;
                }
                MtpEvent::AuthAccept { .. } => {
                    println!("auth: accepted");
                    conn.send_init2()?;
                    conn.send_client_ready()?;
                    println!("auth accepted; sent INIT2 + CLIENT_READY");
                    conn.send_control_disco()?;
                    return Ok(());
                }
                MtpEvent::NodeDef { .. }
                | MtpEvent::ItemDef
                | MtpEvent::MediaAnnounce
                | MtpEvent::Movement(_)
                | MtpEvent::ActiveObjectRemoveAdd { .. }
                | MtpEvent::ActiveObjectMessages { .. } => {}
                MtpEvent::MovePlayer { .. } => {}
                MtpEvent::BlockData { .. } => {}
                MtpEvent::AccessDenied { reason } => {
                    bail!("access denied: {reason}");
                }
                MtpEvent::ChatMessage {
                    message_type,
                    sender,
                    message,
                } => {
                    println!("chat type={} from={} msg={}", message_type, sender, message);
                }
            }
        }
    }

    if !saw_hello {
        bail!("login timed out before hello");
    }
    bail!("login timed out waiting for auth accept");
}

pub(super) fn connect(address: &str, player: &str, password: &str) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(200))?;

    let mut state = PlayerState::default();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    let mut need_client_ready = false;
    let mut sent_client_ready = false;
    let mut got_itemdef = false;
    let mut got_nodedef = false;
    let mut last_send = Instant::now();
    let mut got_spawn = false;
    let mut srp: Option<SrpClient> = None;
    let mut last_ping = Instant::now();
    let mut last_send_log = Instant::now();

    loop {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    println!("set peer id: {}", peer_id);
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello { auth_mechs, .. } => {
                    println!("hello auth_mechs=0x{:08x}", auth_mechs);
                    start_authentication(
                        &mut conn,
                        player,
                        password,
                        auth_mechs,
                        &mut srp,
                        true,
                    )?;
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    answer_srp_challenge(&mut conn, srp.as_ref(), &salt, &b, true)?;
                }
                MtpEvent::AuthAccept { .. } => {
                    conn.send_init2()?;
                    need_client_ready = true;
                    ready = true;
                    println!("auth accepted; waiting for client_ready state");
                }
                MtpEvent::MovePlayer { pos, pitch, yaw } => {
                    if !got_spawn {
                        println!(
                            "spawn update pos=({:.2},{:.2},{:.2}) yaw={:.2}",
                            pos.x, pos.y, pos.z, yaw
                        );
                        state.pos = pos;
                        state.pitch = pitch;
                        state.yaw = yaw;
                        got_spawn = true;
                    }
                }
                MtpEvent::ItemDef => {
                    got_itemdef = true;
                    println!("got itemdef");
                }
                MtpEvent::NodeDef { .. } => {
                    got_nodedef = true;
                    println!("got nodedef");
                }
                MtpEvent::MediaAnnounce => {
                    println!("got media announce; sending have_media");
                    conn.send_have_media()?;
                }
                MtpEvent::Movement(_) => {}
                MtpEvent::BlockData { .. } => {}
                MtpEvent::ActiveObjectRemoveAdd { .. } | MtpEvent::ActiveObjectMessages { .. } => {}
                MtpEvent::AccessDenied { reason } => {
                    bail!("access denied: {reason}");
                }
                MtpEvent::ChatMessage {
                    message_type,
                    sender,
                    message,
                } => {
                    println!("chat type={} from={} msg={}", message_type, sender, message);
                }
            }
        }

        if !ready && Instant::now() > deadline {
            bail!("connect timed out before ready");
        }

        if need_client_ready
            && !sent_client_ready
            && should_send_client_ready(got_itemdef, got_nodedef)
        {
            conn.send_client_ready()?;
            sent_client_ready = true;
            println!("sent client_ready");
        }

        if ready && last_ping.elapsed() >= Duration::from_secs(5) {
            conn.send_control_ping()?;
            last_ping = Instant::now();
        }

        if ready && got_spawn && last_send.elapsed() >= Duration::from_millis(200) {
            conn.send_playerpos(&state)?;
            last_send = Instant::now();
            if last_send_log.elapsed() >= Duration::from_secs(2) {
                println!("sent playerpos keepalive");
                last_send_log = Instant::now();
            }
        }
    }
}

pub(super) fn send_chat(address: &str, player: &str, password: &str, message: &str) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(200))?;

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    let mut sent_client_ready = false;
    let mut srp: Option<SrpClient> = None;

    while Instant::now() < deadline {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello { auth_mechs, .. } => {
                    start_authentication(
                        &mut conn,
                        player,
                        password,
                        auth_mechs,
                        &mut srp,
                        false,
                    )?;
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    answer_srp_challenge(&mut conn, srp.as_ref(), &salt, &b, false)?;
                }
                MtpEvent::AuthAccept { .. } => {
                    conn.send_init2()?;
                    conn.send_client_ready()?;
                    sent_client_ready = true;
                    ready = true;
                }
                MtpEvent::ItemDef => {}
                MtpEvent::NodeDef { .. } => {}
                MtpEvent::MediaAnnounce => {}
                MtpEvent::Movement(_) => {}
                MtpEvent::ActiveObjectRemoveAdd { .. } | MtpEvent::ActiveObjectMessages { .. } => {}
                MtpEvent::AccessDenied { reason } => {
                    bail!("access denied: {reason}");
                }
                MtpEvent::BlockData { .. } => {}
                MtpEvent::ChatMessage { .. } => {}
                MtpEvent::MovePlayer { .. } => {}
            }
        }

        if ready && sent_client_ready {
            conn.send_chat_message(message)?;
            conn.send_control_disco()?;
            return Ok(());
        }
    }

    bail!("chat timed out before ready");
}

pub(super) fn observe(address: &str, player: &str, password: &str, seconds: u64, interval: u64) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(200))?;

    let end_time = Instant::now() + Duration::from_secs(seconds);
    let mut next_emit = Instant::now();
    let mut ready = false;
    let mut need_client_ready = false;
    let mut sent_client_ready = false;
    let mut got_itemdef = false;
    let mut got_nodedef = false;
    let mut got_spawn = false;
    let mut state = PlayerState::default();
    let mut last_chat_sender = String::new();
    let mut last_chat_message = String::new();
    let mut srp: Option<SrpClient> = None;

    while Instant::now() < end_time {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello { auth_mechs, .. } => {
                    start_authentication(
                        &mut conn,
                        player,
                        password,
                        auth_mechs,
                        &mut srp,
                        false,
                    )?;
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    answer_srp_challenge(&mut conn, srp.as_ref(), &salt, &b, false)?;
                }
                MtpEvent::AuthAccept { .. } => {
                    conn.send_init2()?;
                    need_client_ready = true;
                    ready = true;
                }
                MtpEvent::ItemDef => {
                    got_itemdef = true;
                }
                MtpEvent::NodeDef { .. } => {
                    got_nodedef = true;
                }
                MtpEvent::MediaAnnounce => {
                    conn.send_have_media()?;
                }
                MtpEvent::Movement(_) => {}
                MtpEvent::MovePlayer { pos, pitch, yaw } => {
                    if !got_spawn {
                        state.pos = pos;
                        state.pitch = pitch;
                        state.yaw = yaw;
                        got_spawn = true;
                    }
                }
                MtpEvent::BlockData { .. } => {}
                MtpEvent::ActiveObjectRemoveAdd { .. } | MtpEvent::ActiveObjectMessages { .. } => {}
                MtpEvent::ChatMessage {
                    sender, message, ..
                } => {
                    last_chat_sender = sender;
                    last_chat_message = message;
                }
                MtpEvent::AccessDenied { reason } => {
                    bail!("access denied: {reason}");
                }
            }
        }

        if need_client_ready
            && !sent_client_ready
            && should_send_client_ready(got_itemdef, got_nodedef)
        {
            conn.send_client_ready()?;
            sent_client_ready = true;
        }

        if ready && Instant::now() >= next_emit {
            let now_ms = Instant::now().elapsed().as_millis();
            println!(
                "{{\"t_ms\":{},\"ready\":{},\"spawned\":{},\"pos\":[{:.2},{:.2},{:.2}],\"yaw\":{:.3},\"last_chat\":{{\"from\":\"{}\",\"msg\":\"{}\"}}}}",
                now_ms,
                ready,
                got_spawn,
                state.pos.x,
                state.pos.y,
                state.pos.z,
                state.yaw,
                json_escape(&last_chat_sender),
                json_escape(&last_chat_message)
            );
            next_emit = Instant::now() + Duration::from_secs(interval);
        }
    }

    if conn.peer_id != protocol::PEER_ID_INEXISTENT {
        conn.send_control_disco()?;
    }

    Ok(())
}

pub(super) fn trace_session(address: &str, player: &str, password: &str, seconds: u64) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(200))?;
    conn.set_debug_packets(false);

    let end_time = Instant::now() + Duration::from_secs(seconds);
    let mut ready = false;
    let mut need_client_ready = false;
    let mut sent_client_ready = false;
    let mut got_itemdef = false;
    let mut got_nodedef = false;
    let mut got_spawn = false;
    let mut state = PlayerState::default();
    let mut srp: Option<SrpClient> = None;
    let mut last_ping = Instant::now();
    let mut last_send = Instant::now();

    let start = Instant::now();
    while Instant::now() < end_time {
        if let Some((trace, event)) = conn.recv_packet_trace()? {
            let t_ms = start.elapsed().as_millis();
            print_trace_json(t_ms, &trace, event.as_ref());

            if let Some(ev) = event {
                match ev {
                    MtpEvent::SetPeerId(peer_id) => {
                        conn.peer_id = peer_id;
                        conn.send_init(player)?;
                    }
                    MtpEvent::ToClientHello { auth_mechs, .. } => {
                        start_authentication(
                            &mut conn,
                            player,
                            password,
                            auth_mechs,
                            &mut srp,
                            false,
                        )?;
                    }
                    MtpEvent::SrpBytesSB { salt, b } => {
                        answer_srp_challenge(&mut conn, srp.as_ref(), &salt, &b, false)?;
                    }
                    MtpEvent::AuthAccept { .. } => {
                        conn.send_init2()?;
                        need_client_ready = true;
                        ready = true;
                    }
                    MtpEvent::ItemDef => {
                        got_itemdef = true;
                    }
                    MtpEvent::NodeDef { .. } => {
                        got_nodedef = true;
                    }
                    MtpEvent::MediaAnnounce => {
                        conn.send_have_media()?;
                    }
                    MtpEvent::Movement(_) => {}
                    MtpEvent::MovePlayer { pos, pitch, yaw } => {
                        if !got_spawn {
                            state.pos = pos;
                            state.pitch = pitch;
                            state.yaw = yaw;
                            got_spawn = true;
                        }
                    }
                    MtpEvent::ChatMessage { .. } => {}
                    MtpEvent::BlockData { .. } => {}
                    MtpEvent::ActiveObjectRemoveAdd { .. }
                    | MtpEvent::ActiveObjectMessages { .. } => {}
                    MtpEvent::AccessDenied { reason } => {
                        bail!("access denied: {reason}");
                    }
                }
            }
        }

        if need_client_ready
            && !sent_client_ready
            && should_send_client_ready(got_itemdef, got_nodedef)
        {
            conn.send_client_ready()?;
            sent_client_ready = true;
        }

        if ready && got_spawn && last_send.elapsed() >= Duration::from_millis(200) {
            conn.send_playerpos(&state)?;
            last_send = Instant::now();
        }

        if ready && last_ping.elapsed() >= Duration::from_secs(5) {
            conn.send_control_ping()?;
            last_ping = Instant::now();
        }
    }

    if conn.peer_id != protocol::PEER_ID_INEXISTENT {
        conn.send_control_disco()?;
    }

    Ok(())
}

fn print_trace_json(t_ms: u128, trace: &TracePacket, event: Option<&MtpEvent>) {
    let mut out = String::new();
    out.push_str("{");
    out.push_str(&format!("\"t_ms\":{},", t_ms));
    out.push_str(&format!("\"channel\":{},", trace.channel));
    out.push_str(&format!("\"packet_type\":{},", trace.packet_type));
    if let Some(seq) = trace.reliable_seq {
        out.push_str(&format!("\"reliable_seq\":{},", seq));
    }
    if let Some(ctrl) = trace.control_type {
        out.push_str(&format!("\"control_type\":{},", ctrl));
    }
    if let Some(split_seq) = trace.split_seq {
        out.push_str(&format!("\"split_seq\":{},", split_seq));
        if let Some(split_chunk) = trace.split_chunk {
            out.push_str(&format!("\"split_chunk\":{},", split_chunk));
        }
        if let Some(split_count) = trace.split_count {
            out.push_str(&format!("\"split_count\":{},", split_count));
        }
    }
    if let Some(cmd) = trace.cmd {
        out.push_str(&format!("\"cmd\":{},", cmd));
    }
    if let Some(len) = trace.payload_len {
        out.push_str(&format!("\"payload_len\":{},", len));
    }

    if let Some(ev) = event {
        out.push_str("\"event\":{");
        match ev {
            MtpEvent::SetPeerId(peer_id) => {
                out.push_str("\"type\":\"SetPeerId\",");
                out.push_str(&format!("\"peer_id\":{}", peer_id));
            }
            MtpEvent::ToClientHello {
                auth_mechs,
                proto_ver,
                ser_ver,
            } => {
                out.push_str("\"type\":\"ToClientHello\",");
                out.push_str(&format!("\"auth_mechs\":{},", auth_mechs));
                out.push_str(&format!("\"proto_ver\":{},", proto_ver));
                out.push_str(&format!("\"ser_ver\":{}", ser_ver));
            }
            MtpEvent::AuthAccept {
                recommended_send_interval,
            } => {
                out.push_str("\"type\":\"AuthAccept\",");
                out.push_str(&format!(
                    "\"recommended_send_interval\":{}",
                    recommended_send_interval
                ));
            }
            MtpEvent::MovePlayer { pos, pitch, yaw } => {
                out.push_str("\"type\":\"MovePlayer\",");
                out.push_str(&format!(
                    "\"pos\":[{:.2},{:.2},{:.2}],\"pitch\":{:.2},\"yaw\":{:.2}",
                    pos.x, pos.y, pos.z, pitch, yaw
                ));
            }
            MtpEvent::SrpBytesSB { .. } => {
                out.push_str("\"type\":\"SrpBytesSB\"");
            }
            MtpEvent::NodeDef { .. } => {
                out.push_str("\"type\":\"NodeDef\"");
            }
            MtpEvent::ItemDef => {
                out.push_str("\"type\":\"ItemDef\"");
            }
            MtpEvent::MediaAnnounce => {
                out.push_str("\"type\":\"MediaAnnounce\"");
            }
            MtpEvent::Movement(_) => {
                out.push_str("\"type\":\"Movement\"");
            }
            MtpEvent::BlockData { pos, data } => {
                out.push_str("\"type\":\"BlockData\",");
                out.push_str(&format!(
                    "\"pos\":[{},{},{}],\"bytes\":{}",
                    pos.x,
                    pos.y,
                    pos.z,
                    data.len()
                ));
            }
            MtpEvent::ActiveObjectRemoveAdd { removed, added } => {
                out.push_str("\"type\":\"ActiveObjectRemoveAdd\",");
                out.push_str(&format!(
                    "\"removed\":{},\"added\":{}",
                    removed.len(),
                    added.len()
                ));
            }
            MtpEvent::ActiveObjectMessages { messages } => {
                out.push_str("\"type\":\"ActiveObjectMessages\",");
                out.push_str(&format!("\"count\":{}", messages.len()));
            }
            MtpEvent::ChatMessage {
                message_type,
                sender,
                message,
            } => {
                out.push_str("\"type\":\"ChatMessage\",");
                out.push_str(&format!("\"message_type\":{},", message_type));
                out.push_str(&format!(
                    "\"sender\":\"{}\",\"message\":\"{}\"",
                    json_escape(sender),
                    json_escape(message)
                ));
            }
            MtpEvent::AccessDenied { reason } => {
                out.push_str("\"type\":\"AccessDenied\",");
                out.push_str(&format!("\"reason\":{}", reason));
            }
        }
        out.push_str("}");
    } else {
        out.push_str("\"event\":null");
    }

    out.push_str("}");
    println!("{}", out);
}
