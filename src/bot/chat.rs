//! Player chat parsing, authorization, history, and protocol replies.

use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use super::replies::PendingReplies;

#[derive(Clone, Debug)]
pub(crate) struct ChatEntry {
    pub(in crate::bot) id: u64,
    ts_ms: u128,
    sender: String,
    message: String,
}

pub(crate) struct ChatLog {
    next_id: u64,
    entries: VecDeque<ChatEntry>,
}

impl Default for ChatLog {
    fn default() -> Self {
        Self {
            // A cursor of zero means "no messages seen yet" to API clients, so
            // real chat entries start at one.
            next_id: 1,
            entries: VecDeque::new(),
        }
    }
}

pub(super) fn extract_chat_player(message: &str) -> Option<String> {
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

pub(super) fn effective_chat_sender_message(sender: &str, message: &str) -> (String, String) {
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

pub(super) fn bot_protocol_message(message: &str) -> Option<(&str, &str)> {
    if !message.starts_with("BOT_") {
        return None;
    }
    Some(message.split_once(' ').unwrap_or((message, "")))
}

pub(super) fn is_invalid_server_command(sender: &str, message: &str, command: &str) -> bool {
    sender.contains("Invalid command") && message.trim().trim_start_matches('/') == command
}

pub(super) fn resolve_unsupported_bot_command(
    pending_replies: &PendingReplies,
    sender: &str,
    message: &str,
) -> Option<&'static str> {
    const COMMAND_REPLIES: &[(&str, &str)] = &[
        ("bot_attack", "BOT_ATTACK"),
        ("bot_attack_mobs", "BOT_DEFEND"),
        ("bot_approach", "BOT_APPROACH"),
        ("bot_interact", "BOT_INTERACT"),
        ("bot_fight", "BOT_FIGHT"),
        ("bot_sleep", "BOT_SLEEP"),
        ("bot_mine", "BOT_MINE"),
        ("bot_collect", "BOT_COLLECT"),
        ("bot_prepare_mine", "BOT_MINE_PREPARE"),
        ("bot_verify_mine", "BOT_MINE_VERIFY"),
        ("bot_chest_inspect", "BOT_CHEST_INSPECT"),
        ("bot_chest_prepare", "BOT_CHEST_PREPARE"),
        ("bot_chest_receipt", "BOT_CHEST_RECEIPT"),
        ("bot_chest_cancel", "BOT_CHEST_CANCEL"),
        ("bot_chest_verify", "BOT_CHEST_VERIFY"),
        ("bot_furnace_inspect", "BOT_FURNACE_INSPECT"),
        ("bot_furnace_prepare", "BOT_FURNACE_PREPARE"),
        ("bot_furnace_receipt", "BOT_FURNACE_RECEIPT"),
        ("bot_furnace_cancel", "BOT_FURNACE_CANCEL"),
        ("bot_craft_prepare", "BOT_CRAFT_PREPARE"),
        ("bot_craft_receipt", "BOT_CRAFT_RECEIPT"),
        ("bot_craft_cancel", "BOT_CRAFT_CANCEL"),
        ("bot_place", "BOT_PLACE"),
        ("bot_drop", "BOT_DROP"),
        ("bot_wield", "BOT_WIELD"),
        ("bot_use", "BOT_USE"),
        ("bot_path_node", "BOT_PATH_NODE"),
        ("bot_gather_path", "BOT_GATHER_PATH"),
        ("bot_hunt_path", "BOT_HUNT_PATH"),
        ("bot_hunt", "BOT_HUNT"),
    ];

    COMMAND_REPLIES.iter().find_map(|&(command, tag)| {
        if !is_invalid_server_command(sender, message, command) {
            return None;
        }
        let response = json!({
            "ok": false,
            "status": "unsupported_command",
            "command": command,
        })
        .to_string();
        let _ = pending_replies.resolve(tag, response);
        Some(command)
    })
}

pub(super) fn parse_sender_colon_message(message: &str) -> Option<(String, String)> {
    let mut parts = message.splitn(2, ':');
    let name = parts.next()?.trim();
    let rest = parts.next()?.trim();
    if name.is_empty() || rest.is_empty() {
        return None;
    }
    Some((name.to_string(), rest.to_string()))
}

pub(super) fn parse_sender_angle_message(message: &str) -> Option<(String, String)> {
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

pub(super) fn normalize_player_name(name: &str) -> String {
    let trimmed = name.trim();
    if let Some(stripped) = trimmed.strip_prefix("@__builtin)") {
        return stripped.trim().to_string();
    }
    trimmed.to_string()
}

#[derive(Clone, Debug)]
pub(super) enum ControlCommand {
    Follow(String),
    Teleport(String),
    Stop,
    Where,
    Attack(String),
    AttackMobs(Option<i32>),
    Sleep(Option<i32>),
    Approach(String),
    Interact(String),
    Fight(String),
}

pub(super) fn parse_control_command(message: &str) -> Option<ControlCommand> {
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
            Some(ControlCommand::Attack(target))
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

pub(super) fn parse_allowlist(value: &str) -> Vec<String> {
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

pub(super) fn is_sender_allowed(allow_list: &[String], sender: &str) -> bool {
    if allow_list.is_empty() {
        return true;
    }
    let name = normalize_player_name(sender);
    allow_list.iter().any(|allowed| allowed == &name)
}

pub(crate) fn json_escape(value: &str) -> String {
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

pub(super) fn log_chat(chat_log: &Arc<Mutex<ChatLog>>, sender: &str, message: &str) {
    if let Ok(mut log) = chat_log.lock() {
        let id = log.next_id;
        log.next_id = log.next_id.saturating_add(1);
        log.entries.push_back(ChatEntry {
            id,
            ts_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            sender: sender.to_string(),
            message: message.to_string(),
        });
        while log.entries.len() > 200 {
            log.entries.pop_front();
        }
    }
}

pub(crate) fn collect_chat_entries(
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

pub(crate) fn build_chat_json(entries: Vec<ChatEntry>, last_id: u64) -> String {
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
