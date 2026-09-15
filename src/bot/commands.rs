//! REST-facing commands and their dispatch into the live game session.

use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::sync::mpsc;

use crate::game::{AntiStuck, PlayerState};
use crate::network::MtpConnection;
use crate::types::IVec3;
use crate::world::World;
use super::chat::normalize_player_name;
use super::entities::{find_target_id, RemotePlayer};
use super::navigation::{build_move_goal, MoveGoal, MoveRequest, NavigationSnapshot};
use super::observation::build_observe_json;
use super::replies::PendingReplies;
use super::tasks::{
    cancel_native_craft, cancel_native_inventory_transfer, cancel_native_mining,
    continue_native_craft, continue_native_inventory_transfer, continue_native_mining,
    fail_native_craft, native_collect_targets, InventoryTransferOperation, NativeCraftTask,
    NativeInventoryTransferTask, NativeMiningOrigin, NativeMiningTask,
};

pub(crate) enum ApiCommand {
    Follow(String),
    Teleport(String),
    Attack(String),
    AttackMobs(Option<i32>),
    Say(String),
    Approach(String),
    Interact(String),
    Fight(String),
    Sleep(Option<i32>),
    Mine(Option<IVec3>),
    Collect {
        node: String,
        count: u16,
        radius: i32,
    },
    NavigateNode {
        node: String,
        radius: i32,
    },
    GatherResource {
        node: String,
        count: u16,
        radius: i32,
    },
    HuntFood {
        target: String,
        radius: i32,
    },
    Place(Option<IVec3>),
    Drop {
        item: Option<String>,
        count: Option<u16>,
    },
    Wield(String),
    Use(Option<String>),
    ChestInspect(IVec3),
    ChestDeposit {
        pos: IVec3,
        item: String,
        count: u16,
    },
    ChestWithdraw {
        pos: IVec3,
        item: String,
        count: u16,
    },
    FurnaceInspect(IVec3),
    FurnaceTransfer {
        operation: InventoryTransferOperation,
        pos: IVec3,
        item: String,
        count: u16,
    },
    Craft {
        item: String,
        count: u16,
    },
    Stop,
    Where,
    Move(MoveRequest),
    Observe {
        radius: i32,
        reply: mpsc::Sender<String>,
    },
    ObserveServer {
        radius: i32,
    },
}

pub(super) fn handle_api_commands(
    api_rx: &mpsc::Receiver<ApiCommand>,
    conn: &mut MtpConnection,
    state: &PlayerState,
    world: &World,
    players: &HashMap<u16, RemotePlayer>,
    follow_enabled: &mut bool,
    follow_target_name: &mut Option<String>,
    follow_target_id: &mut Option<u16>,
    move_goal: &mut Option<MoveGoal>,
    native_mining: &mut Option<NativeMiningTask>,
    native_inventory_transfer: &mut Option<NativeInventoryTransferTask>,
    native_craft: &mut Option<NativeCraftTask>,
    anti_stuck: &mut AntiStuck,
    navigation: &mut NavigationSnapshot,
    pending_replies: &PendingReplies,
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
                anti_stuck.reset();
                navigation.begin(if move_goal.is_some() {
                    "moving"
                } else {
                    "following"
                });
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
            ApiCommand::AttackMobs(radius) => {
                let radius = radius.unwrap_or(12).clamp(1, 20);
                let cmd = format!("/bot_attack_mobs {}", radius);
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
                if native_mining.is_some()
                    || native_inventory_transfer.is_some()
                    || native_craft.is_some()
                {
                    let _ = pending_replies.resolve(
                        "BOT_MINE",
                        json!({"ok": false, "status": "mining_busy"}).to_string(),
                    );
                } else {
                    *native_mining = Some(NativeMiningTask::single(pos));
                    continue_native_mining(
                        native_mining,
                        conn,
                        pending_replies,
                        navigation,
                    )?;
                }
            }
            ApiCommand::Collect {
                node,
                count,
                radius,
            } => {
                if native_mining.is_some()
                    || native_inventory_transfer.is_some()
                    || native_craft.is_some()
                {
                    let _ = pending_replies.resolve(
                        "BOT_COLLECT",
                        json!({"ok": false, "status": "mining_busy"}).to_string(),
                    );
                } else {
                    let count = count.clamp(1, 8);
                    let targets = native_collect_targets(
                        world,
                        state,
                        &node,
                        count,
                        radius,
                        None,
                    );
                    let mut task = NativeMiningTask::collect(
                        NativeMiningOrigin::Collect,
                        node,
                        count,
                        targets,
                    );
                    if task.targets.is_empty() {
                        task.record_failure("no_reachable_nodes");
                    }
                    *native_mining = Some(task);
                    continue_native_mining(
                        native_mining,
                        conn,
                        pending_replies,
                        navigation,
                    )?;
                }
            }
            ApiCommand::NavigateNode { node, radius } => {
                let cmd = format!("/bot_path_node {} {}", node, radius.clamp(2, 32));
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::GatherResource {
                node,
                count,
                radius,
            } => {
                let cmd = format!(
                    "/bot_gather_path {} {} {}",
                    node,
                    count.clamp(1, 8),
                    radius.clamp(2, 32)
                );
                conn.send_chat_message(&cmd)?;
            }
            ApiCommand::HuntFood { target, radius } => {
                let cmd = format!("/bot_hunt_path {} {}", target, radius.clamp(2, 32));
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
            ApiCommand::ChestInspect(pos) => {
                conn.send_chat_message(&format!(
                    "/bot_chest_inspect {} {} {}",
                    pos.x, pos.y, pos.z
                ))?;
            }
            ApiCommand::ChestDeposit { pos, item, count } => {
                if native_inventory_transfer.is_some()
                    || native_craft.is_some()
                    || native_mining.is_some()
                {
                    let _ = pending_replies.resolve(
                        InventoryTransferOperation::ChestDeposit.reply_tag(),
                        json!({"ok": false, "status": "inventory_transfer_busy"}).to_string(),
                    );
                } else {
                    *native_inventory_transfer = Some(NativeInventoryTransferTask::new(
                        InventoryTransferOperation::ChestDeposit,
                        pos,
                        item,
                        count.clamp(1, 99),
                    ));
                    continue_native_inventory_transfer(
                        native_inventory_transfer,
                        conn,
                        pending_replies,
                    )?;
                }
            }
            ApiCommand::ChestWithdraw { pos, item, count } => {
                if native_inventory_transfer.is_some()
                    || native_craft.is_some()
                    || native_mining.is_some()
                {
                    let _ = pending_replies.resolve(
                        InventoryTransferOperation::ChestWithdraw.reply_tag(),
                        json!({"ok": false, "status": "inventory_transfer_busy"}).to_string(),
                    );
                } else {
                    *native_inventory_transfer = Some(NativeInventoryTransferTask::new(
                        InventoryTransferOperation::ChestWithdraw,
                        pos,
                        item,
                        count.clamp(1, 99),
                    ));
                    continue_native_inventory_transfer(
                        native_inventory_transfer,
                        conn,
                        pending_replies,
                    )?;
                }
            }
            ApiCommand::FurnaceInspect(pos) => {
                conn.send_chat_message(&format!(
                    "/bot_furnace_inspect {} {} {}",
                    pos.x, pos.y, pos.z
                ))?;
            }
            ApiCommand::FurnaceTransfer {
                operation,
                pos,
                item,
                count,
            } => {
                let reply_tag = operation.reply_tag();
                if native_inventory_transfer.is_some()
                    || native_craft.is_some()
                    || native_mining.is_some()
                {
                    let _ = pending_replies.resolve(
                        reply_tag,
                        json!({"ok": false, "status": "inventory_transfer_busy"}).to_string(),
                    );
                } else {
                    *native_inventory_transfer = Some(NativeInventoryTransferTask::new(
                        operation,
                        pos,
                        item,
                        count.clamp(1, 99),
                    ));
                    continue_native_inventory_transfer(
                        native_inventory_transfer,
                        conn,
                        pending_replies,
                    )?;
                }
            }
            ApiCommand::Craft { item, count } => {
                if native_inventory_transfer.is_some()
                    || native_craft.is_some()
                    || native_mining.is_some()
                {
                    let _ = pending_replies.resolve(
                        "BOT_CRAFT",
                        json!({"ok": false, "status": "inventory_action_busy"}).to_string(),
                    );
                } else {
                    *native_craft = Some(NativeCraftTask::new(item, count.clamp(1, 64)));
                    if let Err(error) = continue_native_craft(native_craft, conn) {
                        fail_native_craft(
                            native_craft,
                            conn,
                            format!("prepare_send_failed:{error}"),
                            pending_replies,
                        );
                    }
                }
            }
            ApiCommand::Stop => {
                cancel_native_mining(
                    native_mining,
                    conn,
                    state,
                    pending_replies,
                    navigation,
                )?;
                cancel_native_inventory_transfer(
                    native_inventory_transfer,
                    conn,
                    pending_replies,
                )?;
                cancel_native_craft(native_craft, conn, pending_replies)?;
                *follow_enabled = false;
                *follow_target_id = None;
                *follow_target_name = None;
                *move_goal = None;
                anti_stuck.reset();
                navigation.stop();
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
                    move_goal.as_ref(),
                    navigation,
                );
                let _ = reply.send(json);
            }
            ApiCommand::ObserveServer { radius } => {
                let cmd = format!("/bot_observe {}", radius);
                let _ = conn.send_chat_message(&cmd);
            }
            ApiCommand::Move(request) => {
                let goal = build_move_goal(state, request, default_move_speed);
                *move_goal = Some(goal);
                *follow_enabled = false;
                *follow_target_id = None;
                anti_stuck.reset();
                navigation.begin("moving");
            }
        }
    }
    Ok(())
}
