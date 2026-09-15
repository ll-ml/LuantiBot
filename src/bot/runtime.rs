use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::game::{
    step_player_bs, AntiStuck, InputState, PhysicsParams, PlayerCollider,
    PlayerState,
};
use crate::network::protocol;
use crate::network::{
    should_send_client_ready, ActiveObjectInit, ActiveObjectMessage, MtpEvent,
    SrpClient,
};
use crate::types::Vec3;
use crate::world::{parse_nodedef_zstd, World};

use crate::app::session::{answer_srp_challenge, open_connection, start_authentication};
use crate::api::run_api_server;
use super::chat::{
    bot_protocol_message, effective_chat_sender_message, is_sender_allowed,
    log_chat, normalize_player_name, parse_allowlist, parse_control_command,
    resolve_unsupported_bot_command, ChatLog, ControlCommand,
};
use super::entities::{
    find_target_id, parse_active_object_init, parse_active_object_update_position, RemotePlayer,
};
use super::replies::PendingReplies;
use super::commands::handle_api_commands;
use super::movement::{advance_position_bs, apply_ground_snap};
use super::navigation::{
    approach_angle, invalid_route_response, parse_route_goal,
    route_response_status, send_hunt_arrival_action, waypoint_reached, ArrivalAction, MoveGoal,
    NavigationSnapshot, MAX_ROUTE_WAYPOINTS,
};
use super::observation::enrich_server_observation;
use super::tasks::{
    advance_native_craft, advance_native_inventory_transfer, advance_native_mining,
    cancel_native_craft, cancel_native_inventory_transfer, cancel_native_mining,
    continue_native_mining,
    face_native_target, fail_current_native_target, fail_native_craft,
    fail_native_inventory_transfer, handle_native_craft_prepare,
    handle_native_craft_receipt, handle_native_inventory_prepare,
    handle_native_inventory_receipt, handle_native_mine_prepare, handle_native_mine_verify,
    native_collect_targets, NativeCraftTask,
    NativeInventoryTransferTask, NativeMiningOrigin, NativeMiningTask,
};

pub(crate) fn join_bot(
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
    let mut conn = open_connection(address, Duration::from_millis(200))?;

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
    let pending_replies = PendingReplies::default();
    println!("movement mode: {}", if float { "float" } else { "physics" });
    if !api_addr.is_empty() {
        let addr = api_addr.to_string();
        let token = api_token.to_string();
        let pos_ref = Arc::clone(&last_pos);
        let pending_ref = pending_replies.clone();
        let chat_ref = Arc::clone(&chat_log);
        thread::spawn(move || {
            run_api_server(
                &addr,
                &token,
                api_tx,
                pos_ref,
                pending_ref,
                chat_ref,
            )
        });
        println!("api listening on {}", api_addr);
    }
    let mut world = World::new();
    let collider = PlayerCollider::default();
    let mut physics_params = PhysicsParams::default();
    let mut gotblocks_pending = Vec::new();
    let mut ser_ver: Option<u8> = None;
    let mut proto_ver: Option<u16> = None;
    let mut players: HashMap<u16, RemotePlayer> = HashMap::new();
    let mut follow_target_name: Option<String> = None;
    let mut follow_target_id: Option<u16> = None;
    let mut follow_enabled = false;
    let mut move_goal: Option<MoveGoal> = None;
    let mut native_mining: Option<NativeMiningTask> = None;
    let mut native_inventory_transfer: Option<NativeInventoryTransferTask> = None;
    let mut native_craft: Option<NativeCraftTask> = None;
    let mut anti_stuck = AntiStuck::default();
    let mut navigation = NavigationSnapshot::default();
    let mut last_sent_pos: Option<Vec3> = None;
    let mut last_server_pos: Option<Vec3> = None;
    let mut last_follow_debug: Option<Instant> = None;

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
                    println!("joined; waiting for client_ready state");
                }
                MtpEvent::ItemDef => {
                    got_itemdef = true;
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
                    conn.send_have_media()?;
                }
                MtpEvent::MovePlayer { pos, pitch, yaw } => {
                    last_server_pos = Some(pos);
                    state.pos = pos;
                    state.pitch = pitch;
                    state.yaw = yaw;
                    state.speed = Vec3::default();
                    last_sent_pos = Some(pos);
                    got_spawn = true;
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
                    if let Some((tag, payload)) = bot_protocol_message(&message) {
                        if tag == "BOT_MINE_PREPARE" {
                            handle_native_mine_prepare(
                                payload,
                                &mut native_mining,
                                &mut conn,
                                &mut state,
                                &pending_replies,
                                &mut navigation,
                            )?;
                            continue;
                        }
                        if tag == "BOT_MINE_VERIFY" {
                            handle_native_mine_verify(
                                payload,
                                &mut native_mining,
                                &mut conn,
                                &pending_replies,
                                &mut navigation,
                            )?;
                            continue;
                        }
                        if matches!(tag, "BOT_CHEST_PREPARE" | "BOT_FURNACE_PREPARE") {
                            let expected = native_inventory_transfer
                                .as_ref()
                                .is_some_and(|task| task.operation.prepare_tag() == tag);
                            if expected {
                                handle_native_inventory_prepare(
                                    payload,
                                    &mut native_inventory_transfer,
                                    &mut conn,
                                    &pending_replies,
                                )?;
                            }
                            continue;
                        }
                        if matches!(tag, "BOT_CHEST_RECEIPT" | "BOT_FURNACE_RECEIPT") {
                            let expected = native_inventory_transfer
                                .as_ref()
                                .is_some_and(|task| task.operation.receipt_tag() == tag);
                            if expected {
                                handle_native_inventory_receipt(
                                    payload,
                                    &mut native_inventory_transfer,
                                    &mut conn,
                                    &pending_replies,
                                )?;
                            }
                            continue;
                        }
                        if tag == "BOT_CRAFT_PREPARE" {
                            handle_native_craft_prepare(
                                payload,
                                &mut native_craft,
                                &mut conn,
                                &pending_replies,
                            )?;
                            continue;
                        }
                        if tag == "BOT_CRAFT_RECEIPT" {
                            handle_native_craft_receipt(
                                payload,
                                &mut native_craft,
                                &mut conn,
                                &pending_replies,
                            );
                            continue;
                        }
                        let mut response = if tag == "BOT_OBSERVE" {
                            enrich_server_observation(
                                payload,
                                follow_enabled,
                                follow_target_name.as_deref(),
                                move_goal.as_ref(),
                                &navigation,
                            )
                        } else {
                            payload.to_string()
                        };

                        let status = route_response_status(payload);
                        let carries_route = matches!(
                            tag,
                            "BOT_PATH_NODE" | "BOT_GATHER_PATH" | "BOT_HUNT_PATH"
                        ) || (tag == "BOT_HUNT"
                            && status.as_deref() == Some("repath_required"));
                        let route = carries_route.then(|| {
                            parse_route_goal(payload, state.pos, follow_speed)
                        });
                        if let Some(Err(error)) = route.as_ref() {
                            response = invalid_route_response(error);
                        }

                        let resolved = pending_replies.resolve(tag, response);
                        let accept_route = resolved || tag == "BOT_HUNT";
                        if accept_route {
                            match route {
                                Some(Ok(Some(goal))) => {
                                    move_goal = Some(goal);
                                    follow_enabled = false;
                                    follow_target_id = None;
                                    anti_stuck.reset();
                                    navigation.begin("moving");
                                }
                                Some(Ok(None)) => {
                                    let error = status
                                        .clone()
                                        .unwrap_or_else(|| "path_not_found".to_string());
                                    navigation.last_error = Some(error.clone());
                                    if move_goal.is_none() && !follow_enabled {
                                        navigation.fail(error);
                                    }
                                }
                                Some(Err(error)) => {
                                    navigation.fail(format!("invalid_path_response: {error}"));
                                }
                                None => {}
                            }
                        }

                        if tag == "BOT_HUNT" && !carries_route {
                            let ok = serde_json::from_str::<serde_json::Value>(payload)
                                .ok()
                                .and_then(|value| value.get("ok").and_then(|ok| ok.as_bool()))
                                .unwrap_or(false);
                            if ok {
                                navigation.stop();
                            } else {
                                navigation.fail(
                                    status.unwrap_or_else(|| "hunt_failed".to_string()),
                                );
                            }
                        } else if tag == "BOT_COLLECT" && !resolved && navigation.status == "moving"
                        {
                            let ok = serde_json::from_str::<serde_json::Value>(payload)
                                .ok()
                                .and_then(|value| value.get("ok").and_then(|ok| ok.as_bool()))
                                .unwrap_or(false);
                            if ok {
                                navigation.stop();
                            } else {
                                navigation.fail(
                                    status.unwrap_or_else(|| "collect_failed".to_string()),
                                );
                            }
                        }

                        if resolved {
                            let action = tag
                                .strip_prefix("BOT_")
                                .unwrap_or(tag)
                                .to_ascii_lowercase();
                            println!("{action} response received ({} bytes)", payload.len());
                        }
                        // Structured BOT_* messages are an internal protocol. Late
                        // or unsolicited replies must never enter player chat/context.
                        continue;
                    }
                    let (effective_sender, effective_message) =
                        effective_chat_sender_message(&sender, &message);
                    if let Some(command) = resolve_unsupported_bot_command(
                        &pending_replies,
                        &effective_sender,
                        &effective_message,
                    ) {
                        eprintln!(
                            "server mod does not provide /{command}; disabling that action until restart"
                        );
                        if matches!(command, "bot_prepare_mine" | "bot_verify_mine") {
                            fail_current_native_target(
                                &mut native_mining,
                                format!("unsupported_command:{command}"),
                                &mut conn,
                                &pending_replies,
                                &mut navigation,
                            )?;
                            continue;
                        }
                        if matches!(
                            command,
                            "bot_chest_prepare"
                                | "bot_chest_receipt"
                                | "bot_chest_cancel"
                                | "bot_furnace_prepare"
                                | "bot_furnace_receipt"
                                | "bot_furnace_cancel"
                        ) {
                            fail_native_inventory_transfer(
                                &mut native_inventory_transfer,
                                &mut conn,
                                format!("unsupported_command:{command}"),
                                &pending_replies,
                            );
                            continue;
                        }
                        if matches!(
                            command,
                            "bot_craft_prepare" | "bot_craft_receipt" | "bot_craft_cancel"
                        ) {
                            fail_native_craft(
                                &mut native_craft,
                                &mut conn,
                                format!("unsupported_command:{command}"),
                                &pending_replies,
                            );
                            continue;
                        }
                        let navigation_command = matches!(
                            command,
                            "bot_path_node" | "bot_gather_path" | "bot_hunt_path"
                        );
                        let arrival_command = matches!(command, "bot_collect" | "bot_hunt")
                            && navigation.status == "moving"
                            && move_goal.is_none();
                        if arrival_command
                            || (navigation_command && move_goal.is_none() && !follow_enabled)
                        {
                            navigation.fail(format!("unsupported_command:{command}"));
                        }
                        continue;
                    }
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
                                    anti_stuck.reset();
                                    navigation.begin(if move_goal.is_some() {
                                        "moving"
                                    } else {
                                        "following"
                                    });
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
                                    cancel_native_mining(
                                        &mut native_mining,
                                        &mut conn,
                                        &state,
                                        &pending_replies,
                                        &mut navigation,
                                    )?;
                                    cancel_native_inventory_transfer(
                                        &mut native_inventory_transfer,
                                        &mut conn,
                                        &pending_replies,
                                    )?;
                                    cancel_native_craft(
                                        &mut native_craft,
                                        &mut conn,
                                        &pending_replies,
                                    )?;
                                    follow_enabled = false;
                                    follow_target_id = None;
                                    follow_target_name = None;
                                    move_goal = None;
                                    anti_stuck.reset();
                                    navigation.stop();
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
            &mut move_goal,
            &mut native_mining,
            &mut native_inventory_transfer,
            &mut native_craft,
            &mut anti_stuck,
            &mut navigation,
            &pending_replies,
            follow_speed,
            tp_cmd,
            stop_cmd,
        )?;

        advance_native_mining(
            &mut native_mining,
            &mut conn,
            &state,
            &pending_replies,
            &mut navigation,
        )?;

        advance_native_inventory_transfer(
            &mut native_inventory_transfer,
            &mut conn,
            &pending_replies,
        )?;

        advance_native_craft(&mut native_craft, &mut conn, &pending_replies)?;

        if ready && got_spawn && last_send.elapsed() >= Duration::from_millis(200) {
            let dt = last_send.elapsed().as_secs_f32().clamp(0.001, 0.25);
            let mut forward = false;
            let mut jump = false;
            let mut move_speed = follow_speed;
            let mut move_active = false;
            let mut navigation_target = None;
            let mut completed_action = None;
            if float && !follow_enabled && move_goal.is_none() {
                if let Some(server_pos) = last_server_pos {
                    state.pos = server_pos;
                    state.speed = Vec3::default();
                }
                apply_ground_snap(&mut state, &world, collider);
            }
            for _ in 0..=MAX_ROUTE_WAYPOINTS {
                let Some(goal) = move_goal.as_ref() else {
                    break;
                };
                let waypoint = goal.current_waypoint();
                if waypoint_reached(state.pos, waypoint, goal.stop_dist) {
                    let finished = move_goal
                        .as_mut()
                        .is_some_and(MoveGoal::advance_waypoint);
                    anti_stuck.reset();
                    if finished {
                        completed_action = move_goal
                            .take()
                            .and_then(|mut goal| goal.arrival_action.take());
                        if completed_action.is_none() {
                            if follow_enabled {
                                navigation.begin("following");
                            } else {
                                navigation.stop();
                            }
                        }
                        break;
                    }
                    continue;
                }

                let dx = waypoint.x - state.pos.x;
                let dy = waypoint.y - state.pos.y;
                let dz = waypoint.z - state.pos.z;
                let dist = (dx * dx + dz * dz).sqrt();
                state.yaw = (-dx).atan2(dz);
                forward = dist > goal.stop_dist;
                jump = dy > 5.0;
                move_speed = goal.speed;
                move_active = true;
                navigation_target = Some(waypoint);
                break;
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
                            let desired_yaw = (-dx).atan2(dz);
                            state.yaw = approach_angle(state.yaw, desired_yaw, 0.2);
                            let horiz = (dx * dx + dz * dz).sqrt();
                            if horiz > 0.01 {
                                let desired_pitch = (-dy).atan2(horiz);
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
                        let follow_dist = follow_distance * 10.0;
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
                        navigation_target = Some(target.pos);
                    }
                }
            }
            let triggered_arrival_action = completed_action.is_some();
            if let Some(action) = completed_action {
                match action {
                    ArrivalAction::Collect {
                        node,
                        count,
                        radius,
                        target,
                    } => {
                        if native_mining.is_some() {
                            navigation.fail("mining_busy");
                        } else {
                            let targets = native_collect_targets(
                                &world,
                                &state,
                                &node,
                                count,
                                radius,
                                target,
                            );
                            let mut task = NativeMiningTask::collect(
                                NativeMiningOrigin::ArrivalCollect,
                                node,
                                count,
                                targets,
                            );
                            if task.targets.is_empty() {
                                task.record_failure("no_reachable_nodes");
                            }
                            native_mining = Some(task);
                            navigation.begin("mining");
                            continue_native_mining(
                                &mut native_mining,
                                &mut conn,
                                &pending_replies,
                                &mut navigation,
                            )?;
                        }
                    }
                    ArrivalAction::Hunt { hunt_id } => {
                        send_hunt_arrival_action(&mut conn, hunt_id)?;
                        navigation.begin("moving");
                    }
                }
            }

            let mining_active = native_mining.is_some();
            if mining_active {
                forward = false;
                jump = false;
                if let Some(target) = native_mining
                    .as_ref()
                    .and_then(NativeMiningTask::active_target)
                {
                    face_native_target(&mut state, target);
                }
            }
            let navigation_moving = !mining_active
                && (move_active || (follow_enabled && (forward || jump)));
            // A route waypoint is fixed, so progress must reduce its distance.
            // A followed player is a moving target: matching their speed at a
            // constant separation is healthy progress and is judged by the
            // bot's own displacement instead.
            let anti_stuck_target = move_active.then_some(navigation_target).flatten();
            let recovery = anti_stuck.adjust(
                state.pos,
                anti_stuck_target,
                navigation_moving,
                state.yaw,
                jump,
                dt,
            );
            state.yaw = recovery.yaw;
            jump = recovery.jump;
            if recovery.recovering {
                // A pure vertical goal can otherwise keep jumping in place. Give
                // every recovery heading actual horizontal movement authority.
                forward = true;
            }
            navigation.recovering = recovery.recovering;
            navigation.recovery_attempts = if recovery.give_up {
                recovery.attempts
            } else {
                anti_stuck.recovery_attempts()
            };
            navigation.stalled_for_seconds = anti_stuck.stalled_for();
            if recovery.recovering {
                navigation.status = "recovering";
            } else if navigation.status != "failed" {
                navigation.status = if mining_active {
                    "mining"
                } else if move_active {
                    "moving"
                } else if follow_enabled {
                    "following"
                } else if triggered_arrival_action {
                    "moving"
                } else {
                    "idle"
                };
            }
            if recovery.give_up {
                forward = false;
                jump = false;
                if move_active {
                    move_goal = None;
                    move_active = false;
                }
                if follow_enabled {
                    follow_enabled = false;
                    follow_target_id = None;
                }
                navigation.fail("stuck_after_recovery");
                navigation.recovery_attempts = recovery.attempts;
            }
            let input = InputState {
                forward,
                jump,
                auto_jump: !recovery.recovering,
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
                step_player_bs(
                    &mut state,
                    &world,
                    collider,
                    physics_params,
                    input,
                    dt,
                );
            }

            state.key_pressed = 0;
            if forward {
                state.key_pressed |= protocol::KEY_FORWARD;
                state.movement_speed = 1.0;
                state.movement_dir = 0.0;
            } else {
                state.movement_speed = 0.0;
                state.movement_dir = 0.0;
            }
            if jump || (!float && state.speed.y > 0.0) {
                state.key_pressed |= protocol::KEY_JUMP;
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
