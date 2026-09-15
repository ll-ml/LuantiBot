//! Native chest and furnace inventory transfers.

use anyhow::Result;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::network::{InventoryLocation, MtpConnection};
use crate::types::IVec3;
use crate::bot::replies::PendingReplies;
use crate::bot::validation::{chest_reply_has_nonce, parse_node_position};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InventoryTransferOperation {
    ChestDeposit,
    ChestWithdraw,
    FurnaceInput,
    FurnaceFuel,
    FurnaceOutput,
}

impl InventoryTransferOperation {
    pub(in crate::bot) fn command_name(self) -> &'static str {
        match self {
            Self::ChestDeposit => "deposit",
            Self::ChestWithdraw => "withdraw",
            Self::FurnaceInput => "input",
            Self::FurnaceFuel => "fuel",
            Self::FurnaceOutput => "output",
        }
    }

    pub(in crate::bot) fn container_kind(self) -> &'static str {
        match self {
            Self::ChestDeposit | Self::ChestWithdraw => "chest",
            Self::FurnaceInput | Self::FurnaceFuel | Self::FurnaceOutput => "furnace",
        }
    }

    pub(in crate::bot) fn prepare_command(self) -> &'static str {
        match self.container_kind() {
            "chest" => "bot_chest_prepare",
            _ => "bot_furnace_prepare",
        }
    }

    pub(in crate::bot) fn receipt_command(self) -> &'static str {
        match self.container_kind() {
            "chest" => "bot_chest_receipt",
            _ => "bot_furnace_receipt",
        }
    }

    pub(in crate::bot) fn cancel_command(self) -> &'static str {
        match self.container_kind() {
            "chest" => "bot_chest_cancel",
            _ => "bot_furnace_cancel",
        }
    }

    pub(in crate::bot) fn prepare_tag(self) -> &'static str {
        match self.container_kind() {
            "chest" => "BOT_CHEST_PREPARE",
            _ => "BOT_FURNACE_PREPARE",
        }
    }

    pub(in crate::bot) fn receipt_tag(self) -> &'static str {
        match self.container_kind() {
            "chest" => "BOT_CHEST_RECEIPT",
            _ => "BOT_FURNACE_RECEIPT",
        }
    }

    pub(crate) fn reply_tag(self) -> &'static str {
        match self {
            Self::ChestDeposit => "BOT_CHEST_DEPOSIT",
            Self::ChestWithdraw => "BOT_CHEST_WITHDRAW",
            Self::FurnaceInput => "BOT_FURNACE_INPUT",
            Self::FurnaceFuel => "BOT_FURNACE_FUEL",
            Self::FurnaceOutput => "BOT_FURNACE_OUTPUT",
        }
    }

    pub(in crate::bot) fn expected_container_list(self) -> &'static str {
        match self {
            Self::ChestDeposit | Self::ChestWithdraw => "main",
            Self::FurnaceInput => "src",
            Self::FurnaceFuel => "fuel",
            Self::FurnaceOutput => "dst",
        }
    }

    pub(in crate::bot) fn deposits_into_container(self) -> bool {
        matches!(
            self,
            Self::ChestDeposit | Self::FurnaceInput | Self::FurnaceFuel
        )
    }

    pub(in crate::bot) fn completed_status(self) -> &'static str {
        match self {
            Self::ChestDeposit => "deposited",
            Self::ChestWithdraw => "withdrawn",
            Self::FurnaceInput => "input_loaded",
            Self::FurnaceFuel => "fuel_loaded",
            Self::FurnaceOutput => "output_collected",
        }
    }

    pub(in crate::bot) fn partial_status(self) -> &'static str {
        match self {
            Self::ChestDeposit => "partially_deposited",
            Self::ChestWithdraw => "partially_withdrawn",
            Self::FurnaceInput => "input_partially_loaded",
            Self::FurnaceFuel => "fuel_partially_loaded",
            Self::FurnaceOutput => "output_partially_collected",
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::bot) enum NativeInventoryTransferPhase {
    Ready,
    Preparing {
        nonce: String,
        deadline: Instant,
    },
    WaitingForReceipt {
        nonce: String,
        action_count: u16,
        poll_at: Instant,
        expires_at: Instant,
    },
    ReceiptRequested {
        nonce: String,
        action_count: u16,
        retry_at: Instant,
        expires_at: Instant,
    },
}

impl NativeInventoryTransferPhase {
    pub(in crate::bot) fn action_dispatched(&self) -> bool {
        matches!(
            self,
            Self::WaitingForReceipt { .. } | Self::ReceiptRequested { .. }
        )
    }
}

#[derive(Clone, Debug)]
pub(in crate::bot) struct NativeInventoryTransferTask {
    pub(in crate::bot) id: u64,
    pub(in crate::bot) operation: InventoryTransferOperation,
    pub(in crate::bot) target: IVec3,
    pub(in crate::bot) item: String,
    pub(in crate::bot) requested: u16,
    pub(in crate::bot) moved: u16,
    pub(in crate::bot) actions: u8,
    pub(in crate::bot) deadline: Instant,
    pub(in crate::bot) phase: NativeInventoryTransferPhase,
    pub(in crate::bot) last_error: Option<String>,
}

impl NativeInventoryTransferTask {
    pub(in crate::bot) fn new(
        operation: InventoryTransferOperation,
        target: IVec3,
        item: String,
        requested: u16,
    ) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            operation,
            target,
            item,
            requested,
            moved: 0,
            actions: 0,
            deadline: Instant::now() + NATIVE_INVENTORY_TASK_TIMEOUT,
            phase: NativeInventoryTransferPhase::Ready,
            last_error: None,
        }
    }

    pub(in crate::bot) fn result_status(&self) -> (&str, bool, bool) {
        let outcome_unknown = self
            .last_error
            .as_deref()
            .is_some_and(|error| error.ends_with("outcome_unknown"));
        if outcome_unknown {
            return (
                self.last_error.as_deref().unwrap_or("outcome_unknown"),
                false,
                true,
            );
        }
        if self.moved >= self.requested {
            return (self.operation.completed_status(), true, false);
        }
        if self.moved > 0 {
            return (self.operation.partial_status(), true, false);
        }
        (
            self.last_error.as_deref().unwrap_or("transfer_failed"),
            false,
            false,
        )
    }
}

pub(in crate::bot) const MAX_NATIVE_INVENTORY_ACTIONS: u8 = 8;
pub(in crate::bot) const NATIVE_INVENTORY_TASK_TIMEOUT: Duration = Duration::from_secs(12);
pub(crate) const API_INVENTORY_TRANSFER_TIMEOUT: Duration = Duration::from_secs(15);

pub(in crate::bot) fn continue_native_inventory_transfer(
    native_transfer: &mut Option<NativeInventoryTransferTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) -> Result<()> {
    let Some(task) = native_transfer.as_mut() else {
        return Ok(());
    };
    if !matches!(task.phase, NativeInventoryTransferPhase::Ready) {
        return Ok(());
    }
    if task.moved >= task.requested || task.actions >= MAX_NATIVE_INVENTORY_ACTIONS {
        finish_native_inventory_transfer(native_transfer, pending_replies);
        return Ok(());
    }
    let remaining = task.requested - task.moved;
    let nonce = format!("{:x}-{}", task.id, task.actions.saturating_add(1));
    conn.send_chat_message(&format!(
        "/{} {} {} {} {} {} {} {}",
        task.operation.prepare_command(),
        nonce,
        task.operation.command_name(),
        task.target.x,
        task.target.y,
        task.target.z,
        task.item,
        remaining
    ))?;
    task.phase = NativeInventoryTransferPhase::Preparing {
        nonce,
        deadline: Instant::now() + Duration::from_secs(3),
    };
    Ok(())
}

pub(in crate::bot) fn finish_native_inventory_transfer(
    native_transfer: &mut Option<NativeInventoryTransferTask>,
    pending_replies: &PendingReplies,
) {
    let Some(task) = native_transfer.take() else {
        return;
    };
    let (status, ok, outcome_unknown) = task.result_status();
    let response = json!({
        "ok": ok,
        "status": status,
        "container": task.operation.container_kind(),
        "operation": task.operation.command_name(),
        "target": [task.target.x, task.target.y, task.target.z],
        "item": task.item,
        "requested": task.requested,
        "moved": task.moved,
        "confirmed_moved": task.moved,
        "remaining": task.requested.saturating_sub(task.moved),
        "actions": task.actions,
        "outcome_unknown": outcome_unknown,
        "error": (!ok).then_some(status),
    })
    .to_string();
    let _ = pending_replies.resolve(task.operation.reply_tag(), response);
    println!(
        "native inventory transfer finished: container={} operation={} status={} moved={}/{} actions={}",
        task.operation.container_kind(),
        task.operation.command_name(),
        status,
        task.moved,
        task.requested,
        task.actions
    );
}

pub(in crate::bot) fn fail_native_inventory_transfer(
    native_transfer: &mut Option<NativeInventoryTransferTask>,
    conn: &mut MtpConnection,
    error: impl Into<String>,
    pending_replies: &PendingReplies,
) {
    if let Some(task) = native_transfer.as_mut() {
        let action_dispatched = task.phase.action_dispatched();
        let mut error = error.into();
        if action_dispatched && !error.ends_with("outcome_unknown") {
            error.push_str("_outcome_unknown");
        }
        eprintln!("native inventory transfer failed: {error}");
        task.last_error = Some(error);
        let nonce = match &task.phase {
            NativeInventoryTransferPhase::Preparing { nonce, .. }
            | NativeInventoryTransferPhase::WaitingForReceipt { nonce, .. }
            | NativeInventoryTransferPhase::ReceiptRequested { nonce, .. } => {
                Some(nonce.clone())
            }
            NativeInventoryTransferPhase::Ready => None,
        };
        if let Some(nonce) = nonce {
            if let Err(cancel_error) = conn.send_chat_message(&format!(
                "/{} {nonce}",
                task.operation.cancel_command()
            )) {
                eprintln!("failed to clear inventory receipt {nonce}: {cancel_error}");
            }
        }
    }
    finish_native_inventory_transfer(native_transfer, pending_replies);
}

pub(in crate::bot) fn cancel_native_inventory_transfer(
    native_transfer: &mut Option<NativeInventoryTransferTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) -> Result<()> {
    let Some(mut task) = native_transfer.take() else {
        return Ok(());
    };
    let action_dispatched = task.phase.action_dispatched();
    let nonce = match &task.phase {
        NativeInventoryTransferPhase::Preparing { nonce, .. }
        | NativeInventoryTransferPhase::WaitingForReceipt { nonce, .. }
        | NativeInventoryTransferPhase::ReceiptRequested { nonce, .. } => Some(nonce.clone()),
        NativeInventoryTransferPhase::Ready => None,
    };
    if let Some(nonce) = nonce {
        if let Err(error) = conn.send_chat_message(&format!(
            "/{} {nonce}",
            task.operation.cancel_command()
        )) {
            eprintln!("failed to clear cancelled inventory receipt {nonce}: {error}");
        }
    }
    let status = if action_dispatched {
        "cancelled_outcome_unknown"
    } else {
        "cancelled"
    };
    task.last_error = Some(status.to_string());
    let response = json!({
        "ok": false,
        "status": status,
        "container": task.operation.container_kind(),
        "operation": task.operation.command_name(),
        "target": [task.target.x, task.target.y, task.target.z],
        "item": task.item,
        "requested": task.requested,
        "moved": task.moved,
        "confirmed_moved": task.moved,
        "remaining": task.requested.saturating_sub(task.moved),
        "actions": task.actions,
        "outcome_unknown": action_dispatched,
        "error": status,
    })
    .to_string();
    let _ = pending_replies.resolve(task.operation.reply_tag(), response);
    println!(
        "native inventory transfer cancelled: container={} operation={} moved={}/{} actions={}",
        task.operation.container_kind(),
        task.operation.command_name(),
        task.moved,
        task.requested,
        task.actions
    );
    Ok(())
}

pub(in crate::bot) fn handle_native_inventory_prepare(
    payload: &str,
    native_transfer: &mut Option<NativeInventoryTransferTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) -> Result<()> {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(error) => {
            fail_native_inventory_transfer(
                native_transfer,
                conn,
                format!("invalid_prepare_response:{error}"),
                pending_replies,
            );
            return Ok(());
        }
    };
    let Some(task) = native_transfer.as_ref() else {
        return Ok(());
    };
    let expected_nonce = match &task.phase {
        NativeInventoryTransferPhase::Preparing { nonce, .. } => nonce.clone(),
        _ => return Ok(()),
    };
    if !chest_reply_has_nonce(&value, &expected_nonce) {
        eprintln!("ignoring stale inventory prepare reply");
        return Ok(());
    }
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("prepare_failed");
        fail_native_inventory_transfer(native_transfer, conn, status, pending_replies);
        return Ok(());
    }
    if value
        .get("operation")
        .and_then(serde_json::Value::as_str)
        != Some(task.operation.command_name())
        || value.get("item").and_then(serde_json::Value::as_str) != Some(task.item.as_str())
        || value.get("requested_target").and_then(parse_node_position) != Some(task.target)
    {
        fail_native_inventory_transfer(
            native_transfer,
            conn,
            "prepare_mismatch",
            pending_replies,
        );
        return Ok(());
    }
    let container_pos = value.get("container_pos").and_then(parse_node_position);
    let Some(container_pos) = container_pos else {
        fail_native_inventory_transfer(
            native_transfer,
            conn,
            "prepare_missing_container_position",
            pending_replies,
        );
        return Ok(());
    };
    let separation = (i64::from(container_pos.x) - i64::from(task.target.x)).abs()
        + (i64::from(container_pos.y) - i64::from(task.target.y)).abs()
        + (i64::from(container_pos.z) - i64::from(task.target.z)).abs();
    let allowed_separation = u64::from(task.operation.container_kind() == "chest");
    if u64::try_from(separation).unwrap_or(u64::MAX) > allowed_separation {
        fail_native_inventory_transfer(
            native_transfer,
            conn,
            "prepare_invalid_container_position",
            pending_replies,
        );
        return Ok(());
    }
    let action_count = value
        .get("action_count")
        .and_then(serde_json::Value::as_u64)
        .and_then(|count| u16::try_from(count).ok())
        .filter(|count| *count > 0 && *count <= task.requested.saturating_sub(task.moved));
    let Some(action_count) = action_count else {
        fail_native_inventory_transfer(
            native_transfer,
            conn,
            "prepare_invalid_action_count",
            pending_replies,
        );
        return Ok(());
    };
    let container_list = value
        .get("container_list")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| task.operation.expected_container_list());
    if container_list != task.operation.expected_container_list() {
        fail_native_inventory_transfer(
            native_transfer,
            conn,
            "prepare_invalid_container_list",
            pending_replies,
        );
        return Ok(());
    }
    if task.operation.deposits_into_container() {
        let player_slot = value
            .get("player_slot")
            .and_then(serde_json::Value::as_u64)
            .and_then(|slot| u16::try_from(slot).ok());
        let Some(player_slot) = player_slot else {
            fail_native_inventory_transfer(
                native_transfer,
                conn,
                "prepare_invalid_player_slot",
                pending_replies,
            );
            return Ok(());
        };
        conn.send_inventory_move_somewhere(
            action_count,
            InventoryLocation::CurrentPlayer,
            "main",
            player_slot,
            InventoryLocation::NodeMeta(container_pos),
            container_list,
        )?;
    } else {
        let container_slot = value
            .get("container_slot")
            .or_else(|| value.get("furnace_slot"))
            .or_else(|| value.get("chest_slot"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|slot| u16::try_from(slot).ok());
        let Some(container_slot) = container_slot else {
            fail_native_inventory_transfer(
                native_transfer,
                conn,
                "prepare_invalid_container_slot",
                pending_replies,
            );
            return Ok(());
        };
        conn.send_inventory_move_somewhere(
            action_count,
            InventoryLocation::NodeMeta(container_pos),
            container_list,
            container_slot,
            InventoryLocation::CurrentPlayer,
            "main",
        )?;
    }

    if let Some(task) = native_transfer.as_mut() {
        task.actions = task.actions.saturating_add(1);
        let now = Instant::now();
        task.phase = NativeInventoryTransferPhase::WaitingForReceipt {
            nonce: expected_nonce,
            action_count,
            poll_at: now + Duration::from_millis(200),
            expires_at: now + Duration::from_secs(4),
        };
    }
    Ok(())
}

pub(in crate::bot) fn handle_native_inventory_receipt(
    payload: &str,
    native_transfer: &mut Option<NativeInventoryTransferTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) -> Result<()> {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(error) => {
            fail_native_inventory_transfer(
                native_transfer,
                conn,
                format!("invalid_receipt_response:{error}"),
                pending_replies,
            );
            return Ok(());
        }
    };
    let Some(task) = native_transfer.as_mut() else {
        return Ok(());
    };
    let (expected_nonce, action_count, expires_at) = match &task.phase {
        NativeInventoryTransferPhase::ReceiptRequested {
            nonce,
            action_count,
            expires_at,
            ..
        } => (nonce.clone(), *action_count, *expires_at),
        _ => return Ok(()),
    };
    if !chest_reply_has_nonce(&value, &expected_nonce) {
        eprintln!("ignoring stale inventory receipt reply");
        return Ok(());
    }
    if value.get("status").and_then(serde_json::Value::as_str) == Some("pending") {
        task.phase = NativeInventoryTransferPhase::WaitingForReceipt {
            nonce: expected_nonce,
            action_count,
            poll_at: Instant::now() + Duration::from_millis(200),
            expires_at,
        };
        return Ok(());
    }
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("receipt_failed");
        fail_native_inventory_transfer(native_transfer, conn, status, pending_replies);
        return Ok(());
    }
    if value
        .get("operation")
        .and_then(serde_json::Value::as_str)
        != Some(task.operation.command_name())
        || value.get("item").and_then(serde_json::Value::as_str) != Some(task.item.as_str())
        || value.get("requested_target").and_then(parse_node_position) != Some(task.target)
    {
        fail_native_inventory_transfer(
            native_transfer,
            conn,
            "receipt_mismatch",
            pending_replies,
        );
        return Ok(());
    }
    let moved = value
        .get("moved")
        .and_then(serde_json::Value::as_u64)
        .and_then(|count| u16::try_from(count).ok())
        .filter(|count| *count > 0 && *count <= action_count);
    let Some(moved) = moved else {
        fail_native_inventory_transfer(
            native_transfer,
            conn,
            "receipt_invalid_moved_count",
            pending_replies,
        );
        return Ok(());
    };
    task.moved = task.moved.saturating_add(moved).min(task.requested);
    task.phase = NativeInventoryTransferPhase::Ready;
    continue_native_inventory_transfer(native_transfer, conn, pending_replies)
}

pub(in crate::bot) fn advance_native_inventory_transfer(
    native_transfer: &mut Option<NativeInventoryTransferTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) -> Result<()> {
    let now = Instant::now();
    if native_transfer
        .as_ref()
        .is_some_and(|task| now >= task.deadline)
    {
        fail_native_inventory_transfer(
            native_transfer,
            conn,
            "task_timeout",
            pending_replies,
        );
        return Ok(());
    }
    let phase = native_transfer.as_ref().map(|task| task.phase.clone());
    match phase {
        Some(NativeInventoryTransferPhase::Preparing { deadline, .. }) if now >= deadline => {
            fail_native_inventory_transfer(
                native_transfer,
                conn,
                "prepare_timeout",
                pending_replies,
            );
        }
        Some(NativeInventoryTransferPhase::WaitingForReceipt { expires_at, .. })
            if now >= expires_at =>
        {
            fail_native_inventory_transfer(
                native_transfer,
                conn,
                "receipt_timeout",
                pending_replies,
            );
        }
        Some(NativeInventoryTransferPhase::WaitingForReceipt {
            nonce,
            action_count,
            poll_at,
            expires_at,
        }) if now >= poll_at => {
            let Some(task) = native_transfer.as_ref() else {
                return Ok(());
            };
            conn.send_chat_message(&format!(
                "/{} {nonce}",
                task.operation.receipt_command()
            ))?;
            if let Some(task) = native_transfer.as_mut() {
                task.phase = NativeInventoryTransferPhase::ReceiptRequested {
                    nonce,
                    action_count,
                    retry_at: now + Duration::from_millis(500),
                    expires_at,
                };
            }
        }
        Some(NativeInventoryTransferPhase::ReceiptRequested { expires_at, .. })
            if now >= expires_at =>
        {
            fail_native_inventory_transfer(
                native_transfer,
                conn,
                "receipt_timeout",
                pending_replies,
            );
        }
        Some(NativeInventoryTransferPhase::ReceiptRequested {
            nonce,
            action_count,
            retry_at,
            expires_at,
        }) if now >= retry_at => {
            if let Some(task) = native_transfer.as_mut() {
                task.phase = NativeInventoryTransferPhase::WaitingForReceipt {
                    nonce,
                    action_count,
                    poll_at: now,
                    expires_at,
                };
            }
        }
        _ => {}
    }
    Ok(())
}
