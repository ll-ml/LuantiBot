use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::f32::consts::PI;
use std::io::{Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod agent;
mod codec;
mod mtp;
mod nodedef;
mod physics;
mod protocol;
mod srp;
mod types;
mod world;

use crate::agent::{run_agent_loop, AgentConfig};
use crate::codec::ByteReader;
use crate::types::IVec3;
use mtp::{
    auth_mechanism_choice, auth_mechanism_supported, should_send_client_ready, ActiveObjectInit,
    ActiveObjectMessage, AuthChoice, MtpConnection, MtpEvent, PlayerState, TracePacket, Vec3,
};
use nodedef::parse_nodedef_zstd;
use physics::{snap_to_ground_height, step_player_bs, InputState, PlayerCollider};
use srp::SrpClient;
use world::World;

#[derive(Parser)]
#[command(name = "luanti-proto-bot")]
#[command(about = "Minimal Luanti protocol client", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ping a Luanti server (UDP) and read peer id
    Ping { address: String },
    /// Perform a minimal handshake and read TOCLIENT_HELLO
    Handshake { address: String, player: String },
    /// Register/login with FIRST_SRP (empty password recommended for local dev)
    Login {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Login and stay connected, sending player position updates
    Connect {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Connect and move forward for a duration (simple demo)
    Move {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to move forward
        seconds: f32,
        /// Speed in nodes/sec
        #[arg(long, default_value = "2.0")]
        speed: f32,
    },
    /// Connect and follow the first non-self player seen in chat
    Follow {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to follow before exiting
        #[arg(long, default_value = "30")]
        seconds: f32,
        /// Speed in nodes/sec
        #[arg(long, default_value = "2.0")]
        speed: f32,
        /// Desired follow distance in nodes
        #[arg(long, default_value = "2.0")]
        distance: f32,
    },
    /// Connect, teleport to first non-self player, then send follow command
    FollowCmd {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to wait for a target
        #[arg(long, default_value = "30")]
        seconds: f32,
        /// Teleport command template (use {player})
        #[arg(long, default_value = "/teleport {player}")]
        tp_cmd: String,
        /// Follow command template (use {player})
        #[arg(long, default_value = "/bot_follow {player}")]
        follow_cmd: String,
    },
    /// Connect and stay in-game, accept chat commands
    Join {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Comma-separated list of allowed senders (empty = allow anyone)
        #[arg(long, default_value = "")]
        allow: String,
        /// Teleport command template (use {player})
        #[arg(long, default_value = "/teleport {player}")]
        tp_cmd: String,
        /// Follow command template (use {player})
        #[arg(long, default_value = "/bot_follow {player}")]
        follow_cmd: String,
        /// Stop command template
        #[arg(long, default_value = "/bot_stop")]
        stop_cmd: String,
        /// Follow speed in nodes/sec
        #[arg(long, default_value = "80.0")]
        follow_speed: f32,
        /// Follow distance in nodes
        #[arg(long, default_value = "2.0")]
        follow_distance: f32,
        /// Disable physics (float mode)
        #[arg(long, default_value_t = false)]
        float: bool,
        /// REST API address (empty disables)
        #[arg(long, default_value = "127.0.0.1:9123")]
        api_addr: String,
        /// REST API token (optional)
        #[arg(long, default_value = "")]
        api_token: String,
    },
    /// Send a chat message and exit
    Chat {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        message: String,
    },
    /// Connect and periodically emit a JSON observation
    Observe {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to run before exiting
        #[arg(long, default_value = "15")]
        seconds: u64,
        /// Observation interval in seconds
        #[arg(long, default_value = "1")]
        interval: u64,
    },
    /// Trace inbound packets as JSON
    Trace {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to run before exiting
        #[arg(long, default_value = "15")]
        seconds: u64,
    },
    /// Run an LLM agent loop using the REST API
    Agent {
        /// API base address (host:port or http://host:port)
        #[arg(long, default_value = "127.0.0.1:9123")]
        api: String,
        /// API token (optional)
        #[arg(long, default_value = "")]
        api_token: String,
        /// LLM server URL (OpenAI-compatible)
        #[arg(long, default_value = "http://127.0.0.1:8080/v1/chat/completions")]
        llm_url: String,
        /// Model name for the LLM server
        #[arg(long, default_value = "local-model")]
        model: String,
        /// Bot player name (used to ignore self chat)
        #[arg(long, default_value = "")]
        bot_name: String,
        /// Passive mode (observe + chat only)
        #[arg(long, default_value_t = false)]
        passive: bool,
        /// Observe radius
        #[arg(long, default_value = "4")]
        radius: i32,
        /// Decision interval in milliseconds
        #[arg(long, default_value = "800")]
        interval_ms: u64,
        /// LLM temperature
        #[arg(long, default_value = "0.2")]
        temperature: f32,
        /// LLM max tokens
        #[arg(long, default_value = "512")]
        max_tokens: u32,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Ping { address } => ping_server(&address),
        Commands::Handshake { address, player } => handshake(&address, &player),
        Commands::Login {
            address,
            player,
            password,
        } => login(&address, &player, &password),
        Commands::Connect {
            address,
            player,
            password,
        } => connect(&address, &player, &password),
        Commands::Move {
            address,
            player,
            password,
            seconds,
            speed,
        } => move_forward(&address, &player, &password, seconds, speed),
        Commands::Follow {
            address,
            player,
            password,
            seconds,
            speed,
            distance,
        } => follow_player(&address, &player, &password, seconds, speed, distance),
        Commands::FollowCmd {
            address,
            player,
            password,
            seconds,
            tp_cmd,
            follow_cmd,
        } => follow_command(&address, &player, &password, seconds, &tp_cmd, &follow_cmd),
        Commands::Join {
            address,
            player,
            password,
            allow,
            tp_cmd,
            follow_cmd,
            stop_cmd,
            follow_speed,
            follow_distance,
            float,
            api_addr,
            api_token,
        } => join_bot(
            &address,
            &player,
            &password,
            &allow,
            &tp_cmd,
            &follow_cmd,
            &stop_cmd,
            follow_speed,
            follow_distance,
            float,
            &api_addr,
            &api_token,
        ),
        Commands::Chat {
            address,
            player,
            password,
            message,
        } => send_chat(&address, &player, &password, &message),
        Commands::Observe {
            address,
            player,
            password,
            seconds,
            interval,
        } => observe(&address, &player, &password, seconds, interval),
        Commands::Trace {
            address,
            player,
            password,
            seconds,
        } => trace_session(&address, &player, &password, seconds),
        Commands::Agent {
            api,
            api_token,
            llm_url,
            model,
            bot_name,
            passive,
            radius,
            interval_ms,
            temperature,
            max_tokens,
        } => run_agent_loop(AgentConfig {
            api_base: api,
            api_token,
            llm_url,
            model,
            bot_name,
            passive,
            observe_radius: radius,
            interval_ms,
            temperature,
            max_tokens,
        }),
    }
}

fn ping_server(address: &str) -> Result<()> {
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

fn handshake(address: &str, player: &str) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

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
                MtpEvent::AuthAccept => {
                    conn.send_control_disco()?;
                    return Ok(());
                }
                MtpEvent::NodeDef { .. }
                | MtpEvent::ItemDef
                | MtpEvent::MediaAnnounce
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

fn login(address: &str, player: &str, password: &str) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

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
                    if !auth_mechanism_supported(auth_mechs) {
                        bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                    }
                    match auth_mechanism_choice(auth_mechs) {
                        AuthChoice::FirstSrp => {
                            println!("auth: FIRST_SRP register");
                            conn.send_first_srp(player, password)?;
                        }
                        AuthChoice::Srp => {
                            println!("auth: SRP login");
                            let client = SrpClient::new(player, password)?;
                            conn.send_srp_a(&client.a_bytes)?;
                            srp = Some(client);
                        }
                    }
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    if let Some(client) = srp.as_ref() {
                        println!("auth: got SRP S,B; sending M");
                        let m = client.process_challenge(&salt, &b)?;
                        conn.send_srp_m(&m)?;
                    }
                }
                MtpEvent::AuthAccept => {
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

fn connect(address: &str, player: &str, password: &str) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

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
                    if !auth_mechanism_supported(auth_mechs) {
                        bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                    }
                    match auth_mechanism_choice(auth_mechs) {
                        AuthChoice::FirstSrp => {
                            println!("auth: FIRST_SRP register");
                            conn.send_first_srp(player, password)?;
                        }
                        AuthChoice::Srp => {
                            println!("auth: SRP login");
                            let client = SrpClient::new(player, password)?;
                            conn.send_srp_a(&client.a_bytes)?;
                            srp = Some(client);
                        }
                    }
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    if let Some(client) = srp.as_ref() {
                        println!("auth: got SRP S,B; sending M");
                        let m = client.process_challenge(&salt, &b)?;
                        conn.send_srp_m(&m)?;
                    }
                }
                MtpEvent::AuthAccept => {
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

fn send_chat(address: &str, player: &str, password: &str, message: &str) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

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
                    if !auth_mechanism_supported(auth_mechs) {
                        bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                    }
                    match auth_mechanism_choice(auth_mechs) {
                        AuthChoice::FirstSrp => {
                            conn.send_first_srp(player, password)?;
                        }
                        AuthChoice::Srp => {
                            let client = SrpClient::new(player, password)?;
                            conn.send_srp_a(&client.a_bytes)?;
                            srp = Some(client);
                        }
                    }
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    if let Some(client) = srp.as_ref() {
                        let m = client.process_challenge(&salt, &b)?;
                        conn.send_srp_m(&m)?;
                    }
                }
                MtpEvent::AuthAccept => {
                    conn.send_init2()?;
                    conn.send_client_ready()?;
                    sent_client_ready = true;
                    ready = true;
                }
                MtpEvent::ItemDef => {}
                MtpEvent::NodeDef { .. } => {}
                MtpEvent::MediaAnnounce => {}
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

fn observe(address: &str, player: &str, password: &str, seconds: u64, interval: u64) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

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
                    if !auth_mechanism_supported(auth_mechs) {
                        bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                    }
                    match auth_mechanism_choice(auth_mechs) {
                        AuthChoice::FirstSrp => {
                            conn.send_first_srp(player, password)?;
                        }
                        AuthChoice::Srp => {
                            let client = SrpClient::new(player, password)?;
                            conn.send_srp_a(&client.a_bytes)?;
                            srp = Some(client);
                        }
                    }
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    if let Some(client) = srp.as_ref() {
                        let m = client.process_challenge(&salt, &b)?;
                        conn.send_srp_m(&m)?;
                    }
                }
                MtpEvent::AuthAccept => {
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
                escape_json(&last_chat_sender),
                escape_json(&last_chat_message)
            );
            next_emit = Instant::now() + Duration::from_secs(interval);
        }
    }

    if conn.peer_id != protocol::PEER_ID_INEXISTENT {
        conn.send_control_disco()?;
    }

    Ok(())
}

fn trace_session(address: &str, player: &str, password: &str, seconds: u64) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.set_debug_packets(false);
    conn.send_dummy_reliable()?;

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
                        if !auth_mechanism_supported(auth_mechs) {
                            bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                        }
                        match auth_mechanism_choice(auth_mechs) {
                            AuthChoice::FirstSrp => {
                                conn.send_first_srp(player, password)?;
                            }
                            AuthChoice::Srp => {
                                let client = SrpClient::new(player, password)?;
                                conn.send_srp_a(&client.a_bytes)?;
                                srp = Some(client);
                            }
                        }
                    }
                    MtpEvent::SrpBytesSB { salt, b } => {
                        if let Some(client) = srp.as_ref() {
                            let m = client.process_challenge(&salt, &b)?;
                            conn.send_srp_m(&m)?;
                        }
                    }
                    MtpEvent::AuthAccept => {
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
            MtpEvent::AuthAccept => {
                out.push_str("\"type\":\"AuthAccept\"");
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
                    escape_json(sender),
                    escape_json(message)
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

fn escape_json(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn bs_to_nodes(pos: Vec3) -> Vec3 {
    Vec3 {
        x: pos.x / 10.0,
        y: pos.y / 10.0,
        z: pos.z / 10.0,
    }
}

fn advance_position_bs(state: &mut PlayerState, input: InputState, dt: f32) {
    let speed = if input.forward {
        input.speed * 10.0
    } else {
        0.0
    };
    let dir = Vec3 {
        x: input.yaw.sin(),
        y: 0.0,
        z: input.yaw.cos(),
    };
    let desired = Vec3 {
        x: dir.x * speed,
        y: 0.0,
        z: dir.z * speed,
    };
    let smoothing = (dt * 8.0).clamp(0.0, 1.0);
    state.speed.x += (desired.x - state.speed.x) * smoothing;
    state.speed.y += (desired.y - state.speed.y) * smoothing;
    state.speed.z += (desired.z - state.speed.z) * smoothing;
    state.pos.x += state.speed.x * dt;
    state.pos.y += state.speed.y * dt;
    state.pos.z += state.speed.z * dt;
}

fn apply_ground_snap(state: &mut PlayerState, world: &World, collider: PlayerCollider) {
    let mut probe = state.pos;
    probe.y += 1.0;
    if let Some(y) = snap_to_ground_height(probe, world, collider) {
        let target = y + 0.5;
        if target >= state.pos.y {
            state.pos.y = target;
            state.speed.y = 0.0;
        } else if state.pos.y - target <= 2.0 {
            state.pos.y = target.max(state.pos.y);
            state.speed.y = 0.0;
        }
    }
}

#[derive(Clone, Debug)]
struct RemotePlayer {
    name: String,
    pos: Vec3,
}

#[derive(Clone, Debug)]
struct ChatEntry {
    id: u64,
    ts_ms: u128,
    sender: String,
    message: String,
}

#[derive(Default)]
struct ChatLog {
    next_id: u64,
    entries: VecDeque<ChatEntry>,
}

#[derive(Clone, Debug)]
struct ActiveObjectInitInfo {
    name: String,
    is_player: bool,
    pos: Vec3,
}

struct PendingAttack {
    id: u16,
    actions: Vec<u8>,
    next_idx: usize,
    send_at: Instant,
}

fn find_target_id(players: &HashMap<u16, RemotePlayer>, name: Option<&str>) -> Option<u16> {
    let target = name?;
    let target = normalize_player_name(target);
    players.iter().find_map(|(id, info)| {
        if normalize_player_name(&info.name) == target {
            Some(*id)
        } else {
            None
        }
    })
}

fn parse_active_object_init(data: &[u8]) -> Result<ActiveObjectInitInfo> {
    let mut reader = ByteReader::new(data);
    let version = reader.read_u8()?;
    if version < 1 {
        bail!("unsupported active object init version: {version}");
    }
    let name = reader.read_string16()?;
    let is_player = reader.read_u8()? != 0;
    let _id = reader.read_u16()?;
    let pos = read_v3f32(&mut reader)?;
    let _rot = read_v3f32(&mut reader)?;
    let _hp = reader.read_u16()?;
    let msg_count = reader.read_u8()? as usize;
    for _ in 0..msg_count {
        let _ = reader.read_string32()?;
    }
    Ok(ActiveObjectInitInfo {
        name: normalize_player_name(&name),
        is_player,
        pos,
    })
}

fn parse_active_object_update_position(data: &[u8]) -> Result<Option<Vec3>> {
    let mut reader = ByteReader::new(data);
    let cmd = reader.read_u8()?;
    if cmd != 1 {
        return Ok(None);
    }
    let pos = read_v3f32(&mut reader)?;
    let _vel = read_v3f32(&mut reader)?;
    let _acc = read_v3f32(&mut reader)?;
    let _rot = read_v3f32(&mut reader)?;
    let _do_interpolate = reader.read_u8()?;
    let _is_end = reader.read_u8()?;
    let _update_interval = reader.read_f32()?;
    Ok(Some(pos))
}

fn extract_chat_player(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if !trimmed.starts_with("***") {
        return None;
    }
    let rest = trimmed.trim_start_matches("***").trim();
    let joined = "joined the game";
    if let Some(pos) = rest.find(joined) {
        let name = rest[..pos].trim().trim_start_matches('@').trim();
        if !name.is_empty() {
            return Some(normalize_player_name(name));
        }
    }
    None
}

fn effective_chat_sender_message(sender: &str, message: &str) -> (String, String) {
    if !sender.trim().is_empty() {
        return (normalize_player_name(sender), message.trim().to_string());
    }
    let trimmed = message.trim();
    if let Some((name, rest)) = parse_sender_colon_message(trimmed) {
        return (normalize_player_name(&name), rest);
    }
    if let Some((name, rest)) = parse_sender_angle_message(trimmed) {
        return (normalize_player_name(&name), rest);
    }
    (String::new(), trimmed.to_string())
}

fn parse_sender_colon_message(message: &str) -> Option<(String, String)> {
    let mut parts = message.splitn(2, ':');
    let name = parts.next()?.trim();
    let rest = parts.next()?.trim();
    if name.is_empty() || rest.is_empty() {
        return None;
    }
    Some((name.to_string(), rest.to_string()))
}

fn parse_sender_angle_message(message: &str) -> Option<(String, String)> {
    if !message.starts_with('<') {
        return None;
    }
    let end = message.find('>')?;
    let name = message[1..end].trim();
    let rest = message[end + 1..].trim();
    if name.is_empty() || rest.is_empty() {
        return None;
    }
    Some((name.to_string(), rest.to_string()))
}

fn normalize_player_name(name: &str) -> String {
    let trimmed = name.trim();
    if let Some(stripped) = trimmed.strip_prefix("@__builtin)") {
        return stripped.trim().to_string();
    }
    trimmed.to_string()
}

#[derive(Clone, Debug)]
enum ControlCommand {
    Follow(String),
    Teleport(String),
    Stop,
    Where,
    Attack(String),
    AttackAll(String),
    AttackMobs(Option<i32>),
    Sleep(Option<i32>),
    Approach(String),
    Interact(String),
    Fight(String),
}

enum ApiCommand {
    Follow(String),
    Teleport(String),
    Attack(String),
    Say(String),
    Approach(String),
    Interact(String),
    Fight(String),
    Sleep(Option<i32>),
    Mine(Option<IVec3>),
    Place(Option<IVec3>),
    Drop {
        item: Option<String>,
        count: Option<u16>,
    },
    Wield(String),
    Use(Option<String>),
    Stop,
    Where,
    Move(MoveRequest),
    Observe {
        radius: i32,
        reply: mpsc::Sender<String>,
    },
    ObserveServer {
        radius: i32,
        reply: mpsc::Sender<String>,
    },
}

#[derive(Clone, Debug)]
enum MoveDirection {
    Forward,
    Backward,
    Left,
    Right,
}

#[derive(Clone, Debug)]
enum MoveSpec {
    Direction { dir: MoveDirection, steps: f32 },
    Delta { dx: f32, dy: f32, dz: f32 },
    Target { x: f32, y: Option<f32>, z: f32 },
}

#[derive(Clone, Debug)]
struct MoveRequest {
    spec: MoveSpec,
    speed: Option<f32>,
}

#[derive(Clone, Debug)]
struct MoveGoal {
    target: Vec3,
    speed: f32,
    stop_dist: f32,
}

fn parse_control_command(message: &str) -> Option<ControlCommand> {
    let trimmed = message.trim();
    if !trimmed.starts_with('!') {
        return None;
    }
    let mut parts = trimmed.split_whitespace();
    let cmd = parts.next()?.to_ascii_lowercase();
    match cmd.as_str() {
        "!follow" => {
            let target = parts.next()?.to_string();
            Some(ControlCommand::Follow(target))
        }
        "!tp" => {
            let target = parts.next()?.to_string();
            Some(ControlCommand::Teleport(target))
        }
        "!attack" => {
            let target = parts.next()?.to_string();
            Some(ControlCommand::Attack(target))
        }
        "!attackall" => {
            let target = parts.next()?.to_string();
            Some(ControlCommand::AttackAll(target))
        }
        "!attackmobs" => {
            let radius = parts.next().and_then(|v| v.parse::<i32>().ok());
            Some(ControlCommand::AttackMobs(radius))
        }
        "!sleep" => {
            let radius = parts.next().and_then(|v| v.parse::<i32>().ok());
            Some(ControlCommand::Sleep(radius))
        }
        "!approach" => {
            let target = parts.next()?.to_string();
            Some(ControlCommand::Approach(target))
        }
        "!interact" => {
            let target = parts.next()?.to_string();
            Some(ControlCommand::Interact(target))
        }
        "!fight" => {
            let target = parts.next()?.to_string();
            Some(ControlCommand::Fight(target))
        }
        "!stop" => Some(ControlCommand::Stop),
        "!where" => Some(ControlCommand::Where),
        _ => None,
    }
}

fn handle_control_command(
    conn: &mut MtpConnection,
    cmd: &ControlCommand,
    tp_cmd: &str,
    follow_cmd: &str,
    stop_cmd: &str,
    state: &PlayerState,
) -> Result<()> {
    match cmd {
        ControlCommand::Follow(target) => {
            if !follow_cmd.is_empty() {
                let msg = follow_cmd.replace("{player}", target);
                conn.send_chat_message(&msg)?;
                println!("follow command from chat: {}", msg);
            }
        }
        ControlCommand::Teleport(target) => {
            if !tp_cmd.is_empty() {
                let msg = tp_cmd.replace("{player}", target);
                conn.send_chat_message(&msg)?;
                println!("teleport command from chat: {}", msg);
            }
        }
        ControlCommand::Stop => {
            if !stop_cmd.is_empty() {
                conn.send_chat_message(stop_cmd)?;
                println!("stop command from chat: {}", stop_cmd);
            }
        }
        ControlCommand::Where => {
            let msg = format!(
                "pos=({:.2},{:.2},{:.2})",
                state.pos.x, state.pos.y, state.pos.z
            );
            conn.send_chat_message(&msg)?;
            println!("where response: {}", msg);
        }
        ControlCommand::Attack(_) => {}
        ControlCommand::AttackAll(_) => {}
        ControlCommand::AttackMobs(_) => {}
        ControlCommand::Sleep(_) => {}
        ControlCommand::Approach(_) => {}
        ControlCommand::Interact(_) => {}
        ControlCommand::Fight(_) => {}
    }
    Ok(())
}

fn parse_allowlist(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter_map(|entry| {
            let name = entry.trim();
            if name.is_empty() {
                None
            } else {
                Some(normalize_player_name(name))
            }
        })
        .collect()
}

fn is_sender_allowed(allow_list: &[String], sender: &str) -> bool {
    if allow_list.is_empty() {
        return true;
    }
    let name = normalize_player_name(sender);
    allow_list.iter().any(|allowed| allowed == &name)
}

fn handle_api_commands(
    api_rx: &mpsc::Receiver<ApiCommand>,
    conn: &mut MtpConnection,
    state: &PlayerState,
    world: &World,
    players: &HashMap<u16, RemotePlayer>,
    follow_enabled: &mut bool,
    follow_target_name: &mut Option<String>,
    follow_target_id: &mut Option<u16>,
    follow_distance_override: &mut Option<f32>,
    move_goal: &mut Option<MoveGoal>,
    default_move_speed: f32,
    tp_cmd: &str,
    stop_cmd: &str,
) -> Result<()> {
    while let Ok(cmd) = api_rx.try_recv() {
        match cmd {
            ApiCommand::Follow(target) => {
                *follow_target_name = Some(normalize_player_name(&target));
                *follow_target_id = find_target_id(players, follow_target_name.as_deref());
                *follow_enabled = true;
            }
            ApiCommand::Teleport(target) => {
                let cmd = tp_cmd.replace("{player}", &target);
                if !cmd.is_empty() {
                    conn.send_chat_message(&cmd)?;
                }
            }
            ApiCommand::Attack(target) => {
                let cmd = format!("/bot_attack {}", target);
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Say(message) => {
                if !message.is_empty() {
                    conn.send_chat_message(&message)?;
                }
            }
            ApiCommand::Approach(target) => {
                let cmd = format!("/bot_approach {}", target);
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Interact(target) => {
                let cmd = format!("/bot_interact {}", target);
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Fight(target) => {
                let cmd = format!("/bot_fight {}", target);
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Sleep(radius) => {
                let r = radius.unwrap_or(6).clamp(1, 20);
                let cmd = format!("/bot_sleep {}", r);
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Mine(pos) => {
                let cmd = if let Some(pos) = pos {
                    format!("/bot_mine {} {} {}", pos.x, pos.y, pos.z)
                } else {
                    "/bot_mine".to_string()
                };
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Place(pos) => {
                let cmd = if let Some(pos) = pos {
                    format!("/bot_place {} {} {}", pos.x, pos.y, pos.z)
                } else {
                    "/bot_place".to_string()
                };
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Drop { item, count } => {
                let cmd = match (item, count) {
                    (Some(item), Some(count)) => format!("/bot_drop {} {}", item, count),
                    (Some(item), None) => format!("/bot_drop {}", item),
                    (None, _) => "/bot_drop".to_string(),
                };
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Wield(item) => {
                let cmd = format!("/bot_wield {}", item);
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Use(item) => {
                let cmd = if let Some(item) = item {
                    format!("/bot_use {}", item)
                } else {
                    "/bot_use".to_string()
                };
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::Stop => {
                *follow_enabled = false;
                *follow_target_id = None;
                *follow_target_name = None;
                *follow_distance_override = None;
                *move_goal = None;
                if !stop_cmd.is_empty() {
                    conn.send_chat_message(stop_cmd)?;
                }
            }
            ApiCommand::Where => {
                let msg = format!(
                    "pos=({:.2},{:.2},{:.2})",
                    state.pos.x, state.pos.y, state.pos.z
                );
                conn.send_chat_message(&msg)?;
            }
            ApiCommand::Observe { radius, reply } => {
                let json = build_observe_json(
                    state.pos,
                    state.yaw,
                    radius,
                    world,
                    players,
                    *follow_enabled,
                    follow_target_name.as_deref(),
                );
                let _ = reply.send(json);
            }
            ApiCommand::ObserveServer { radius, reply: _ } => {
                let cmd = format!("/bot_observe {}", radius);
                let _ = conn.send_chat_message(&cmd);
            }
            ApiCommand::Move(request) => {
                let goal = build_move_goal(state, request, default_move_speed);
                *move_goal = Some(goal);
                *follow_enabled = false;
                *follow_target_id = None;
                *follow_distance_override = None;
            }
        }
    }
    Ok(())
}

fn run_api_server(
    addr: &str,
    token: &str,
    tx: mpsc::Sender<ApiCommand>,
    last_pos: Arc<Mutex<Vec3>>,
    pending_observe: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    pending_sleep: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    pending_mine: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    pending_place: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    pending_drop: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    pending_wield: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    pending_use: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    chat_log: Arc<Mutex<ChatLog>>,
) {
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(v) => v,
        Err(err) => {
            println!("api bind failed: {}", err);
            return;
        }
    };
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mut buf = [0u8; 8192];
        let size = match stream.read(&mut buf) {
            Ok(0) | Err(_) => continue,
            Ok(n) => n,
        };
        let mut req_bytes = buf[..size].to_vec();
        let req_str = String::from_utf8_lossy(&req_bytes).to_string();
        if !req_str.contains("\r\n\r\n") {
            continue;
        }
        let mut lines = req_str.lines();
        let request_line = match lines.next() {
            Some(v) => v,
            None => continue,
        };
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");

        let mut auth_ok = token.is_empty();
        for line in lines.clone() {
            if line.trim().is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix("Authorization: Bearer ") {
                if rest.trim() == token {
                    auth_ok = true;
                }
            }
        }

        let body = if let Some(idx) = req_str.find("\r\n\r\n") {
            let body_start = idx + 4;
            let body = req_bytes.split_off(body_start);
            String::from_utf8_lossy(&body).to_string()
        } else {
            String::new()
        };

        let (endpoint, query) = match path.split_once('?') {
            Some((p, q)) => (p, q),
            None => (path, ""),
        };
        let params = parse_query(query);

        if !auth_ok {
            let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n");
            continue;
        }

        let mut response = "OK".to_string();
        let mut response_type = "text/plain";
        let mut handled = true;
        match (method, endpoint) {
            ("GET", "/health") => {}
            ("GET", "/where") => {
                if let Ok(pos) = last_pos.lock() {
                    response = format!("pos=({:.2},{:.2},{:.2})", pos.x, pos.y, pos.z);
                }
                let _ = tx.send(ApiCommand::Where);
            }
            ("GET", "/observe") => {
                let radius = params
                    .get("radius")
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(2)
                    .clamp(1, 8);
                let (reply_tx, reply_rx) = mpsc::channel();
                let _ = tx.send(ApiCommand::Observe {
                    radius,
                    reply: reply_tx,
                });
                match reply_rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(body) => {
                        response = body;
                        response_type = "application/json";
                    }
                    Err(_) => {
                        let _ = stream.write_all(b"HTTP/1.1 504 Gateway Timeout\r\n\r\n");
                        continue;
                    }
                }
            }
            ("GET", "/observe_server") => {
                let radius = params
                    .get("radius")
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(2)
                    .clamp(1, 8);
                let (reply_tx, reply_rx) = mpsc::channel();
                if let Ok(mut slot) = pending_observe.lock() {
                    *slot = Some(reply_tx);
                }
                let _ = tx.send(ApiCommand::ObserveServer {
                    radius,
                    reply: mpsc::channel().0,
                });
                match reply_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(body) => response = body,
                    Err(_) => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 504 Gateway Timeout\r\nContent-Type: text/plain\r\nContent-Length: 21\r\n\r\nobserve_server timeout",
                        );
                        continue;
                    }
                }
            }
            ("GET", "/chat") => {
                let since = params
                    .get("since")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                let limit = params
                    .get("limit")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(50)
                    .clamp(1, 100);
                let (entries, last_id) = collect_chat_entries(&chat_log, since, limit);
                response = build_chat_json(entries, last_id);
                response_type = "application/json";
            }
            ("POST", "/chat") => {
                let Some(payload) = parse_json_value(&body) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_json");
                    continue;
                };
                let msg = payload
                    .get("message")
                    .or_else(|| payload.get("msg"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if msg.is_empty() {
                    response = json_error("missing_message");
                    response_type = "application/json";
                } else {
                    let clipped = if msg.len() > 256 {
                        msg[..256].to_string()
                    } else {
                        msg.to_string()
                    };
                    let _ = tx.send(ApiCommand::Say(clipped));
                    response = json_ok();
                    response_type = "application/json";
                }
            }
            ("POST", "/follow") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"));
                if let Some(target) = target {
                    let _ = tx.send(ApiCommand::Follow(target));
                } else {
                    handled = false;
                }
            }
            ("POST", "/attack") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"));
                if let Some(target) = target {
                    let _ = tx.send(ApiCommand::Attack(target));
                } else {
                    handled = false;
                }
            }
            ("POST", "/approach") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"));
                if let Some(target) = target {
                    let _ = tx.send(ApiCommand::Approach(target));
                } else {
                    handled = false;
                }
            }
            ("POST", "/interact") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"));
                if let Some(target) = target {
                    let _ = tx.send(ApiCommand::Interact(target));
                } else {
                    handled = false;
                }
            }
            ("POST", "/fight") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"));
                if let Some(target) = target {
                    let _ = tx.send(ApiCommand::Fight(target));
                } else {
                    handled = false;
                }
            }
            ("POST", "/sleep") => {
                let Some(payload) = parse_json_value(&body) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_json");
                    continue;
                };
                let radius = payload
                    .get("radius")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(6)
                    .clamp(1, 20) as i32;
                let (reply_tx, reply_rx) = mpsc::channel();
                if let Ok(mut slot) = pending_sleep.lock() {
                    *slot = Some(reply_tx);
                }
                let _ = tx.send(ApiCommand::Sleep(Some(radius)));
                match reply_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(body) => {
                        if parse_json_value(&body).is_some() {
                            response = body;
                        } else {
                            response = json!({
                                "ok": false,
                                "error": "sleep_response_invalid_json",
                                "raw": body,
                            })
                            .to_string();
                        }
                        response_type = "application/json";
                    }
                    Err(_) => {
                        write_json_error(&mut stream, "504 Gateway Timeout", "sleep_timeout");
                        continue;
                    }
                }
            }
            ("POST", "/mine") => {
                let x = params.get("x").and_then(|v| v.parse::<i32>().ok());
                let y = params.get("y").and_then(|v| v.parse::<i32>().ok());
                let z = params.get("z").and_then(|v| v.parse::<i32>().ok());
                let pos = match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(IVec3 { x, y, z }),
                    _ => None,
                };
                let (reply_tx, reply_rx) = mpsc::channel();
                if let Ok(mut slot) = pending_mine.lock() {
                    *slot = Some(reply_tx);
                }
                let _ = tx.send(ApiCommand::Mine(pos));
                match reply_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(body) => {
                        response = body;
                        response_type = "application/json";
                    }
                    Err(_) => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 504 Gateway Timeout\r\nContent-Type: text/plain\r\nContent-Length: 12\r\n\r\nmine timeout",
                        );
                        continue;
                    }
                }
            }
            ("POST", "/place") => {
                let x = params.get("x").and_then(|v| v.parse::<i32>().ok());
                let y = params.get("y").and_then(|v| v.parse::<i32>().ok());
                let z = params.get("z").and_then(|v| v.parse::<i32>().ok());
                let pos = match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(IVec3 { x, y, z }),
                    _ => None,
                };
                let (reply_tx, reply_rx) = mpsc::channel();
                if let Ok(mut slot) = pending_place.lock() {
                    *slot = Some(reply_tx);
                }
                let _ = tx.send(ApiCommand::Place(pos));
                match reply_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(body) => {
                        response = body;
                        response_type = "application/json";
                    }
                    Err(_) => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 504 Gateway Timeout\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nplace timeout",
                        );
                        continue;
                    }
                }
            }
            ("POST", "/drop") => {
                let mut item = params.get("item").cloned();
                let mut count = params.get("count").and_then(|v| v.parse::<u16>().ok());
                if item.is_none() && !body.trim().is_empty() {
                    if let Some(payload) = parse_json_value(&body) {
                        if item.is_none() {
                            item = payload
                                .get("item")
                                .and_then(|v| v.as_str())
                                .map(|v| v.to_string());
                        }
                        if count.is_none() {
                            count = payload
                                .get("count")
                                .and_then(|v| v.as_u64())
                                .map(|v| v.min(u16::MAX as u64) as u16);
                        }
                    }
                }
                let (reply_tx, reply_rx) = mpsc::channel();
                if let Ok(mut slot) = pending_drop.lock() {
                    *slot = Some(reply_tx);
                }
                let _ = tx.send(ApiCommand::Drop { item, count });
                match reply_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(body) => {
                        response = body;
                        response_type = "application/json";
                    }
                    Err(_) => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 504 Gateway Timeout\r\nContent-Type: text/plain\r\nContent-Length: 12\r\n\r\ndrop timeout",
                        );
                        continue;
                    }
                }
            }
            ("POST", "/wield") => {
                let item = params
                    .get("item")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "item"));
                let Some(item) = item else {
                    handled = false;
                    continue;
                };
                let (reply_tx, reply_rx) = mpsc::channel();
                if let Ok(mut slot) = pending_wield.lock() {
                    *slot = Some(reply_tx);
                }
                let _ = tx.send(ApiCommand::Wield(item));
                match reply_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(body) => {
                        response = body;
                        response_type = "application/json";
                    }
                    Err(_) => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 504 Gateway Timeout\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nwield timeout",
                        );
                        continue;
                    }
                }
            }
            ("POST", "/use") => {
                let item = params
                    .get("item")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "item"));
                let (reply_tx, reply_rx) = mpsc::channel();
                if let Ok(mut slot) = pending_use.lock() {
                    *slot = Some(reply_tx);
                }
                let _ = tx.send(ApiCommand::Use(item));
                match reply_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(body) => {
                        response = body;
                        response_type = "application/json";
                    }
                    Err(_) => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 504 Gateway Timeout\r\nContent-Type: text/plain\r\nContent-Length: 11\r\n\r\nuse timeout",
                        );
                        continue;
                    }
                }
            }
            ("POST", "/say") => {
                let msg = params
                    .get("msg")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "message"))
                    .or_else(|| parse_json_field(&body, "msg"))
                    .unwrap_or_default();
                let trimmed = msg.trim();
                if trimmed.is_empty() {
                    response = json_error("missing_message");
                    response_type = "application/json";
                } else {
                    let clipped = if trimmed.len() > 256 {
                        trimmed[..256].to_string()
                    } else {
                        trimmed.to_string()
                    };
                    let _ = tx.send(ApiCommand::Say(clipped));
                    response = json_ok();
                    response_type = "application/json";
                }
            }
            ("POST", "/tp") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"));
                if let Some(target) = target {
                    let _ = tx.send(ApiCommand::Teleport(target));
                } else {
                    handled = false;
                }
            }
            ("POST", "/teleport") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"));
                if let Some(target) = target {
                    let _ = tx.send(ApiCommand::Teleport(target));
                } else {
                    handled = false;
                }
            }
            ("POST", "/move") => {
                let speed = params.get("speed").and_then(|v| v.parse::<f32>().ok());
                let steps = params
                    .get("steps")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(1.0);
                if let Some(dir_raw) = params.get("direction").or_else(|| params.get("dir")) {
                    if let Some(dir) = parse_move_direction(dir_raw) {
                        println!(
                            "api move: direction={} steps={} speed={:?}",
                            dir_raw, steps, speed
                        );
                        let request = MoveRequest {
                            spec: MoveSpec::Direction { dir, steps },
                            speed,
                        };
                        let _ = tx.send(ApiCommand::Move(request));
                    } else {
                        handled = false;
                    }
                } else {
                    let dx = params
                        .get("dx")
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(0.0);
                    let dy = params
                        .get("dy")
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(0.0);
                    let dz = params
                        .get("dz")
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(0.0);
                    if dx != 0.0 || dy != 0.0 || dz != 0.0 {
                        let request = MoveRequest {
                            spec: MoveSpec::Delta { dx, dy, dz },
                            speed,
                        };
                        let _ = tx.send(ApiCommand::Move(request));
                    } else {
                        handled = false;
                    }
                }
            }
            ("POST", "/move_to") => {
                let speed = params.get("speed").and_then(|v| v.parse::<f32>().ok());
                let x = params.get("x").and_then(|v| v.parse::<f32>().ok());
                let y = params.get("y").and_then(|v| v.parse::<f32>().ok());
                let z = params.get("z").and_then(|v| v.parse::<f32>().ok());
                if let (Some(x), Some(z)) = (x, z) {
                    let request = MoveRequest {
                        spec: MoveSpec::Target { x, y, z },
                        speed,
                    };
                    let _ = tx.send(ApiCommand::Move(request));
                } else {
                    handled = false;
                }
            }
            ("POST", "/stop") => {
                let _ = tx.send(ApiCommand::Stop);
            }
            _ => handled = false,
        }

        if handled {
            let body = response;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n{}",
                response_type,
                body.len(),
                body
            );
            let _ = stream.write_all(reply.as_bytes());
        } else {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        }
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(url_decode(k), url_decode(v));
        } else {
            out.insert(url_decode(pair), String::new());
        }
    }
    out
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                    out.push((h << 4 | l) as char);
                    i += 3;
                    continue;
                }
                out.push('%');
                i += 1;
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            ch => {
                out.push(ch as char);
                i += 1;
            }
        }
    }
    out
}

fn hex_val(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(ch - b'a' + 10),
        b'A'..=b'F' => Some(ch - b'A' + 10),
        _ => None,
    }
}

fn parse_json_value(body: &str) -> Option<serde_json::Value> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn json_ok() -> String {
    json!({"ok": true}).to_string()
}

fn json_error(message: &str) -> String {
    json!({"ok": false, "error": message}).to_string()
}

fn write_json_error(stream: &mut std::net::TcpStream, status: &str, message: &str) {
    let body = json_error(message);
    let reply = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    let _ = stream.write_all(reply.as_bytes());
}

fn parse_json_field(body: &str, key: &str) -> Option<String> {
    let key_pat = format!("\"{}\"", key);
    let idx = body.find(&key_pat)?;
    let rest = &body[idx + key_pat.len()..];
    let colon = rest.find(':')?;
    let mut s = rest[colon + 1..].trim_start();
    if !s.starts_with('"') {
        return None;
    }
    s = &s[1..];
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => {
                if let Some(next) = chars.next() {
                    match next {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        _ => out.push(next),
                    }
                }
            }
            _ => out.push(c),
        }
    }
    None
}

fn parse_move_direction(value: &str) -> Option<MoveDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "forward" | "fwd" => Some(MoveDirection::Forward),
        "back" | "backward" => Some(MoveDirection::Backward),
        "left" => Some(MoveDirection::Left),
        "right" => Some(MoveDirection::Right),
        _ => None,
    }
}

fn yaw_for_direction(yaw: f32, dir: &MoveDirection) -> f32 {
    match dir {
        MoveDirection::Forward => yaw,
        MoveDirection::Backward => yaw + PI,
        MoveDirection::Left => yaw - PI * 0.5,
        MoveDirection::Right => yaw + PI * 0.5,
    }
}

fn wrap_angle(mut angle: f32) -> f32 {
    while angle > PI {
        angle -= 2.0 * PI;
    }
    while angle < -PI {
        angle += 2.0 * PI;
    }
    angle
}

fn approach_angle(current: f32, target: f32, factor: f32) -> f32 {
    let diff = wrap_angle(target - current);
    current + diff * factor.clamp(0.0, 1.0)
}

fn build_move_goal(state: &PlayerState, request: MoveRequest, default_speed: f32) -> MoveGoal {
    let speed = request.speed.unwrap_or(default_speed).max(0.1);
    let stop_dist = 5.0;
    let target = match request.spec {
        MoveSpec::Direction { dir, steps } => {
            let steps = steps.max(0.0);
            let step_bs = steps * 10.0;
            let yaw = yaw_for_direction(state.yaw, &dir);
            let dir_vec = Vec3 {
                x: yaw.sin(),
                y: 0.0,
                z: yaw.cos(),
            };
            Vec3 {
                x: state.pos.x + dir_vec.x * step_bs,
                y: state.pos.y,
                z: state.pos.z + dir_vec.z * step_bs,
            }
        }
        MoveSpec::Delta { dx, dy, dz } => Vec3 {
            x: state.pos.x + dx * 10.0,
            y: state.pos.y + dy * 10.0,
            z: state.pos.z + dz * 10.0,
        },
        MoveSpec::Target { x, y, z } => Vec3 {
            x: x * 10.0,
            y: y.unwrap_or(state.pos.y / 10.0) * 10.0,
            z: z * 10.0,
        },
    };
    MoveGoal {
        target,
        speed,
        stop_dist,
    }
}

fn build_observe_json(
    pos: Vec3,
    yaw: f32,
    radius: i32,
    world: &World,
    players: &HashMap<u16, RemotePlayer>,
    follow_enabled: bool,
    follow_target: Option<&str>,
) -> String {
    let node_pos = IVec3 {
        x: (pos.x / 10.0).floor() as i32,
        y: (pos.y / 10.0).floor() as i32,
        z: (pos.z / 10.0).floor() as i32,
    };

    let facing = facing_from_yaw(yaw);
    let front = offset_from_facing(facing);
    let left = offset_from_facing(turn_left(facing));
    let right = offset_from_facing(turn_right(facing));
    let back = offset_from_facing(turn_back(facing));

    let obstacles = [
        ("front", front),
        ("left", left),
        ("right", right),
        ("back", back),
    ];

    let mut out = String::new();
    out.push('{');
    out.push_str("\"health\":null,");
    out.push_str(&format!(
        "\"position\":[{},{},{}],",
        node_pos.x, node_pos.y, node_pos.z
    ));
    out.push_str(&format!("\"facing\":\"{}\",", facing));

    out.push_str("\"nodes\":[");
    let mut first_node = true;
    for y in (node_pos.y - radius)..=(node_pos.y + radius) {
        for x in (node_pos.x - radius)..=(node_pos.x + radius) {
            for z in (node_pos.z - radius)..=(node_pos.z + radius) {
                let pos = IVec3 { x, y, z };
                let name = match world.get_node(pos) {
                    Some(node) => world.node_name(node),
                    None => "unknown".to_string(),
                };
                if !first_node {
                    out.push(',');
                }
                first_node = false;
                out.push_str(&format!(
                    "{{\"pos\":[{},{},{}],\"name\":\"{}\"}}",
                    x,
                    y,
                    z,
                    json_escape(&name)
                ));
            }
        }
    }
    out.push_str("],");

    out.push_str("\"hostiles\":[");
    out.push_str("],");

    out.push_str("\"items\":[");
    let mut first_item = true;
    for info in players.values() {
        if follow_enabled {
            if let Some(target) = follow_target {
                if info.name == target {
                    continue;
                }
            }
        }
        let dx = ((info.pos.x / 10.0).floor() as i32) - node_pos.x;
        let dy = ((info.pos.y / 10.0).floor() as i32) - node_pos.y;
        let dz = ((info.pos.z / 10.0).floor() as i32) - node_pos.z;
        if dx.abs() > radius || dy.abs() > radius || dz.abs() > radius {
            continue;
        }
        if !first_item {
            out.push(',');
        }
        first_item = false;
        out.push_str(&format!(
            "{{\"type\":\"player\",\"name\":\"{}\",\"dx\":{},\"dy\":{},\"dz\":{}}}",
            json_escape(&info.name),
            dx,
            dy,
            dz
        ));
    }
    out.push_str("],");

    out.push_str("\"obstacles\":{");
    let mut first_obs = true;
    for (label, delta) in obstacles {
        if !first_obs {
            out.push(',');
        }
        first_obs = false;
        let pos = IVec3 {
            x: node_pos.x + delta.0,
            y: node_pos.y,
            z: node_pos.z + delta.1,
        };
        let value = obstacle_kind(world, pos);
        out.push_str(&format!("\"{}\":\"{}\"", label, value));
    }
    out.push_str("},");

    if follow_enabled {
        if let Some(target) = follow_target {
            out.push_str(&format!("\"goal\":\"follow {}\"", json_escape(target)));
        } else {
            out.push_str("\"goal\":\"follow\"");
        }
    } else {
        out.push_str("\"goal\":\"idle\"");
    }
    out.push('}');
    out
}

fn facing_from_yaw(yaw: f32) -> &'static str {
    let x = yaw.sin();
    let z = yaw.cos();
    if x.abs() > z.abs() {
        if x > 0.0 {
            "east"
        } else {
            "west"
        }
    } else if z > 0.0 {
        "north"
    } else {
        "south"
    }
}

fn turn_left(facing: &str) -> &'static str {
    match facing {
        "north" => "west",
        "west" => "south",
        "south" => "east",
        "east" => "north",
        _ => "north",
    }
}

fn turn_right(facing: &str) -> &'static str {
    match facing {
        "north" => "east",
        "east" => "south",
        "south" => "west",
        "west" => "north",
        _ => "north",
    }
}

fn turn_back(facing: &str) -> &'static str {
    match facing {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        _ => "south",
    }
}

fn offset_from_facing(facing: &str) -> (i32, i32) {
    match facing {
        "north" => (0, 1),
        "south" => (0, -1),
        "east" => (1, 0),
        "west" => (-1, 0),
        _ => (0, 1),
    }
}

fn obstacle_kind(world: &World, pos: IVec3) -> &'static str {
    match world.get_node(pos) {
        Some(node) if world.is_air_or_ignore(node) => "air",
        Some(_) => "solid",
        None => "unknown",
    }
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c <= '\u{1F}' => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            _ => out.push(ch),
        }
    }
    out
}

fn log_chat(chat_log: &Arc<Mutex<ChatLog>>, sender: &str, message: &str) {
    if let Ok(mut log) = chat_log.lock() {
        let id = log.next_id;
        log.next_id = log.next_id.saturating_add(1);
        log.entries.push_back(ChatEntry {
            id,
            ts_ms: Instant::now().elapsed().as_millis(),
            sender: sender.to_string(),
            message: message.to_string(),
        });
        while log.entries.len() > 200 {
            log.entries.pop_front();
        }
    }
}

fn collect_chat_entries(
    chat_log: &Arc<Mutex<ChatLog>>,
    since: u64,
    limit: usize,
) -> (Vec<ChatEntry>, u64) {
    if let Ok(log) = chat_log.lock() {
        let mut out = Vec::new();
        for entry in log.entries.iter() {
            if entry.id > since {
                out.push(entry.clone());
                if out.len() >= limit {
                    break;
                }
            }
        }
        let last_id = log.next_id.saturating_sub(1);
        return (out, last_id);
    }
    (Vec::new(), since)
}

fn build_chat_json(entries: Vec<ChatEntry>, last_id: u64) -> String {
    let mut out = String::new();
    out.push('{');
    out.push_str(&format!("\"last\":{},\"messages\":[", last_id));
    let mut first = true;
    for entry in entries {
        if !first {
            out.push(',');
        }
        first = false;
        out.push('{');
        out.push_str(&format!("\"id\":{},\"ts_ms\":{},", entry.id, entry.ts_ms));
        out.push_str(&format!("\"from\":\"{}\",", json_escape(&entry.sender)));
        out.push_str(&format!("\"msg\":\"{}\"", json_escape(&entry.message)));
        out.push('}');
    }
    out.push_str("]}");
    out
}

fn read_v3f32(reader: &mut ByteReader) -> Result<Vec3> {
    Ok(Vec3 {
        x: reader.read_f32()?,
        y: reader.read_f32()?,
        z: reader.read_f32()?,
    })
}

fn move_forward(
    address: &str,
    player: &str,
    password: &str,
    seconds: f32,
    speed: f32,
) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

    let mut state = PlayerState::default();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = false;
    let mut need_client_ready = false;
    let mut sent_client_ready = false;
    let mut got_itemdef = false;
    let mut got_nodedef = false;
    let mut got_spawn = false;
    let mut last_send = Instant::now();
    let start_move = Instant::now();
    let mut last_wait_log = Instant::now();
    let mut srp: Option<SrpClient> = None;
    let mut world = World::new();
    let collider = PlayerCollider::default();
    let mut gotblocks_pending = Vec::new();
    let mut logged_blocks = false;
    let mut ser_ver: Option<u8> = None;
    let mut proto_ver: Option<u16> = None;
    let mut blockdata_count: u64 = 0;
    let mut last_block_log = Instant::now();

    loop {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    println!("set peer id: {}", peer_id);
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello {
                    auth_mechs,
                    ser_ver: hello_ser_ver,
                    proto_ver: hello_proto_ver,
                    ..
                } => {
                    println!("hello auth_mechs=0x{:08x}", auth_mechs);
                    ser_ver = Some(hello_ser_ver);
                    proto_ver = Some(hello_proto_ver);
                    if !auth_mechanism_supported(auth_mechs) {
                        bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                    }
                    match auth_mechanism_choice(auth_mechs) {
                        AuthChoice::FirstSrp => {
                            println!("auth: FIRST_SRP register");
                            conn.send_first_srp(player, password)?;
                        }
                        AuthChoice::Srp => {
                            println!("auth: SRP login");
                            let client = SrpClient::new(player, password)?;
                            conn.send_srp_a(&client.a_bytes)?;
                            srp = Some(client);
                        }
                    }
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    if let Some(client) = srp.as_ref() {
                        println!("auth: got SRP S,B; sending M");
                        let m = client.process_challenge(&salt, &b)?;
                        conn.send_srp_m(&m)?;
                    }
                }
                MtpEvent::AuthAccept => {
                    conn.send_init2()?;
                    need_client_ready = true;
                    ready = true;
                    println!("connected; waiting for client_ready state");
                }
                MtpEvent::ItemDef => {
                    got_itemdef = true;
                    println!("got itemdef");
                }
                MtpEvent::NodeDef { data } => {
                    got_nodedef = true;
                    let version = proto_ver.unwrap_or(protocol::LATEST_PROTOCOL_VERSION);
                    match parse_nodedef_zstd(&data, version) {
                        Ok(mut manager) => {
                            manager.resolve_crossrefs();
                            world.set_nodedef(manager);
                            println!("got nodedef");
                        }
                        Err(err) => {
                            println!("nodedef parse failed: {}", err);
                        }
                    }
                }
                MtpEvent::MediaAnnounce => {
                    println!("got media announce; sending have_media");
                    conn.send_have_media()?;
                }
                MtpEvent::ActiveObjectRemoveAdd { .. } | MtpEvent::ActiveObjectMessages { .. } => {}
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
                MtpEvent::BlockData { pos, data } => {
                    let ver = ser_ver.unwrap_or(protocol::SER_FMT_VER_HIGHEST_READ);
                    if let Err(err) = world.ingest_block(pos, &data, ver) {
                        println!("block parse failed at {:?}: {:?}", pos, err);
                    } else {
                        gotblocks_pending.push(pos);
                        blockdata_count += 1;
                        if !logged_blocks && world.block_count() > 0 {
                            println!("blockdata loaded; blocks={}", world.block_count());
                            logged_blocks = true;
                        }
                    }
                }
                MtpEvent::ChatMessage {
                    message_type,
                    sender,
                    message,
                } => {
                    println!("chat type={} from={} msg={}", message_type, sender, message);
                }
                MtpEvent::AccessDenied { reason } => {
                    bail!("access denied: {reason}");
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

        if ready && !got_spawn && last_wait_log.elapsed() >= Duration::from_secs(3) {
            println!("waiting for spawn position from server...");
            last_wait_log = Instant::now();
        }

        if ready && got_spawn && last_send.elapsed() >= Duration::from_millis(200) {
            let elapsed = start_move.elapsed().as_secs_f32();
            let dt = 0.2_f32;
            let forward = elapsed <= seconds;
            let input = InputState {
                forward,
                speed,
                yaw: state.yaw,
            };
            step_player_bs(&mut state, &world, collider, input, dt);

            if forward {
                state.key_pressed = protocol::KEY_FORWARD;
                state.movement_speed = 1.0;
                state.movement_dir = 0.0;
            } else {
                state.key_pressed = 0;
                state.movement_speed = 0.0;
            }

            conn.send_playerpos(&state)?;
            last_send = Instant::now();

            if elapsed > seconds + dt {
                conn.send_control_disco()?;
                return Ok(());
            }
        }

        if last_block_log.elapsed() >= Duration::from_secs(2) {
            println!(
                "blockdata recv={} stored={}",
                blockdata_count,
                world.block_count()
            );
            last_block_log = Instant::now();
        }

        if !gotblocks_pending.is_empty() {
            let batch = gotblocks_pending.len().min(10);
            let send = gotblocks_pending.drain(0..batch).collect::<Vec<_>>();
            conn.send_gotblocks(&send)?;
        }
    }
}

fn follow_player(
    address: &str,
    player: &str,
    password: &str,
    seconds: f32,
    speed: f32,
    distance: f32,
) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

    let mut state = PlayerState::default();
    let deadline = Instant::now() + Duration::from_secs(30);
    let end_time = Instant::now() + Duration::from_secs_f32(seconds.max(1.0));
    let mut ready = false;
    let mut need_client_ready = false;
    let mut sent_client_ready = false;
    let mut got_itemdef = false;
    let mut got_nodedef = false;
    let mut got_spawn = false;
    let mut last_send = Instant::now();
    let mut last_wait_log = Instant::now();
    let mut srp: Option<SrpClient> = None;
    let mut world = World::new();
    let collider = PlayerCollider::default();
    let mut gotblocks_pending = Vec::new();
    let mut ser_ver: Option<u8> = None;
    let mut proto_ver: Option<u16> = None;
    let mut players: HashMap<u16, RemotePlayer> = HashMap::new();
    let mut follow_target_name: Option<String> = None;
    let mut follow_target_id: Option<u16> = None;

    loop {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    println!("set peer id: {}", peer_id);
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello {
                    auth_mechs,
                    ser_ver: hello_ser_ver,
                    proto_ver: hello_proto_ver,
                    ..
                } => {
                    println!("hello auth_mechs=0x{:08x}", auth_mechs);
                    ser_ver = Some(hello_ser_ver);
                    proto_ver = Some(hello_proto_ver);
                    if !auth_mechanism_supported(auth_mechs) {
                        bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                    }
                    match auth_mechanism_choice(auth_mechs) {
                        AuthChoice::FirstSrp => {
                            println!("auth: FIRST_SRP register");
                            conn.send_first_srp(player, password)?;
                        }
                        AuthChoice::Srp => {
                            println!("auth: SRP login");
                            let client = SrpClient::new(player, password)?;
                            conn.send_srp_a(&client.a_bytes)?;
                            srp = Some(client);
                        }
                    }
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    if let Some(client) = srp.as_ref() {
                        println!("auth: got SRP S,B; sending M");
                        let m = client.process_challenge(&salt, &b)?;
                        conn.send_srp_m(&m)?;
                    }
                }
                MtpEvent::AuthAccept => {
                    conn.send_init2()?;
                    need_client_ready = true;
                    ready = true;
                    println!("connected; waiting for client_ready state");
                }
                MtpEvent::ItemDef => {
                    got_itemdef = true;
                    println!("got itemdef");
                }
                MtpEvent::NodeDef { data } => {
                    got_nodedef = true;
                    let version = proto_ver.unwrap_or(protocol::LATEST_PROTOCOL_VERSION);
                    match parse_nodedef_zstd(&data, version) {
                        Ok(mut manager) => {
                            manager.resolve_crossrefs();
                            world.set_nodedef(manager);
                            println!("got nodedef");
                        }
                        Err(err) => {
                            println!("nodedef parse failed: {}", err);
                        }
                    }
                }
                MtpEvent::MediaAnnounce => {
                    println!("got media announce; sending have_media");
                    conn.send_have_media()?;
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
                MtpEvent::BlockData { pos, data } => {
                    let ver = ser_ver.unwrap_or(protocol::SER_FMT_VER_HIGHEST_READ);
                    if world.ingest_block(pos, &data, ver).is_ok() {
                        gotblocks_pending.push(pos);
                    }
                }
                MtpEvent::ActiveObjectRemoveAdd { removed, added } => {
                    if !removed.is_empty() || !added.is_empty() {
                        println!(
                            "active objects: removed={} added={}",
                            removed.len(),
                            added.len()
                        );
                    }
                    for id in removed {
                        players.remove(&id);
                        if follow_target_id == Some(id) {
                            follow_target_id = None;
                        }
                    }
                    for ActiveObjectInit { id, data, .. } in added {
                        if let Ok(info) = parse_active_object_init(&data) {
                            if info.is_player && info.name != player {
                                println!("player object: name={} id={}", info.name, id);
                                players.insert(
                                    id,
                                    RemotePlayer {
                                        name: info.name,
                                        pos: info.pos,
                                    },
                                );
                            } else if info.is_player {
                                println!("local player object id={}", id);
                            }
                        } else {
                            println!("active object init parse failed (id={})", id);
                        }
                    }
                    if follow_target_id.is_none() {
                        follow_target_id = find_target_id(&players, follow_target_name.as_deref());
                        if let (Some(id), Some(name)) =
                            (follow_target_id, follow_target_name.as_ref())
                        {
                            println!("target resolved: {} (id={})", name, id);
                        }
                    }
                }
                MtpEvent::ActiveObjectMessages { messages } => {
                    for ActiveObjectMessage { id, data } in messages {
                        if let Ok(Some(pos)) = parse_active_object_update_position(&data) {
                            if let Some(entry) = players.get_mut(&id) {
                                entry.pos = pos;
                                if follow_target_id == Some(id) {
                                    println!(
                                        "target pos=({:.2},{:.2},{:.2})",
                                        entry.pos.x, entry.pos.y, entry.pos.z
                                    );
                                }
                            }
                        }
                    }
                }
                MtpEvent::ChatMessage {
                    sender, message, ..
                } => {
                    println!("chat from={} msg={}", sender, message);
                    if follow_target_name.is_none() {
                        if !sender.is_empty() && sender != player {
                            follow_target_name = Some(sender);
                        } else if let Some(name) = extract_chat_player(&message) {
                            if name != player {
                                follow_target_name = Some(name);
                            }
                        }
                    }
                    if let Some(ref name) = follow_target_name {
                        follow_target_id = find_target_id(&players, follow_target_name.as_deref());
                        println!("following first chat player: {}", name);
                    }
                }
                MtpEvent::AccessDenied { reason } => {
                    bail!("access denied: {reason}");
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

        if ready && !got_spawn && last_wait_log.elapsed() >= Duration::from_secs(3) {
            println!("waiting for spawn position from server...");
            last_wait_log = Instant::now();
        }

        if ready && got_spawn && last_send.elapsed() >= Duration::from_millis(200) {
            let dt = 0.2_f32;
            let mut forward = false;
            if let Some(id) = follow_target_id {
                if let Some(target) = players.get(&id) {
                    let dx = target.pos.x - state.pos.x;
                    let dy = target.pos.y - state.pos.y;
                    let dz = target.pos.z - state.pos.z;
                    let dist = (dx * dx + dz * dz).sqrt();
                    if dist > 0.01 {
                        let desired_yaw = dx.atan2(dz);
                        state.yaw = approach_angle(state.yaw, desired_yaw, 0.2);
                        let horiz = (dx * dx + dz * dz).sqrt();
                        if horiz > 0.01 {
                            let desired_pitch = dy.atan2(horiz);
                            state.pitch = approach_angle(state.pitch, desired_pitch, 0.2);
                        }
                    }
                    let follow_dist = distance * 10.0;
                    let follow_stop = follow_dist * 0.9;
                    if dist > follow_dist {
                        forward = true;
                    } else if dist < follow_stop {
                        forward = false;
                    }
                } else {
                    println!("target id={} not in player map", id);
                }
            } else if let Some(name) = follow_target_name.as_ref() {
                println!("waiting for active object for {}", name);
            }
            let input = InputState {
                forward,
                speed,
                yaw: state.yaw,
            };
            advance_position_bs(&mut state, input, dt);
            apply_ground_snap(&mut state, &world, collider);

            if forward {
                state.key_pressed = protocol::KEY_FORWARD;
                state.movement_speed = 1.0;
                state.movement_dir = 0.0;
            } else {
                state.key_pressed = 0;
                state.movement_speed = 0.0;
            }

            conn.send_playerpos(&state)?;
            last_send = Instant::now();

            if Instant::now() > end_time {
                conn.send_control_disco()?;
                return Ok(());
            }
        }

        if !gotblocks_pending.is_empty() {
            let batch = gotblocks_pending.len().min(10);
            let send = gotblocks_pending.drain(0..batch).collect::<Vec<_>>();
            conn.send_gotblocks(&send)?;
        }
    }
}

fn follow_command(
    address: &str,
    player: &str,
    password: &str,
    seconds: f32,
    tp_cmd: &str,
    follow_cmd: &str,
) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

    let deadline = Instant::now() + Duration::from_secs_f32(seconds.max(1.0));
    let mut ready = false;
    let mut need_client_ready = false;
    let mut sent_client_ready = false;
    let mut got_itemdef = false;
    let mut got_nodedef = false;
    let mut srp: Option<SrpClient> = None;
    let mut follow_target_name: Option<String> = None;
    let mut sent_tp = false;
    let mut sent_follow = false;

    while Instant::now() < deadline {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello { auth_mechs, .. } => {
                    if !auth_mechanism_supported(auth_mechs) {
                        bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                    }
                    match auth_mechanism_choice(auth_mechs) {
                        AuthChoice::FirstSrp => {
                            conn.send_first_srp(player, password)?;
                        }
                        AuthChoice::Srp => {
                            let client = SrpClient::new(player, password)?;
                            conn.send_srp_a(&client.a_bytes)?;
                            srp = Some(client);
                        }
                    }
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    if let Some(client) = srp.as_ref() {
                        let m = client.process_challenge(&salt, &b)?;
                        conn.send_srp_m(&m)?;
                    }
                }
                MtpEvent::AuthAccept => {
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
                MtpEvent::ChatMessage {
                    sender, message, ..
                } => {
                    if follow_target_name.is_none() {
                        if !sender.is_empty() && sender != player {
                            follow_target_name = Some(normalize_player_name(&sender));
                        } else if let Some(name) = extract_chat_player(&message) {
                            if name != player {
                                follow_target_name = Some(normalize_player_name(&name));
                            }
                        }
                        if let Some(ref name) = follow_target_name {
                            println!("follow_cmd target: {}", name);
                        }
                    }
                }
                MtpEvent::AccessDenied { reason } => {
                    bail!("access denied: {reason}");
                }
                _ => {}
            }
        }

        if need_client_ready
            && !sent_client_ready
            && should_send_client_ready(got_itemdef, got_nodedef)
        {
            conn.send_client_ready()?;
            sent_client_ready = true;
            println!("sent client_ready");
        }

        if ready && sent_client_ready {
            if let Some(ref name) = follow_target_name {
                if !sent_tp && !tp_cmd.is_empty() {
                    let cmd = tp_cmd.replace("{player}", name);
                    conn.send_chat_message(&cmd)?;
                    sent_tp = true;
                    println!("sent tp command: {}", cmd);
                }
                if !sent_follow && !follow_cmd.is_empty() {
                    let cmd = follow_cmd.replace("{player}", name);
                    conn.send_chat_message(&cmd)?;
                    sent_follow = true;
                    println!("sent follow command: {}", cmd);
                }
                if sent_tp && sent_follow {
                    conn.send_control_disco()?;
                    return Ok(());
                }
            }
        }
    }

    bail!("follow_cmd timed out without finding target");
}

fn join_bot(
    address: &str,
    player: &str,
    password: &str,
    allow: &str,
    tp_cmd: &str,
    follow_cmd: &str,
    stop_cmd: &str,
    follow_speed: f32,
    follow_distance: f32,
    float: bool,
    api_addr: &str,
    api_token: &str,
) -> Result<()> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .context("set read timeout")?;

    let mut conn = MtpConnection::new(socket, addr);
    conn.send_dummy_reliable()?;

    let mut ready = false;
    let mut need_client_ready = false;
    let mut sent_client_ready = false;
    let mut got_itemdef = false;
    let mut got_nodedef = false;
    let mut got_spawn = false;
    let mut srp: Option<SrpClient> = None;
    let mut state = PlayerState::default();
    let allow_list = parse_allowlist(allow);
    let mut last_send = Instant::now();
    let last_pos = Arc::new(Mutex::new(Vec3::default()));
    let chat_log = Arc::new(Mutex::new(ChatLog::default()));
    let (api_tx, api_rx) = mpsc::channel();
    let pending_observe = Arc::new(Mutex::new(None::<mpsc::Sender<String>>));
    let pending_sleep = Arc::new(Mutex::new(None::<mpsc::Sender<String>>));
    let pending_mine = Arc::new(Mutex::new(None::<mpsc::Sender<String>>));
    let pending_place = Arc::new(Mutex::new(None::<mpsc::Sender<String>>));
    let pending_drop = Arc::new(Mutex::new(None::<mpsc::Sender<String>>));
    let pending_wield = Arc::new(Mutex::new(None::<mpsc::Sender<String>>));
    let pending_use = Arc::new(Mutex::new(None::<mpsc::Sender<String>>));
    println!("movement mode: {}", if float { "float" } else { "physics" });
    if !api_addr.is_empty() {
        let addr = api_addr.to_string();
        let token = api_token.to_string();
        let pos_ref = Arc::clone(&last_pos);
        let pending_ref = Arc::clone(&pending_observe);
        let sleep_ref = Arc::clone(&pending_sleep);
        let mine_ref = Arc::clone(&pending_mine);
        let place_ref = Arc::clone(&pending_place);
        let drop_ref = Arc::clone(&pending_drop);
        let wield_ref = Arc::clone(&pending_wield);
        let use_ref = Arc::clone(&pending_use);
        let chat_ref = Arc::clone(&chat_log);
        thread::spawn(move || {
            run_api_server(
                &addr,
                &token,
                api_tx,
                pos_ref,
                pending_ref,
                sleep_ref,
                mine_ref,
                place_ref,
                drop_ref,
                wield_ref,
                use_ref,
                chat_ref,
            )
        });
        println!("api listening on {}", api_addr);
    }
    let mut world = World::new();
    let collider = PlayerCollider::default();
    let mut gotblocks_pending = Vec::new();
    let mut ser_ver: Option<u8> = None;
    let mut proto_ver: Option<u16> = None;
    let mut players: HashMap<u16, RemotePlayer> = HashMap::new();
    let mut follow_target_name: Option<String> = None;
    let mut follow_target_id: Option<u16> = None;
    let mut follow_enabled = false;
    let mut move_goal: Option<MoveGoal> = None;
    let mut last_sent_pos: Option<Vec3> = None;
    let mut last_server_pos: Option<Vec3> = None;
    let mut pending_attack: Option<PendingAttack> = None;
    let attack_range_bs = 50.0;
    let mut follow_distance_override: Option<f32> = None;
    let mut _local_player_id: Option<u16> = None;
    let mut pending_observe_deadline: Option<Instant> = None;
    let mut pending_sleep_deadline: Option<Instant> = None;
    let mut pending_mine_deadline: Option<Instant> = None;
    let mut pending_place_deadline: Option<Instant> = None;
    let mut pending_drop_deadline: Option<Instant> = None;
    let mut pending_wield_deadline: Option<Instant> = None;
    let mut pending_use_deadline: Option<Instant> = None;
    let mut last_follow_debug: Option<Instant> = None;
    let mut last_follow_pos: Option<Vec3> = None;

    loop {
        if let Some(event) = conn.recv_packet()? {
            match event {
                MtpEvent::SetPeerId(peer_id) => {
                    conn.peer_id = peer_id;
                    conn.send_init(player)?;
                }
                MtpEvent::ToClientHello {
                    auth_mechs,
                    ser_ver: hello_ser_ver,
                    proto_ver: hello_proto_ver,
                    ..
                } => {
                    ser_ver = Some(hello_ser_ver);
                    proto_ver = Some(hello_proto_ver);
                    if !auth_mechanism_supported(auth_mechs) {
                        bail!("unsupported auth mechanisms: 0x{:08x}", auth_mechs);
                    }
                    match auth_mechanism_choice(auth_mechs) {
                        AuthChoice::FirstSrp => {
                            conn.send_first_srp(player, password)?;
                        }
                        AuthChoice::Srp => {
                            let client = SrpClient::new(player, password)?;
                            conn.send_srp_a(&client.a_bytes)?;
                            srp = Some(client);
                        }
                    }
                }
                MtpEvent::SrpBytesSB { salt, b } => {
                    if let Some(client) = srp.as_ref() {
                        let m = client.process_challenge(&salt, &b)?;
                        conn.send_srp_m(&m)?;
                    }
                }
                MtpEvent::AuthAccept => {
                    conn.send_init2()?;
                    need_client_ready = true;
                    ready = true;
                    println!("joined; waiting for client_ready state");
                }
                MtpEvent::ItemDef => {
                    got_itemdef = true;
                }
                MtpEvent::NodeDef { data } => {
                    got_nodedef = true;
                    let version = proto_ver.unwrap_or(protocol::LATEST_PROTOCOL_VERSION);
                    match parse_nodedef_zstd(&data, version) {
                        Ok(mut manager) => {
                            manager.resolve_crossrefs();
                            world.set_nodedef(manager);
                            println!("got nodedef");
                        }
                        Err(err) => {
                            println!("nodedef parse failed: {}", err);
                        }
                    }
                }
                MtpEvent::MediaAnnounce => {
                    conn.send_have_media()?;
                }
                MtpEvent::MovePlayer { pos, pitch, yaw } => {
                    last_server_pos = Some(pos);
                    if !got_spawn {
                        state.pos = pos;
                        state.pitch = pitch;
                        state.yaw = yaw;
                        got_spawn = true;
                    } else {
                        let dx = pos.x - state.pos.x;
                        let dy = pos.y - state.pos.y;
                        let dz = pos.z - state.pos.z;
                        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
                        if dist > 100.0 {
                            state.pos = pos;
                            state.speed = Vec3::default();
                            last_sent_pos = Some(pos);
                        }
                    }
                }
                MtpEvent::BlockData { pos, data } => {
                    let ver = ser_ver.unwrap_or(protocol::SER_FMT_VER_HIGHEST_READ);
                    if world.ingest_block(pos, &data, ver).is_ok() {
                        gotblocks_pending.push(pos);
                    }
                }
                MtpEvent::ActiveObjectRemoveAdd { removed, added } => {
                    for id in removed {
                        players.remove(&id);
                        if follow_target_id == Some(id) {
                            follow_target_id = None;
                        }
                    }
                    for ActiveObjectInit { id, data, .. } in added {
                        if let Ok(info) = parse_active_object_init(&data) {
                            if info.is_player && info.name != player {
                                players.insert(
                                    id,
                                    RemotePlayer {
                                        name: info.name.clone(),
                                        pos: info.pos,
                                    },
                                );
                            }
                            if info.is_player && info.name == player {
                                _local_player_id = Some(id);
                            }
                        }
                    }
                    if follow_target_id.is_none() {
                        follow_target_id = find_target_id(&players, follow_target_name.as_deref());
                    }
                }
                MtpEvent::ActiveObjectMessages { messages } => {
                    for ActiveObjectMessage { id, data } in messages {
                        if let Ok(Some(pos)) = parse_active_object_update_position(&data) {
                            if let Some(entry) = players.get_mut(&id) {
                                entry.pos = pos;
                            }
                        }
                    }
                }
                MtpEvent::ChatMessage {
                    sender, message, ..
                } => {
                    if let Some(payload) = message.strip_prefix("BOT_OBSERVE ") {
                        if let Ok(mut slot) = pending_observe.lock() {
                            if let Some(reply) = slot.take() {
                                let _ = reply.send(payload.to_string());
                                pending_observe_deadline = None;
                                println!(
                                    "observe_server response received ({} bytes)",
                                    payload.len()
                                );
                                continue;
                            }
                        }
                    }
                    if let Some(payload) = message.strip_prefix("BOT_MINE ") {
                        if let Ok(mut slot) = pending_mine.lock() {
                            if let Some(reply) = slot.take() {
                                let _ = reply.send(payload.to_string());
                                pending_mine_deadline = None;
                                println!("mine response received ({} bytes)", payload.len());
                                continue;
                            }
                        }
                    }
                    if let Some(payload) = message.strip_prefix("BOT_DROP ") {
                        if let Ok(mut slot) = pending_drop.lock() {
                            if let Some(reply) = slot.take() {
                                let _ = reply.send(payload.to_string());
                                pending_drop_deadline = None;
                                println!("drop response received ({} bytes)", payload.len());
                                continue;
                            }
                        }
                    }
                    if let Some(payload) = message.strip_prefix("BOT_WIELD ") {
                        if let Ok(mut slot) = pending_wield.lock() {
                            if let Some(reply) = slot.take() {
                                let _ = reply.send(payload.to_string());
                                pending_wield_deadline = None;
                                println!("wield response received ({} bytes)", payload.len());
                                continue;
                            }
                        }
                    }
                    if let Some(payload) = message.strip_prefix("BOT_USE ") {
                        if let Ok(mut slot) = pending_use.lock() {
                            if let Some(reply) = slot.take() {
                                let _ = reply.send(payload.to_string());
                                pending_use_deadline = None;
                                println!("use response received ({} bytes)", payload.len());
                                continue;
                            }
                        }
                    }
                    if let Some(payload) = message.strip_prefix("BOT_PLACE ") {
                        if let Ok(mut slot) = pending_place.lock() {
                            if let Some(reply) = slot.take() {
                                let _ = reply.send(payload.to_string());
                                pending_place_deadline = None;
                                println!("place response received ({} bytes)", payload.len());
                                continue;
                            }
                        }
                    }
                    if let Some(payload) = message.strip_prefix("BOT_SLEEP ") {
                        if let Ok(mut slot) = pending_sleep.lock() {
                            if let Some(reply) = slot.take() {
                                let _ = reply.send(payload.to_string());
                                pending_sleep_deadline = None;
                                println!("sleep response received ({} bytes)", payload.len());
                                continue;
                            }
                        }
                    }
                    let (effective_sender, effective_message) =
                        effective_chat_sender_message(&sender, &message);
                    if !effective_message.is_empty() {
                        let is_system_ok = effective_sender.is_empty()
                            && (effective_message == "ok"
                                || effective_message.starts_with("You cannot send more messages"));
                        if !is_system_ok {
                            log_chat(&chat_log, &effective_sender, &effective_message);
                        }
                    }
                    if !effective_sender.is_empty() {
                        println!("chat from={} msg={}", effective_sender, effective_message);
                    }
                    if is_sender_allowed(&allow_list, &effective_sender) {
                        if let Some(cmd) = parse_control_command(&effective_message) {
                            match cmd {
                                ControlCommand::Follow(target) => {
                                    follow_target_name = Some(normalize_player_name(&target));
                                    follow_target_id =
                                        find_target_id(&players, follow_target_name.as_deref());
                                    follow_enabled = true;
                                    println!("follow enabled target={}", target);
                                    if !follow_cmd.is_empty() {
                                        let msg = follow_cmd.replace("{player}", &target);
                                        conn.send_chat_message(&msg)?;
                                    }
                                }
                                ControlCommand::Teleport(target) => {
                                    if !tp_cmd.is_empty() {
                                        let msg = tp_cmd.replace("{player}", &target);
                                        conn.send_chat_message(&msg)?;
                                    }
                                }
                                ControlCommand::Attack(target) => {
                                    let name = normalize_player_name(&target);
                                    let cmd = format!("/bot_attack {}", name);
                                    conn.send_chat_message(&cmd)?;
                                    println!("attack command sent: {}", cmd);
                                }
                                ControlCommand::AttackAll(target) => {
                                    let name = normalize_player_name(&target);
                                    let cmd = format!("/bot_attack {}", name);
                                    conn.send_chat_message(&cmd)?;
                                    println!("attack-all command sent: {}", cmd);
                                }
                                ControlCommand::AttackMobs(radius) => {
                                    let r = radius.unwrap_or(6).clamp(1, 20);
                                    let cmd = format!("/bot_attack_mobs {}", r);
                                    conn.send_chat_message(&cmd)?;
                                    println!("attack-mobs command sent: {}", cmd);
                                }
                                ControlCommand::Sleep(radius) => {
                                    let r = radius.unwrap_or(6).clamp(1, 20);
                                    let cmd = format!("/bot_sleep {}", r);
                                    conn.send_chat_message(&cmd)?;
                                    println!("sleep command sent: {}", cmd);
                                }
                                ControlCommand::Approach(target) => {
                                    let cmd = format!("/bot_approach {}", target);
                                    conn.send_chat_message(&cmd)?;
                                    println!("approach command sent: {}", cmd);
                                }
                                ControlCommand::Interact(target) => {
                                    let cmd = format!("/bot_interact {}", target);
                                    conn.send_chat_message(&cmd)?;
                                    println!("interact command sent: {}", cmd);
                                }
                                ControlCommand::Fight(target) => {
                                    let cmd = format!("/bot_fight {}", target);
                                    conn.send_chat_message(&cmd)?;
                                    println!("fight command sent: {}", cmd);
                                }
                                ControlCommand::Stop => {
                                    follow_enabled = false;
                                    follow_target_id = None;
                                    follow_target_name = None;
                                    if !stop_cmd.is_empty() {
                                        conn.send_chat_message(stop_cmd)?;
                                    }
                                }
                                ControlCommand::Where => {
                                    let msg = format!(
                                        "pos=({:.2},{:.2},{:.2})",
                                        state.pos.x, state.pos.y, state.pos.z
                                    );
                                    conn.send_chat_message(&msg)?;
                                }
                            }
                        }
                    }
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
            println!("sent client_ready");
        }

        handle_api_commands(
            &api_rx,
            &mut conn,
            &state,
            &world,
            &players,
            &mut follow_enabled,
            &mut follow_target_name,
            &mut follow_target_id,
            &mut follow_distance_override,
            &mut move_goal,
            follow_speed,
            tp_cmd,
            stop_cmd,
        )?;

        if let Ok(slot) = pending_observe.lock() {
            if slot.is_some() && pending_observe_deadline.is_none() {
                pending_observe_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
        }

        if let Ok(slot) = pending_sleep.lock() {
            if slot.is_some() && pending_sleep_deadline.is_none() {
                pending_sleep_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
        }

        if let Ok(slot) = pending_mine.lock() {
            if slot.is_some() && pending_mine_deadline.is_none() {
                pending_mine_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
        }

        if let Ok(slot) = pending_place.lock() {
            if slot.is_some() && pending_place_deadline.is_none() {
                pending_place_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
        }

        if let Ok(slot) = pending_drop.lock() {
            if slot.is_some() && pending_drop_deadline.is_none() {
                pending_drop_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
        }

        if let Ok(slot) = pending_wield.lock() {
            if slot.is_some() && pending_wield_deadline.is_none() {
                pending_wield_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
        }

        if let Ok(slot) = pending_use.lock() {
            if slot.is_some() && pending_use_deadline.is_none() {
                pending_use_deadline = Some(Instant::now() + Duration::from_secs(2));
            }
        }

        if let Some(deadline) = pending_observe_deadline {
            if Instant::now() > deadline {
                if let Ok(mut slot) = pending_observe.lock() {
                    slot.take();
                }
                pending_observe_deadline = None;
            }
        }

        if let Some(deadline) = pending_sleep_deadline {
            if Instant::now() > deadline {
                if let Ok(mut slot) = pending_sleep.lock() {
                    slot.take();
                }
                pending_sleep_deadline = None;
            }
        }

        if let Some(deadline) = pending_mine_deadline {
            if Instant::now() > deadline {
                if let Ok(mut slot) = pending_mine.lock() {
                    slot.take();
                }
                pending_mine_deadline = None;
            }
        }

        if let Some(deadline) = pending_place_deadline {
            if Instant::now() > deadline {
                if let Ok(mut slot) = pending_place.lock() {
                    slot.take();
                }
                pending_place_deadline = None;
            }
        }

        if let Some(deadline) = pending_drop_deadline {
            if Instant::now() > deadline {
                if let Ok(mut slot) = pending_drop.lock() {
                    slot.take();
                }
                pending_drop_deadline = None;
            }
        }

        if let Some(deadline) = pending_wield_deadline {
            if Instant::now() > deadline {
                if let Ok(mut slot) = pending_wield.lock() {
                    slot.take();
                }
                pending_wield_deadline = None;
            }
        }

        if let Some(deadline) = pending_use_deadline {
            if Instant::now() > deadline {
                if let Ok(mut slot) = pending_use.lock() {
                    slot.take();
                }
                pending_use_deadline = None;
            }
        }

        if let Some(pending) = pending_attack.as_mut() {
            if let Some(target) = players.get(&pending.id) {
                let dx = target.pos.x - state.pos.x;
                let dy = target.pos.y - state.pos.y;
                let dz = target.pos.z - state.pos.z;
                let dist = (dx * dx + dy * dy + dz * dz).sqrt();
                if std::env::var("LUANTI_DEBUG_INTERACT")
                    .map(|v| v == "1")
                    .unwrap_or(false)
                {
                    println!(
                        "attack dist={:.2} self=({:.2},{:.2},{:.2}) target=({:.2},{:.2},{:.2})",
                        dist,
                        state.pos.x,
                        state.pos.y,
                        state.pos.z,
                        target.pos.x,
                        target.pos.y,
                        target.pos.z
                    );
                }
                if dist <= attack_range_bs {
                    if Instant::now() >= pending.send_at {
                        if pending.next_idx < pending.actions.len() {
                            state.pos = target.pos;
                            state.speed = Vec3::default();
                            conn.send_playeritem(0)?;
                            let action = pending.actions[pending.next_idx];
                            conn.send_interact_object(action, pending.id, &state)?;
                            pending.next_idx += 1;
                            pending.send_at = Instant::now() + Duration::from_millis(180);
                        } else {
                            pending_attack = None;
                            follow_distance_override = None;
                        }
                    }
                } else {
                    state.yaw = dx.atan2(dz);
                    let attack_input = InputState {
                        forward: true,
                        speed: follow_speed,
                        yaw: state.yaw,
                    };
                    if float {
                        advance_position_bs(&mut state, attack_input, 0.2_f32);
                    } else {
                        step_player_bs(&mut state, &world, collider, attack_input, 0.2_f32);
                    }
                    last_sent_pos = Some(state.pos);
                }
            }
        }

        if ready && got_spawn && last_send.elapsed() >= Duration::from_millis(200) {
            let dt = 0.2_f32;
            let mut forward = false;
            let mut move_speed = follow_speed;
            let mut move_active = false;
            if float && !follow_enabled && move_goal.is_none() {
                if let Some(server_pos) = last_server_pos {
                    state.pos = server_pos;
                    state.speed = Vec3::default();
                }
                apply_ground_snap(&mut state, &world, collider);
            }
            if let Some(goal) = move_goal.as_mut() {
                let dx = goal.target.x - state.pos.x;
                let dz = goal.target.z - state.pos.z;
                let dist = (dx * dx + dz * dz).sqrt();
                if dist <= goal.stop_dist {
                    move_goal = None;
                } else {
                    state.yaw = dx.atan2(dz);
                    forward = true;
                    move_speed = goal.speed;
                    move_active = true;
                }
            }
            if !move_active && follow_enabled {
                if follow_target_id.is_none() {
                    follow_target_id = find_target_id(&players, follow_target_name.as_deref());
                    if follow_target_id.is_none() {
                        let now = Instant::now();
                        let should_log = last_follow_debug
                            .map(|t| now.duration_since(t) > Duration::from_secs(3))
                            .unwrap_or(true);
                        if should_log {
                            let name = follow_target_name.as_deref().unwrap_or("<none>");
                            println!(
                                "follow target not found name={} players={}",
                                name,
                                players.len()
                            );
                            last_follow_debug = Some(now);
                        }
                    }
                }
                if let Some(id) = follow_target_id {
                    if let Some(target) = players.get(&id) {
                        let dx = target.pos.x - state.pos.x;
                        let dy = target.pos.y - state.pos.y;
                        let dz = target.pos.z - state.pos.z;
                        let dist = (dx * dx + dz * dz).sqrt();
                        if dist > 0.01 {
                            let desired_yaw = dx.atan2(dz);
                            state.yaw = approach_angle(state.yaw, desired_yaw, 0.2);
                            let horiz = (dx * dx + dz * dz).sqrt();
                            if horiz > 0.01 {
                                let desired_pitch = dy.atan2(horiz);
                                state.pitch = approach_angle(state.pitch, desired_pitch, 0.2);
                            }
                        }
                        if std::env::var("LUANTI_DEBUG_FOLLOW")
                            .map(|v| v == "1")
                            .unwrap_or(false)
                        {
                            println!(
                                "follow dist={:.2} self=({:.2},{:.2},{:.2}) target=({:.2},{:.2},{:.2})",
                                dist,
                                state.pos.x,
                                state.pos.y,
                                state.pos.z,
                                target.pos.x,
                                target.pos.y,
                                target.pos.z
                            );
                        }
                        let follow_dist =
                            follow_distance_override.unwrap_or(follow_distance) * 10.0;
                        let follow_stop = follow_dist * 0.9;
                        if dist > follow_dist {
                            forward = true;
                        } else if dist < follow_stop {
                            forward = false;
                        }
                    }
                }
            }
            let input = InputState {
                forward,
                speed: move_speed,
                yaw: state.yaw,
            };
            if float {
                if move_active || follow_enabled {
                    advance_position_bs(&mut state, input, dt);
                    apply_ground_snap(&mut state, &world, collider);

                    if let Some(prev) = last_sent_pos {
                        let dx = state.pos.x - prev.x;
                        let dy = state.pos.y - prev.y;
                        let dz = state.pos.z - prev.z;
                        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
                        let max_delta = 5.0;
                        if dist > max_delta {
                            let scale = max_delta / dist;
                            state.pos.x = prev.x + dx * scale;
                            state.pos.y = prev.y + dy * scale;
                            state.pos.z = prev.z + dz * scale;
                        }
                        state.speed = Vec3 {
                            x: (state.pos.x - prev.x) / dt,
                            y: (state.pos.y - prev.y) / dt,
                            z: (state.pos.z - prev.z) / dt,
                        };
                    } else {
                        state.speed = Vec3::default();
                    }
                }
            } else {
                step_player_bs(&mut state, &world, collider, input, dt);
                if follow_enabled {
                    if let Some(prev_follow) = last_follow_pos {
                        let dx = state.pos.x - prev_follow.x;
                        let dz = state.pos.z - prev_follow.z;
                        let moved = (dx * dx + dz * dz).sqrt();
                        if forward && moved < 0.05 {
                            let nudge = follow_speed * 10.0 * dt * 2.5;
                            state.pos.x += state.yaw.sin() * nudge;
                            state.pos.z += state.yaw.cos() * nudge;
                        }
                    }
                    last_follow_pos = Some(state.pos);
                }
                if let Some(prev) = last_sent_pos {
                    let dx = state.pos.x - prev.x;
                    let dy = state.pos.y - prev.y;
                    let dz = state.pos.z - prev.z;
                    let dist = (dx * dx + dy * dy + dz * dz).sqrt();
                    let max_delta = 5.0;
                    if dist > max_delta {
                        let scale = max_delta / dist;
                        state.pos.x = prev.x + dx * scale;
                        state.pos.y = prev.y + dy * scale;
                        state.pos.z = prev.z + dz * scale;
                    }
                    state.speed = Vec3 {
                        x: (state.pos.x - prev.x) / dt,
                        y: (state.pos.y - prev.y) / dt,
                        z: (state.pos.z - prev.z) / dt,
                    };
                }
            }

            if forward {
                state.key_pressed = protocol::KEY_FORWARD;
                state.movement_speed = move_speed;
                state.movement_dir = 0.0;
            } else {
                state.key_pressed = 0;
                state.movement_speed = 0.0;
                state.movement_dir = 0.0;
            }

            conn.send_playerpos(&state)?;
            last_sent_pos = Some(state.pos);
            if let Ok(mut pos) = last_pos.lock() {
                *pos = state.pos;
            }
            last_send = Instant::now();
        }

        if !gotblocks_pending.is_empty() {
            let batch = gotblocks_pending.len().min(10);
            let send = gotblocks_pending.drain(0..batch).collect::<Vec<_>>();
            conn.send_gotblocks(&send)?;
        }
    }
}
