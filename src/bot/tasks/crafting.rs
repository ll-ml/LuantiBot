//! Native crafting-grid preparation, execution, and receipt verification.

use anyhow::Result;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::network::{InventoryLocation, MtpConnection};
use crate::bot::replies::PendingReplies;
use crate::bot::validation::chest_reply_has_nonce;

#[derive(Clone, Debug)]
pub(in crate::bot) struct CraftGridMove {
    player_slot: u16,
    craft_slot: u16,
    count: u16,
}

#[derive(Clone, Debug)]
pub(in crate::bot) struct PreparedCraft {
    nonce: String,
    item: String,
    requested: u16,
    batches: u16,
    produced: u16,
    grid_size: u16,
    used_table: bool,
    moves: Vec<CraftGridMove>,
    ingredients: serde_json::Value,
}

#[derive(Clone, Debug)]
pub(in crate::bot) enum NativeCraftPhase {
    Ready,
    Preparing {
        nonce: String,
        deadline: Instant,
    },
    Placing {
        prepared: PreparedCraft,
        craft_at: Instant,
    },
    Crafting {
        prepared: PreparedCraft,
        collect_at: Instant,
    },
    WaitingForReceipt {
        prepared: PreparedCraft,
        poll_at: Instant,
        expires_at: Instant,
    },
    ReceiptRequested {
        prepared: PreparedCraft,
        retry_at: Instant,
        expires_at: Instant,
    },
}

impl NativeCraftPhase {
    pub(in crate::bot) fn action_dispatched(&self) -> bool {
        matches!(
            self,
            Self::Crafting { .. }
                | Self::WaitingForReceipt { .. }
                | Self::ReceiptRequested { .. }
        )
    }
}

#[derive(Clone, Debug)]
pub(in crate::bot) struct NativeCraftTask {
    pub(in crate::bot) id: u64,
    pub(in crate::bot) item: String,
    pub(in crate::bot) requested: u16,
    pub(in crate::bot) phase: NativeCraftPhase,
}

impl NativeCraftTask {
    pub(in crate::bot) fn new(item: String, requested: u16) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            item,
            requested,
            phase: NativeCraftPhase::Ready,
        }
    }
}

pub(in crate::bot) fn continue_native_craft(
    native_craft: &mut Option<NativeCraftTask>,
    conn: &mut MtpConnection,
) -> Result<()> {
    let Some(task) = native_craft.as_mut() else {
        return Ok(());
    };
    if !matches!(task.phase, NativeCraftPhase::Ready) {
        return Ok(());
    }
    let nonce = format!("craft-{:x}", task.id);
    conn.send_chat_message(&format!(
        "/bot_craft_prepare {} {} {}",
        nonce, task.item, task.requested
    ))?;
    task.phase = NativeCraftPhase::Preparing {
        nonce,
        deadline: Instant::now() + Duration::from_secs(4),
    };
    Ok(())
}

pub(in crate::bot) fn craft_phase_nonce(phase: &NativeCraftPhase) -> Option<&str> {
    match phase {
        NativeCraftPhase::Ready => None,
        NativeCraftPhase::Preparing { nonce, .. } => Some(nonce),
        NativeCraftPhase::Placing { prepared, .. }
        | NativeCraftPhase::Crafting { prepared, .. }
        | NativeCraftPhase::WaitingForReceipt { prepared, .. }
        | NativeCraftPhase::ReceiptRequested { prepared, .. } => Some(&prepared.nonce),
    }
}

pub(in crate::bot) fn craft_phase_prepared(phase: &NativeCraftPhase) -> Option<&PreparedCraft> {
    match phase {
        NativeCraftPhase::Placing { prepared, .. }
        | NativeCraftPhase::Crafting { prepared, .. }
        | NativeCraftPhase::WaitingForReceipt { prepared, .. }
        | NativeCraftPhase::ReceiptRequested { prepared, .. } => Some(prepared),
        NativeCraftPhase::Ready | NativeCraftPhase::Preparing { .. } => None,
    }
}

pub(in crate::bot) fn clean_native_craft_grid(conn: &mut MtpConnection, prepared: &PreparedCraft) {
    let mut craft_slots = prepared
        .moves
        .iter()
        .map(|movement| movement.craft_slot)
        .collect::<Vec<_>>();
    craft_slots.sort_unstable();
    craft_slots.dedup();
    for slot in craft_slots {
        if let Err(error) = conn.send_inventory_move_somewhere(
            u16::MAX,
            InventoryLocation::CurrentPlayer,
            "craft",
            slot,
            InventoryLocation::CurrentPlayer,
            "main",
        ) {
            eprintln!("failed to return craft-grid slot {slot}: {error}");
        }
    }
    if let Err(error) = conn.send_inventory_move_somewhere(
        u16::MAX,
        InventoryLocation::CurrentPlayer,
        "craftresult",
        0,
        InventoryLocation::CurrentPlayer,
        "main",
    ) {
        eprintln!("failed to return craft result: {error}");
    }
}

pub(in crate::bot) fn resolve_native_craft(
    native_craft: &mut Option<NativeCraftTask>,
    pending_replies: &PendingReplies,
    ok: bool,
    status: &str,
    delivered: u16,
    prepared: Option<&PreparedCraft>,
) {
    let Some(task) = native_craft.take() else {
        return;
    };
    let produced = prepared.map_or(0, |prepared| prepared.produced);
    let batches = prepared.map_or(0, |prepared| prepared.batches);
    let grid_size = prepared.map_or(0, |prepared| prepared.grid_size);
    let used_table = prepared.is_some_and(|prepared| prepared.used_table);
    let ingredients = prepared
        .map(|prepared| prepared.ingredients.clone())
        .unwrap_or_else(|| json!([]));
    let outcome_unknown = status.ends_with("outcome_unknown");
    let response = json!({
        "ok": ok,
        "status": status,
        "item": task.item,
        "requested": task.requested,
        "produced": produced,
        "delivered": delivered,
        "batches": batches,
        "grid_size": grid_size,
        "surplus": delivered.saturating_sub(task.requested),
        "used_table": used_table,
        "ingredients": ingredients,
        "outcome_unknown": outcome_unknown,
        "error": (!ok).then_some(status),
    })
    .to_string();
    let _ = pending_replies.resolve("BOT_CRAFT", response);
    println!(
        "native craft finished: item={} status={} delivered={}/{} batches={}",
        task.item, status, delivered, task.requested, batches
    );
}

pub(in crate::bot) fn fail_native_craft(
    native_craft: &mut Option<NativeCraftTask>,
    conn: &mut MtpConnection,
    error: impl Into<String>,
    pending_replies: &PendingReplies,
) {
    let mut error = error.into();
    let Some(task) = native_craft.as_ref() else {
        return;
    };
    if task.phase.action_dispatched() && !error.ends_with("outcome_unknown") {
        error.push_str("_outcome_unknown");
    }
    eprintln!("native craft failed: {error}");
    let nonce = craft_phase_nonce(&task.phase).map(str::to_owned);
    let prepared = craft_phase_prepared(&task.phase).cloned();
    if let Some(prepared) = prepared.as_ref() {
        clean_native_craft_grid(conn, prepared);
    }
    if let Some(nonce) = nonce {
        if let Err(cancel_error) = conn.send_chat_message(&format!("/bot_craft_cancel {nonce}")) {
            eprintln!("failed to cancel craft receipt {nonce}: {cancel_error}");
        }
    }
    resolve_native_craft(
        native_craft,
        pending_replies,
        false,
        &error,
        0,
        prepared.as_ref(),
    );
}

pub(in crate::bot) fn cancel_native_craft(
    native_craft: &mut Option<NativeCraftTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) -> Result<()> {
    if native_craft.is_some() {
        fail_native_craft(native_craft, conn, "cancelled", pending_replies);
    }
    Ok(())
}

pub(in crate::bot) fn bounded_json_u16(
    value: &serde_json::Value,
    key: &str,
    minimum: u16,
    maximum: u16,
) -> Option<u16> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|number| u16::try_from(number).ok())
        .filter(|number| *number >= minimum && *number <= maximum)
}

pub(in crate::bot) fn handle_native_craft_prepare(
    payload: &str,
    native_craft: &mut Option<NativeCraftTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) -> Result<()> {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(error) => {
            fail_native_craft(
                native_craft,
                conn,
                format!("invalid_prepare_response:{error}"),
                pending_replies,
            );
            return Ok(());
        }
    };
    let Some(task) = native_craft.as_ref() else {
        return Ok(());
    };
    let expected_nonce = match &task.phase {
        NativeCraftPhase::Preparing { nonce, .. } => nonce.clone(),
        _ => return Ok(()),
    };
    if !chest_reply_has_nonce(&value, &expected_nonce) {
        eprintln!("ignoring stale craft prepare reply");
        return Ok(());
    }
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("prepare_failed");
        fail_native_craft(native_craft, conn, status, pending_replies);
        return Ok(());
    }
    if value.get("item").and_then(serde_json::Value::as_str) != Some(task.item.as_str()) {
        fail_native_craft(native_craft, conn, "prepare_mismatch", pending_replies);
        return Ok(());
    }
    let requested = bounded_json_u16(&value, "requested", 1, 64).unwrap_or(task.requested);
    let batches = bounded_json_u16(&value, "batches", 1, 8);
    let produced = bounded_json_u16(&value, "produced", requested, 64)
        .or_else(|| bounded_json_u16(&value, "output_count", requested, 64));
    let grid_size = bounded_json_u16(&value, "grid_size", 4, 9);
    let (Some(batches), Some(produced), Some(grid_size)) = (batches, produced, grid_size) else {
        fail_native_craft(
            native_craft,
            conn,
            "prepare_invalid_counts",
            pending_replies,
        );
        return Ok(());
    };
    if !matches!(grid_size, 4 | 9) {
        fail_native_craft(
            native_craft,
            conn,
            "prepare_invalid_grid_size",
            pending_replies,
        );
        return Ok(());
    }
    if requested != task.requested {
        fail_native_craft(
            native_craft,
            conn,
            "prepare_requested_count_mismatch",
            pending_replies,
        );
        return Ok(());
    }
    let moves = value
        .get("moves")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|movement| {
            let player_slot = bounded_json_u16(movement, "player_slot", 0, u16::MAX)?;
            let craft_slot = bounded_json_u16(movement, "craft_slot", 0, grid_size - 1)?;
            let count = bounded_json_u16(movement, "count", 1, 99)?;
            Some(CraftGridMove {
                player_slot,
                craft_slot,
                count,
            })
        })
        .collect::<Vec<_>>();
    let raw_move_count = value
        .get("moves")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    if moves.is_empty() || moves.len() != raw_move_count || moves.len() > 36 {
        fail_native_craft(
            native_craft,
            conn,
            "prepare_invalid_moves",
            pending_replies,
        );
        return Ok(());
    }
    let used_table = value
        .get("used_table")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(grid_size == 9);
    if grid_size == 9 && !used_table {
        fail_native_craft(
            native_craft,
            conn,
            "crafting_table_required",
            pending_replies,
        );
        return Ok(());
    }
    let prepared = PreparedCraft {
        nonce: expected_nonce,
        item: task.item.clone(),
        requested,
        batches,
        produced,
        grid_size,
        used_table,
        moves,
        ingredients: value.get("ingredients").cloned().unwrap_or_else(|| json!([])),
    };
    if let Some(task) = native_craft.as_mut() {
        task.phase = NativeCraftPhase::Placing {
            prepared: prepared.clone(),
            craft_at: Instant::now() + Duration::from_millis(250),
        };
    }
    for movement in &prepared.moves {
        if let Err(error) = conn.send_inventory_move(
            movement.count,
            InventoryLocation::CurrentPlayer,
            "main",
            movement.player_slot,
            InventoryLocation::CurrentPlayer,
            "craft",
            movement.craft_slot,
        ) {
            fail_native_craft(
                native_craft,
                conn,
                format!("ingredient_move_send_failed:{error}"),
                pending_replies,
            );
            return Ok(());
        }
    }
    Ok(())
}

pub(in crate::bot) fn handle_native_craft_receipt(
    payload: &str,
    native_craft: &mut Option<NativeCraftTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(error) => {
            fail_native_craft(
                native_craft,
                conn,
                format!("invalid_receipt_response:{error}"),
                pending_replies,
            );
            return;
        }
    };
    let Some(task) = native_craft.as_mut() else {
        return;
    };
    let (prepared, expires_at) = match &task.phase {
        NativeCraftPhase::ReceiptRequested {
            prepared,
            expires_at,
            ..
        } => (prepared.clone(), *expires_at),
        _ => return,
    };
    if !chest_reply_has_nonce(&value, &prepared.nonce) {
        eprintln!("ignoring stale craft receipt reply");
        return;
    }
    if value.get("status").and_then(serde_json::Value::as_str) == Some("pending") {
        task.phase = NativeCraftPhase::WaitingForReceipt {
            prepared,
            poll_at: Instant::now() + Duration::from_millis(200),
            expires_at,
        };
        return;
    }
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("receipt_failed");
        fail_native_craft(native_craft, conn, status, pending_replies);
        return;
    }
    if value.get("item").and_then(serde_json::Value::as_str) != Some(prepared.item.as_str()) {
        fail_native_craft(native_craft, conn, "receipt_mismatch", pending_replies);
        return;
    }
    let delivered = bounded_json_u16(&value, "delivered", 1, prepared.produced)
        .or_else(|| bounded_json_u16(&value, "moved", 1, prepared.produced));
    let Some(delivered) = delivered else {
        fail_native_craft(
            native_craft,
            conn,
            "receipt_invalid_delivered_count",
            pending_replies,
        );
        return;
    };
    let ok = delivered >= prepared.requested;
    let status = if ok { "crafted" } else { "partially_crafted" };
    resolve_native_craft(
        native_craft,
        pending_replies,
        ok,
        status,
        delivered,
        Some(&prepared),
    );
}

pub(in crate::bot) fn advance_native_craft(
    native_craft: &mut Option<NativeCraftTask>,
    conn: &mut MtpConnection,
    pending_replies: &PendingReplies,
) -> Result<()> {
    let now = Instant::now();
    let phase = native_craft.as_ref().map(|task| task.phase.clone());
    match phase {
        Some(NativeCraftPhase::Preparing { deadline, .. }) if now >= deadline => {
            fail_native_craft(native_craft, conn, "prepare_timeout", pending_replies);
        }
        Some(NativeCraftPhase::Placing { prepared, craft_at }) if now >= craft_at => {
            if let Err(error) = conn.send_inventory_craft(prepared.batches) {
                fail_native_craft(
                    native_craft,
                    conn,
                    format!("craft_send_failed:{error}"),
                    pending_replies,
                );
                return Ok(());
            }
            if let Some(task) = native_craft.as_mut() {
                task.phase = NativeCraftPhase::Crafting {
                    prepared,
                    collect_at: now + Duration::from_millis(250),
                };
            }
        }
        Some(NativeCraftPhase::Crafting {
            prepared,
            collect_at,
        }) if now >= collect_at => {
            if let Err(error) = conn.send_inventory_move_somewhere(
                prepared.produced,
                InventoryLocation::CurrentPlayer,
                "craftresult",
                0,
                InventoryLocation::CurrentPlayer,
                "main",
            ) {
                fail_native_craft(
                    native_craft,
                    conn,
                    format!("output_move_send_failed:{error}"),
                    pending_replies,
                );
                return Ok(());
            }
            if let Some(task) = native_craft.as_mut() {
                task.phase = NativeCraftPhase::WaitingForReceipt {
                    prepared,
                    poll_at: now + Duration::from_millis(200),
                    expires_at: now + Duration::from_secs(5),
                };
            }
        }
        Some(NativeCraftPhase::WaitingForReceipt { expires_at, .. }) if now >= expires_at => {
            fail_native_craft(native_craft, conn, "receipt_timeout", pending_replies);
        }
        Some(NativeCraftPhase::WaitingForReceipt {
            prepared,
            poll_at,
            expires_at,
        }) if now >= poll_at => {
            if let Err(error) =
                conn.send_chat_message(&format!("/bot_craft_receipt {}", prepared.nonce))
            {
                fail_native_craft(
                    native_craft,
                    conn,
                    format!("receipt_send_failed:{error}"),
                    pending_replies,
                );
                return Ok(());
            }
            if let Some(task) = native_craft.as_mut() {
                task.phase = NativeCraftPhase::ReceiptRequested {
                    prepared,
                    retry_at: now + Duration::from_millis(500),
                    expires_at,
                };
            }
        }
        Some(NativeCraftPhase::ReceiptRequested { expires_at, .. }) if now >= expires_at => {
            fail_native_craft(native_craft, conn, "receipt_timeout", pending_replies);
        }
        Some(NativeCraftPhase::ReceiptRequested {
            prepared,
            retry_at,
            expires_at,
        }) if now >= retry_at => {
            if let Some(task) = native_craft.as_mut() {
                task.phase = NativeCraftPhase::WaitingForReceipt {
                    prepared,
                    poll_at: now,
                    expires_at,
                };
            }
        }
        _ => {}
    }
    Ok(())
}
