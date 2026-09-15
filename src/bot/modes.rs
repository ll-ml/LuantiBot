//! Standalone movement and follow modes used by diagnostic CLI commands.

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::app::session::{answer_srp_challenge, open_connection, start_authentication};
use crate::game::{step_player_bs, InputState, PhysicsParams, PlayerCollider, PlayerState};
use crate::network::protocol;
use crate::network::{
    should_send_client_ready, ActiveObjectInit, ActiveObjectMessage, MtpEvent, SrpClient,
};
use crate::world::{parse_nodedef_zstd, World};
use super::chat::{extract_chat_player, normalize_player_name};
use super::entities::{
    find_target_id, parse_active_object_init, parse_active_object_update_position, RemotePlayer,
};
use super::navigation::approach_angle;

pub(crate) fn move_forward(
    address: &str,
    player: &str,
    password: &str,
    seconds: f32,
    speed: f32,
) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(200))?;

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
    let mut physics_params = PhysicsParams::default();
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
                        Ok(manager) => {
                            world.set_nodedef(manager);
                            println!("got nodedef");
                        }
                        Err(err) => {
                            println!("nodedef parse failed: {}", err);
                        }
                    }
                }
                MtpEvent::Movement(settings) => {
                    physics_params = PhysicsParams::from_movement(settings);
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
            let dt = last_send.elapsed().as_secs_f32().clamp(0.001, 0.25);
            let forward = elapsed <= seconds;
            let input = InputState {
                forward,
                jump: false,
                auto_jump: true,
                speed,
                yaw: state.yaw,
            };
            step_player_bs(
                &mut state,
                &world,
                collider,
                physics_params,
                input,
                dt,
            );

            state.key_pressed = 0;
            if forward {
                state.key_pressed |= protocol::KEY_FORWARD;
                state.movement_speed = 1.0;
                state.movement_dir = 0.0;
            } else {
                state.movement_speed = 0.0;
            }
            if state.speed.y > 0.0 {
                state.key_pressed |= protocol::KEY_JUMP;
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

pub(crate) fn follow_player(
    address: &str,
    player: &str,
    password: &str,
    seconds: f32,
    speed: f32,
    distance: f32,
) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(200))?;

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
    let mut physics_params = PhysicsParams::default();
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
                        Ok(manager) => {
                            world.set_nodedef(manager);
                            println!("got nodedef");
                        }
                        Err(err) => {
                            println!("nodedef parse failed: {}", err);
                        }
                    }
                }
                MtpEvent::Movement(settings) => {
                    physics_params = PhysicsParams::from_movement(settings);
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
            let dt = last_send.elapsed().as_secs_f32().clamp(0.001, 0.25);
            let mut forward = false;
            let mut jump = false;
            if let Some(id) = follow_target_id {
                if let Some(target) = players.get(&id) {
                    let dx = target.pos.x - state.pos.x;
                    let dy = target.pos.y - state.pos.y;
                    let dz = target.pos.z - state.pos.z;
                    let dist = (dx * dx + dz * dz).sqrt();
                    if dist > 0.01 {
                        let desired_yaw = (-dx).atan2(dz);
                        state.yaw = approach_angle(state.yaw, desired_yaw, 0.2);
                        let horiz = (dx * dx + dz * dz).sqrt();
                        if horiz > 0.01 {
                            let desired_pitch = (-dy).atan2(horiz);
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
                    if dy > 5.0 {
                        jump = true;
                        if dist > 1.0 {
                            forward = true;
                        }
                    }
                } else {
                    println!("target id={} not in player map", id);
                }
            } else if let Some(name) = follow_target_name.as_ref() {
                println!("waiting for active object for {}", name);
            }
            let input = InputState {
                forward,
                jump,
                auto_jump: true,
                speed,
                yaw: state.yaw,
            };
            step_player_bs(
                &mut state,
                &world,
                collider,
                physics_params,
                input,
                dt,
            );

            state.key_pressed = 0;
            if forward {
                state.key_pressed |= protocol::KEY_FORWARD;
                state.movement_speed = 1.0;
                state.movement_dir = 0.0;
            } else {
                state.movement_speed = 0.0;
            }
            if jump || state.speed.y > 0.0 {
                state.key_pressed |= protocol::KEY_JUMP;
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

pub(crate) fn follow_command(
    address: &str,
    player: &str,
    password: &str,
    seconds: f32,
    tp_cmd: &str,
    follow_cmd: &str,
) -> Result<()> {
    let mut conn = open_connection(address, Duration::from_millis(200))?;

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
