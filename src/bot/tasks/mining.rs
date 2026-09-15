//! Native node digging and multi-node collection.

use anyhow::Result;
use serde_json::json;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::game::{PlayerState, BS};
use crate::network::{protocol, MtpConnection};
use crate::types::{IVec3, Vec3};
use crate::world::World;
use crate::bot::navigation::NavigationSnapshot;
use crate::bot::replies::PendingReplies;
use crate::bot::validation::{parse_node_position, valid_command_atom};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::bot) enum NativeMiningOrigin {
    Mine,
    Collect,
    ArrivalCollect,
}

impl NativeMiningOrigin {
    pub(in crate::bot) fn reply_tag(self) -> Option<&'static str> {
        match self {
            Self::Mine => Some("BOT_MINE"),
            Self::Collect => Some("BOT_COLLECT"),
            Self::ArrivalCollect => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::bot) struct PreparedDig {
    target: IVec3,
    above: IVec3,
    wield_index: u16,
    node: String,
    tool: String,
    harvestable: bool,
}

#[derive(Clone, Debug)]
pub(in crate::bot) enum NativeMiningPhase {
    Ready,
    Preparing {
        requested_target: Option<IVec3>,
        deadline: Instant,
    },
    Digging {
        prepared: PreparedDig,
        finish_at: Instant,
    },
    Verifying {
        prepared: PreparedDig,
        deadline: Instant,
    },
}

#[derive(Clone, Debug)]
pub(in crate::bot) struct NativeMiningTask {
    pub(in crate::bot) origin: NativeMiningOrigin,
    pub(in crate::bot) node: Option<String>,
    pub(in crate::bot) requested: u16,
    pub(in crate::bot) targets: VecDeque<Option<IVec3>>,
    pub(in crate::bot) phase: NativeMiningPhase,
    pub(in crate::bot) mined: u16,
    pub(in crate::bot) failed: u16,
    pub(in crate::bot) all_harvestable: bool,
    pub(in crate::bot) last_error: Option<String>,
}

impl NativeMiningTask {
    pub(in crate::bot) fn single(target: Option<IVec3>) -> Self {
        Self {
            origin: NativeMiningOrigin::Mine,
            node: None,
            requested: 1,
            targets: VecDeque::from([target]),
            phase: NativeMiningPhase::Ready,
            mined: 0,
            failed: 0,
            all_harvestable: true,
            last_error: None,
        }
    }

    pub(in crate::bot) fn collect(
        origin: NativeMiningOrigin,
        node: String,
        requested: u16,
        targets: VecDeque<Option<IVec3>>,
    ) -> Self {
        Self {
            origin,
            node: Some(node),
            requested,
            targets,
            phase: NativeMiningPhase::Ready,
            mined: 0,
            failed: 0,
            all_harvestable: true,
            last_error: None,
        }
    }

    pub(in crate::bot) fn record_failure(&mut self, error: impl Into<String>) {
        let error = error.into();
        eprintln!("native mining target failed: {error}");
        self.failed = self.failed.saturating_add(1);
        self.last_error = Some(error);
        self.phase = NativeMiningPhase::Ready;
    }

    pub(in crate::bot) fn active_target(&self) -> Option<IVec3> {
        match &self.phase {
            NativeMiningPhase::Preparing {
                requested_target, ..
            } => *requested_target,
            NativeMiningPhase::Digging { prepared, .. }
            | NativeMiningPhase::Verifying { prepared, .. } => Some(prepared.target),
            NativeMiningPhase::Ready => None,
        }
    }
}

pub(in crate::bot) fn native_collect_targets(
    world: &World,
    state: &PlayerState,
    node_name: &str,
    count: u16,
    radius: i32,
    preferred: Option<IVec3>,
) -> VecDeque<Option<IVec3>> {
    let radius = radius.clamp(1, 6);
    let candidate_limit = count.clamp(1, 8).saturating_add(3).min(8) as usize;
    let center = Vec3 {
        x: state.pos.x / BS,
        y: state.pos.y / BS,
        z: state.pos.z / BS,
    };
    let rounded = IVec3 {
        x: center.x.round() as i32,
        y: center.y.round() as i32,
        z: center.z.round() as i32,
    };
    let mut targets = Vec::new();
    if let Some(target) = preferred {
        targets.push(target);
    }
    for y in (rounded.y - radius)..=(rounded.y + radius) {
        for x in (rounded.x - radius)..=(rounded.x + radius) {
            for z in (rounded.z - radius)..=(rounded.z + radius) {
                let target = IVec3 { x, y, z };
                if targets.contains(&target) {
                    continue;
                }
                let dx = target.x as f32 - center.x;
                let dy = target.y as f32 - center.y;
                let dz = target.z as f32 - center.z;
                if dx * dx + dy * dy + dz * dz > (radius as f32).powi(2) {
                    continue;
                }
                let Some(node) = world.get_node(target) else {
                    continue;
                };
                if world.node_name(node) == node_name {
                    targets.push(target);
                }
            }
        }
    }
    targets.sort_by(|a, b| {
        let distance = |target: &IVec3| {
            let dx = target.x as f32 - center.x;
            let dy = target.y as f32 - center.y;
            let dz = target.z as f32 - center.z;
            dx * dx + dy * dy + dz * dz
        };
        distance(a).total_cmp(&distance(b))
    });
    if let Some(preferred) = preferred {
        if let Some(index) = targets.iter().position(|target| *target == preferred) {
            targets.swap(0, index);
        }
    }
    targets
        .into_iter()
        // Keep a few substitutes queued in case a candidate changes, is
        // protected, or becomes unreachable between discovery and digging.
        .take(candidate_limit)
        .map(Some)
        .collect()
}

pub(in crate::bot) fn continue_native_mining(
    native_mining: &mut Option<NativeMiningTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
    navigation: &mut NavigationSnapshot,
) -> Result<()> {
    loop {
        let Some(task) = native_mining.as_mut() else {
            return Ok(());
        };
        if !matches!(task.phase, NativeMiningPhase::Ready) {
            return Ok(());
        }
        if task.mined >= task.requested {
            finish_native_mining(native_mining, pending_replies, navigation);
            return Ok(());
        }
        let Some(target) = task.targets.pop_front() else {
            finish_native_mining(native_mining, pending_replies, navigation);
            return Ok(());
        };
        let command = if let Some(target) = target {
            format!(
                "/bot_prepare_mine {} {} {}",
                target.x, target.y, target.z
            )
        } else {
            "/bot_prepare_mine".to_string()
        };
        conn.send_chat_message(&command)?;
        task.phase = NativeMiningPhase::Preparing {
            requested_target: target,
            deadline: Instant::now() + Duration::from_secs(3),
        };
        return Ok(());
    }
}

pub(in crate::bot) fn finish_native_mining(
    native_mining: &mut Option<NativeMiningTask>,
    pending_replies: &PendingReplies,
    navigation: &mut NavigationSnapshot,
) {
    let Some(task) = native_mining.take() else {
        return;
    };
    let ok = task.mined > 0;
    let status = if task.origin == NativeMiningOrigin::Mine && ok {
        "mined"
    } else if task.mined >= task.requested {
        "mined"
    } else if ok {
        "partially_mined"
    } else {
        task.last_error.as_deref().unwrap_or("mine_failed")
    };
    let response = json!({
        "ok": ok,
        "status": status,
        "node": task.node,
        "mined": task.mined,
        "requested": task.requested,
        "failed": task.failed,
        "harvestable": task.all_harvestable,
        "drop_pending": ok,
        "drop_status": ok.then_some("server_drop_pending_pickup"),
        "error": (!ok).then_some(status),
    })
    .to_string();
    if let Some(tag) = task.origin.reply_tag() {
        let _ = pending_replies.resolve(tag, response);
    }
    if task.origin == NativeMiningOrigin::ArrivalCollect {
        if ok {
            navigation.stop();
        } else {
            navigation.fail(status);
        }
    }
    println!(
        "native mining finished: status={} mined={}/{} failed={}",
        status, task.mined, task.requested, task.failed
    );
}

pub(in crate::bot) fn fail_current_native_target(
    native_mining: &mut Option<NativeMiningTask>,
    error: impl Into<String>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
    navigation: &mut NavigationSnapshot,
) -> Result<()> {
    if let Some(task) = native_mining.as_mut() {
        task.record_failure(error);
    }
    continue_native_mining(native_mining, conn, pending_replies, navigation)
}

pub(in crate::bot) fn face_native_target(state: &mut PlayerState, target: IVec3) {
    let target_x = target.x as f32 * BS;
    let target_y = (target.y as f32 + 0.5) * BS;
    let target_z = target.z as f32 * BS;
    let dx = target_x - state.pos.x;
    let dy = target_y - (state.pos.y + 1.5 * BS);
    let dz = target_z - state.pos.z;
    state.yaw = (-dx).atan2(dz);
    let horizontal = (dx * dx + dz * dz).sqrt();
    state.pitch = (-dy).atan2(horizontal.max(0.001));
}

pub(in crate::bot) fn handle_native_mine_prepare(
    payload: &str,
    native_mining: &mut Option<NativeMiningTask>,
    conn: &mut MtpConnection,
    state: &mut PlayerState,
    pending_replies: &PendingReplies,
    navigation: &mut NavigationSnapshot,
) -> Result<()> {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(error) => {
            return fail_current_native_target(
                native_mining,
                format!("invalid_prepare_response:{error}"),
                conn,
                pending_replies,
                navigation,
            );
        }
    };
    let Some(task) = native_mining.as_ref() else {
        return Ok(());
    };
    let requested_target = match &task.phase {
        NativeMiningPhase::Preparing {
            requested_target, ..
        } => *requested_target,
        _ => return Ok(()),
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("prepare_failed");
        return fail_current_native_target(
            native_mining,
            status,
            conn,
            pending_replies,
            navigation,
        );
    }
    let Some(target) = value.get("target").and_then(parse_node_position) else {
        return fail_current_native_target(
            native_mining,
            "prepare_missing_target",
            conn,
            pending_replies,
            navigation,
        );
    };
    if requested_target.is_some_and(|requested| requested != target) {
        return fail_current_native_target(
            native_mining,
            "prepare_target_mismatch",
            conn,
            pending_replies,
            navigation,
        );
    }
    let Some(above) = value.get("above").and_then(parse_node_position) else {
        return fail_current_native_target(
            native_mining,
            "prepare_missing_above",
            conn,
            pending_replies,
            navigation,
        );
    };
    let face_offset = (i64::from(above.x) - i64::from(target.x)).abs()
        + (i64::from(above.y) - i64::from(target.y)).abs()
        + (i64::from(above.z) - i64::from(target.z)).abs();
    if face_offset != 1 {
        return fail_current_native_target(
            native_mining,
            "prepare_invalid_above",
            conn,
            pending_replies,
            navigation,
        );
    }
    let wield_index = value
        .get("wield_index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|index| u16::try_from(index).ok());
    let Some(wield_index) = wield_index else {
        return fail_current_native_target(
            native_mining,
            "prepare_invalid_wield_index",
            conn,
            pending_replies,
            navigation,
        );
    };
    let dig_time = value
        .get("dig_time")
        .and_then(serde_json::Value::as_f64)
        .map(|time| time as f32)
        .filter(|time| time.is_finite() && *time >= 0.0 && *time <= 60.0);
    let Some(dig_time) = dig_time else {
        return fail_current_native_target(
            native_mining,
            "prepare_invalid_dig_time",
            conn,
            pending_replies,
            navigation,
        );
    };
    let node = value
        .get("node")
        .and_then(serde_json::Value::as_str)
        .filter(|node| valid_command_atom(node))
        .map(str::to_string);
    let Some(node) = node else {
        return fail_current_native_target(
            native_mining,
            "prepare_missing_node",
            conn,
            pending_replies,
            navigation,
        );
    };
    if task.node.as_deref().is_some_and(|expected| expected != node) {
        return fail_current_native_target(
            native_mining,
            "prepare_node_mismatch",
            conn,
            pending_replies,
            navigation,
        );
    }
    let prepared = PreparedDig {
        target,
        above,
        wield_index,
        node,
        tool: value
            .get("tool")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        harvestable: value
            .get("harvestable")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    };
    face_native_target(state, target);
    conn.select_wield_index(wield_index)?;
    conn.send_node_interact(
        protocol::INTERACT_START_DIGGING,
        wield_index,
        target,
        above,
        state,
    )?;
    let margin = (dig_time * 0.05).max(0.08);
    let finish_at = Instant::now() + Duration::from_secs_f32(dig_time + margin);
    println!(
        "native mining started: node={} target=({}, {}, {}) tool={} slot={} time={:.2}s harvestable={}",
        prepared.node,
        target.x,
        target.y,
        target.z,
        if prepared.tool.is_empty() { "hand" } else { &prepared.tool },
        wield_index,
        dig_time,
        prepared.harvestable
    );
    if let Some(task) = native_mining.as_mut() {
        if task.node.is_none() {
            task.node = Some(prepared.node.clone());
        }
        task.phase = NativeMiningPhase::Digging {
            prepared,
            finish_at,
        };
    }
    Ok(())
}

pub(in crate::bot) fn handle_native_mine_verify(
    payload: &str,
    native_mining: &mut Option<NativeMiningTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
    navigation: &mut NavigationSnapshot,
) -> Result<()> {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(error) => {
            return fail_current_native_target(
                native_mining,
                format!("invalid_verify_response:{error}"),
                conn,
                pending_replies,
                navigation,
            );
        }
    };
    let Some(task) = native_mining.as_mut() else {
        return Ok(());
    };
    let prepared = match &task.phase {
        NativeMiningPhase::Verifying { prepared, .. } => prepared.clone(),
        _ => return Ok(()),
    };
    if value
        .get("target")
        .and_then(parse_node_position)
        .is_some_and(|target| target != prepared.target)
    {
        task.record_failure("verify_target_mismatch");
    } else if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        task.mined = task.mined.saturating_add(1);
        task.all_harvestable &= prepared.harvestable;
        task.phase = NativeMiningPhase::Ready;
    } else {
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("node_unchanged");
        task.record_failure(status);
    }
    continue_native_mining(native_mining, conn, pending_replies, navigation)
}

pub(in crate::bot) fn advance_native_mining(
    native_mining: &mut Option<NativeMiningTask>,
    conn: &mut MtpConnection,
    state: &PlayerState,
    pending_replies: &PendingReplies,
    navigation: &mut NavigationSnapshot,
) -> Result<()> {
    let now = Instant::now();
    let phase = native_mining.as_ref().map(|task| task.phase.clone());
    match phase {
        Some(NativeMiningPhase::Preparing { deadline, .. }) if now >= deadline => {
            fail_current_native_target(
                native_mining,
                "prepare_timeout",
                conn,
                pending_replies,
                navigation,
            )?;
        }
        Some(NativeMiningPhase::Digging {
            prepared,
            finish_at,
        }) if now >= finish_at => {
            let dx = prepared.target.x as f32 - state.pos.x / BS;
            let dy = prepared.target.y as f32 - state.pos.y / BS;
            let dz = prepared.target.z as f32 - state.pos.z / BS;
            if (dx * dx + dy * dy + dz * dz).sqrt() > 6.1 {
                conn.send_node_interact(
                    protocol::INTERACT_STOP_DIGGING,
                    prepared.wield_index,
                    prepared.target,
                    prepared.above,
                    state,
                )?;
                fail_current_native_target(
                    native_mining,
                    "moved_out_of_range",
                    conn,
                    pending_replies,
                    navigation,
                )?;
            } else {
                conn.send_node_interact(
                    protocol::INTERACT_DIGGING_COMPLETED,
                    prepared.wield_index,
                    prepared.target,
                    prepared.above,
                    state,
                )?;
                conn.send_chat_message(&format!(
                    "/bot_verify_mine {} {} {} {}",
                    prepared.target.x,
                    prepared.target.y,
                    prepared.target.z,
                    prepared.node
                ))?;
                if let Some(task) = native_mining.as_mut() {
                    task.phase = NativeMiningPhase::Verifying {
                        prepared,
                        deadline: now + Duration::from_secs(3),
                    };
                }
            }
        }
        Some(NativeMiningPhase::Verifying { deadline, .. }) if now >= deadline => {
            fail_current_native_target(
                native_mining,
                "verify_timeout",
                conn,
                pending_replies,
                navigation,
            )?;
        }
        _ => {}
    }
    Ok(())
}

pub(in crate::bot) fn cancel_native_mining(
    native_mining: &mut Option<NativeMiningTask>,
    conn: &mut MtpConnection,
    state: &PlayerState,
    pending_replies: &PendingReplies,
    navigation: &mut NavigationSnapshot,
) -> Result<()> {
    if let Some(NativeMiningPhase::Digging { prepared, .. }) =
        native_mining.as_ref().map(|task| task.phase.clone())
    {
        conn.send_node_interact(
            protocol::INTERACT_STOP_DIGGING,
            prepared.wield_index,
            prepared.target,
            prepared.above,
            state,
        )?;
    }
    if let Some(task) = native_mining.as_mut() {
        task.targets.clear();
        task.record_failure("cancelled");
    }
    continue_native_mining(native_mining, conn, pending_replies, navigation)
}
