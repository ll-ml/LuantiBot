use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use serde_json::json;

use crate::bot::{
    ApiCommand, ChatLog, InventoryTransferOperation, MoveRequest, MoveSpec, PendingReplies,
    API_INVENTORY_TRANSFER_TIMEOUT, build_chat_json, collect_chat_entries, valid_command_atom,
};
use crate::types::{IVec3, Vec3};
use super::replies::{await_bot_reply, send_and_wait_for_reply, BotReplyError};
use super::request::{
    parse_json_field, parse_json_value, parse_move_direction, parse_query,
    query_or_json_bounded_u16, query_or_json_i32, query_or_json_position,
    read_http_request, request_is_authorized,
};
use super::response::{json_error, json_ok, write_http_response, write_json_error};
use super::telemetry::AgentTelemetryStore;

pub(super) fn handle_api_connection(
    mut stream: std::net::TcpStream,
    token: &str,
    tx: mpsc::Sender<ApiCommand>,
    last_pos: Arc<Mutex<Vec3>>,
    pending_replies: PendingReplies,
    chat_log: Arc<Mutex<ChatLog>>,
    agent_telemetry: AgentTelemetryStore,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut req_bytes = match read_http_request(&mut stream) {
        Ok(Some(request)) => request,
        Ok(None) | Err(_) => return,
    };
    let req_str = String::from_utf8_lossy(&req_bytes).to_string();
    if !req_str.contains("\r\n\r\n") {
        return;
    }
    let mut lines = req_str.lines();
    let request_line = match lines.next() {
        Some(v) => v,
        None => return,
    };
    // Keep request parsing and response state local to this connection.
    {
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");

        if method == "OPTIONS" {
            write_http_response(&mut stream, "204 No Content", "text/plain", "");
            return;
        }

        let auth_ok = request_is_authorized(lines.clone(), token);

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
            write_http_response(&mut stream, "401 Unauthorized", "text/plain", "unauthorized");
            return;
        }

        let mut response = "OK".to_string();
        let mut response_type = "text/plain";
        let mut handled = true;
        match (method, endpoint) {
            ("GET", "/health") => {}
            ("GET", "/agent/telemetry") => {
                response = agent_telemetry.snapshot().to_string();
                response_type = "application/json";
            }
            ("POST", "/agent/telemetry") => match agent_telemetry.publish(&body) {
                Ok(published) => {
                    response = json!({
                        "ok": true,
                        "revision": published.revision,
                        "changed": published.changed,
                    })
                    .to_string();
                    response_type = "application/json";
                }
                Err(error) => {
                    write_json_error(&mut stream, "400 Bad Request", error);
                    return;
                }
            },
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
                        write_http_response(
                            &mut stream,
                            "504 Gateway Timeout",
                            "text/plain",
                            "observe timeout",
                        );
                        return;
                    }
                }
            }
            ("GET", "/observe_server") => {
                let radius = params
                    .get("radius")
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(2)
                    .clamp(1, 8);
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_OBSERVE",
                    ApiCommand::ObserveServer { radius },
                    Duration::from_secs(2),
                    "observe_server_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
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
                    return;
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
                    let clipped: String = msg.chars().take(256).collect();
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
                    let Some(body) = await_bot_reply(
                        &mut stream,
                        &tx,
                        &pending_replies,
                        "BOT_ATTACK",
                        ApiCommand::Attack(target),
                        Duration::from_secs(2),
                        "attack_timeout",
                    ) else {
                        return;
                    };
                    response = body;
                    response_type = "application/json";
                } else {
                    handled = false;
                }
            }
            ("POST", "/defend") => {
                let radius = query_or_json_i32(&params, &body, "radius")
                    .unwrap_or(12)
                    .clamp(1, 20);
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_DEFEND",
                    ApiCommand::AttackMobs(Some(radius)),
                    Duration::from_secs(2),
                    "defend_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/approach") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"));
                if let Some(target) = target {
                    let Some(body) = await_bot_reply(
                        &mut stream,
                        &tx,
                        &pending_replies,
                        "BOT_APPROACH",
                        ApiCommand::Approach(target),
                        Duration::from_secs(2),
                        "approach_timeout",
                    ) else {
                        return;
                    };
                    response = body;
                    response_type = "application/json";
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
                    let Some(body) = await_bot_reply(
                        &mut stream,
                        &tx,
                        &pending_replies,
                        "BOT_INTERACT",
                        ApiCommand::Interact(target),
                        Duration::from_secs(2),
                        "interact_timeout",
                    ) else {
                        return;
                    };
                    response = body;
                    response_type = "application/json";
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
                    let Some(body) = await_bot_reply(
                        &mut stream,
                        &tx,
                        &pending_replies,
                        "BOT_FIGHT",
                        ApiCommand::Fight(target),
                        Duration::from_secs(2),
                        "fight_timeout",
                    ) else {
                        return;
                    };
                    response = body;
                    response_type = "application/json";
                } else {
                    handled = false;
                }
            }
            ("POST", "/sleep") => {
                let radius = query_or_json_i32(&params, &body, "radius")
                    .unwrap_or(6)
                    .clamp(1, 20);
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_SLEEP",
                    ApiCommand::Sleep(Some(radius)),
                    Duration::from_secs(2),
                    "sleep_timeout",
                ) else {
                    return;
                };
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
            ("GET", "/chest") | ("POST", "/chest/inspect") => {
                let Some(pos) = query_or_json_position(&params, &body) else {
                    write_json_error(&mut stream, "400 Bad Request", "missing_or_invalid_position");
                    return;
                };
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_CHEST_INSPECT",
                    ApiCommand::ChestInspect(pos),
                    Duration::from_secs(3),
                    "chest_inspect_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/chest/deposit") | ("POST", "/chest/withdraw") => {
                let Some(pos) = query_or_json_position(&params, &body) else {
                    write_json_error(&mut stream, "400 Bad Request", "missing_or_invalid_position");
                    return;
                };
                let item = params
                    .get("item")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "item"));
                let Some(item) = item.filter(|item| valid_command_atom(item)) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_item");
                    return;
                };
                let Some(count) = query_or_json_bounded_u16(&params, &body, "count", 1, 99)
                else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_count");
                    return;
                };
                let deposit = endpoint == "/chest/deposit";
                let tag = if deposit {
                    InventoryTransferOperation::ChestDeposit.reply_tag()
                } else {
                    InventoryTransferOperation::ChestWithdraw.reply_tag()
                };
                let command = if deposit {
                    ApiCommand::ChestDeposit { pos, item, count }
                } else {
                    ApiCommand::ChestWithdraw { pos, item, count }
                };
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    tag,
                    command,
                    API_INVENTORY_TRANSFER_TIMEOUT,
                    "chest_transfer_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("GET", "/furnace") | ("POST", "/furnace/inspect") => {
                let Some(pos) = query_or_json_position(&params, &body) else {
                    write_json_error(&mut stream, "400 Bad Request", "missing_or_invalid_position");
                    return;
                };
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_FURNACE_INSPECT",
                    ApiCommand::FurnaceInspect(pos),
                    Duration::from_secs(3),
                    "furnace_inspect_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/furnace/input")
            | ("POST", "/furnace/fuel")
            | ("POST", "/furnace/output")
            | ("POST", "/furnace/collect") => {
                let Some(pos) = query_or_json_position(&params, &body) else {
                    write_json_error(&mut stream, "400 Bad Request", "missing_or_invalid_position");
                    return;
                };
                let item = params
                    .get("item")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "item"));
                let Some(item) = item.filter(|item| valid_command_atom(item)) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_item");
                    return;
                };
                let Some(count) = query_or_json_bounded_u16(&params, &body, "count", 1, 99)
                else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_count");
                    return;
                };
                let operation = match endpoint {
                    "/furnace/input" => InventoryTransferOperation::FurnaceInput,
                    "/furnace/fuel" => InventoryTransferOperation::FurnaceFuel,
                    _ => InventoryTransferOperation::FurnaceOutput,
                };
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    operation.reply_tag(),
                    ApiCommand::FurnaceTransfer {
                        operation,
                        pos,
                        item,
                        count,
                    },
                    API_INVENTORY_TRANSFER_TIMEOUT,
                    "furnace_transfer_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/furnace/load") => {
                let Some(pos) = query_or_json_position(&params, &body) else {
                    write_json_error(&mut stream, "400 Bad Request", "missing_or_invalid_position");
                    return;
                };
                let input = params
                    .get("input")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "input"));
                let fuel = params
                    .get("fuel")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "fuel"));
                let Some(input) = input.filter(|item| valid_command_atom(item)) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_input_item");
                    return;
                };
                let Some(fuel) = fuel.filter(|item| valid_command_atom(item)) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_fuel_item");
                    return;
                };
                let Some(input_count) =
                    query_or_json_bounded_u16(&params, &body, "input_count", 1, 99)
                else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_input_count");
                    return;
                };
                let Some(fuel_count) =
                    query_or_json_bounded_u16(&params, &body, "fuel_count", 1, 99)
                else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_fuel_count");
                    return;
                };

                let input_reply = send_and_wait_for_reply(
                    &tx,
                    &pending_replies,
                    InventoryTransferOperation::FurnaceInput.reply_tag(),
                    ApiCommand::FurnaceTransfer {
                        operation: InventoryTransferOperation::FurnaceInput,
                        pos,
                        item: input,
                        count: input_count,
                    },
                    API_INVENTORY_TRANSFER_TIMEOUT,
                );
                let input_reply = match input_reply {
                    Ok(reply) => reply,
                    Err(BotReplyError::Busy) => {
                        write_json_error(&mut stream, "409 Conflict", "request_already_in_progress");
                        return;
                    }
                    Err(BotReplyError::CommandChannelClosed) => {
                        write_json_error(&mut stream, "503 Service Unavailable", "bot_unavailable");
                        return;
                    }
                    Err(BotReplyError::Timeout) => {
                        write_json_error(&mut stream, "504 Gateway Timeout", "furnace_input_timeout");
                        return;
                    }
                };
                let input_value = parse_json_value(&input_reply).unwrap_or_else(|| {
                    json!({"ok": false, "status": "invalid_input_response", "raw": input_reply})
                });
                let input_ok = input_value
                    .get("ok")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
                let input_complete = input_ok
                    && input_value
                        .get("remaining")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0)
                        == 0;
                if !input_ok {
                    response = json!({
                        "ok": false,
                        "status": "input_failed",
                        "input": input_value,
                        "fuel": serde_json::Value::Null,
                    })
                    .to_string();
                    response_type = "application/json";
                } else {
                    let fuel_reply = send_and_wait_for_reply(
                        &tx,
                        &pending_replies,
                        InventoryTransferOperation::FurnaceFuel.reply_tag(),
                        ApiCommand::FurnaceTransfer {
                            operation: InventoryTransferOperation::FurnaceFuel,
                            pos,
                            item: fuel,
                            count: fuel_count,
                        },
                        API_INVENTORY_TRANSFER_TIMEOUT,
                    );
                    let fuel_value = match fuel_reply {
                        Ok(reply) => parse_json_value(&reply).unwrap_or_else(|| {
                            json!({"ok": false, "status": "invalid_fuel_response", "raw": reply})
                        }),
                        Err(BotReplyError::Busy) => {
                            json!({"ok": false, "status": "inventory_transfer_busy"})
                        }
                        Err(BotReplyError::CommandChannelClosed) => {
                            json!({"ok": false, "status": "bot_unavailable"})
                        }
                        Err(BotReplyError::Timeout) => {
                            json!({"ok": false, "status": "furnace_fuel_timeout"})
                        }
                    };
                    let fuel_ok = fuel_value
                        .get("ok")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true);
                    let fuel_complete = fuel_ok
                        && fuel_value
                            .get("remaining")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0)
                            == 0;
                    let completely_loaded = input_complete && fuel_complete;
                    response = json!({
                        "ok": completely_loaded,
                        "status": if completely_loaded { "furnace_loaded" } else { "partially_loaded" },
                        "input": input_value,
                        "fuel": fuel_value,
                    })
                    .to_string();
                    response_type = "application/json";
                }
            }
            ("POST", "/craft") => {
                let item = params
                    .get("item")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "item"));
                let Some(item) = item.filter(|item| valid_command_atom(item)) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_item");
                    return;
                };
                let Some(count) = query_or_json_bounded_u16(&params, &body, "count", 1, 64)
                else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_count");
                    return;
                };
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_CRAFT",
                    ApiCommand::Craft { item, count },
                    Duration::from_secs(20),
                    "craft_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/mine") => {
                let x = params.get("x").and_then(|v| v.parse::<i32>().ok());
                let y = params.get("y").and_then(|v| v.parse::<i32>().ok());
                let z = params.get("z").and_then(|v| v.parse::<i32>().ok());
                let pos = match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(IVec3 { x, y, z }),
                    _ => None,
                };
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_MINE",
                    ApiCommand::Mine(pos),
                    Duration::from_secs(65),
                    "mine_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/collect") => {
                let node = params
                    .get("node")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "node"));
                let Some(node) = node.filter(|node| !node.trim().is_empty()) else {
                    write_json_error(&mut stream, "400 Bad Request", "missing_node");
                    return;
                };
                let count = params
                    .get("count")
                    .and_then(|value| value.parse::<u16>().ok())
                    .unwrap_or(1)
                    .clamp(1, 8);
                let radius = params
                    .get("radius")
                    .and_then(|value| value.parse::<i32>().ok())
                    .unwrap_or(5)
                    .clamp(1, 6);
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_COLLECT",
                    ApiCommand::Collect {
                        node,
                        count,
                        radius,
                    },
                    Duration::from_secs(110),
                    "collect_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/navigate_node") => {
                let node = params
                    .get("node")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "node"));
                let Some(node) = node.filter(|value| valid_command_atom(value)) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_node");
                    return;
                };
                let radius = query_or_json_i32(&params, &body, "radius")
                    .unwrap_or(16)
                    .clamp(2, 32);
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_PATH_NODE",
                    ApiCommand::NavigateNode { node, radius },
                    Duration::from_secs(6),
                    "navigate_node_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/gather_resource") => {
                let node = params
                    .get("node")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "node"));
                let Some(node) = node.filter(|value| valid_command_atom(value)) else {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_node");
                    return;
                };
                let count = query_or_json_i32(&params, &body, "count")
                    .unwrap_or(1)
                    .clamp(1, 8) as u16;
                let radius = query_or_json_i32(&params, &body, "radius")
                    .unwrap_or(16)
                    .clamp(2, 32);
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_GATHER_PATH",
                    ApiCommand::GatherResource {
                        node,
                        count,
                        radius,
                    },
                    Duration::from_secs(6),
                    "gather_resource_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/hunt_food") => {
                let target = params
                    .get("target")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "target"))
                    .unwrap_or_else(|| "auto".to_string());
                if !valid_command_atom(&target) {
                    write_json_error(&mut stream, "400 Bad Request", "invalid_target");
                    return;
                }
                let radius = query_or_json_i32(&params, &body, "radius")
                    .unwrap_or(16)
                    .clamp(2, 32);
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_HUNT_PATH",
                    ApiCommand::HuntFood { target, radius },
                    Duration::from_secs(6),
                    "hunt_food_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/place") => {
                let x = params.get("x").and_then(|v| v.parse::<i32>().ok());
                let y = params.get("y").and_then(|v| v.parse::<i32>().ok());
                let z = params.get("z").and_then(|v| v.parse::<i32>().ok());
                let pos = match (x, y, z) {
                    (Some(x), Some(y), Some(z)) => Some(IVec3 { x, y, z }),
                    _ => None,
                };
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_PLACE",
                    ApiCommand::Place(pos),
                    Duration::from_secs(2),
                    "place_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
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
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_DROP",
                    ApiCommand::Drop { item, count },
                    Duration::from_secs(2),
                    "drop_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/wield") => {
                let item = params
                    .get("item")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "item"));
                let Some(item) = item else {
                    write_http_response(
                        &mut stream,
                        "400 Bad Request",
                        "application/json",
                        "{\"error\":\"missing_item\"}",
                    );
                    return;
                };
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_WIELD",
                    ApiCommand::Wield(item),
                    Duration::from_secs(2),
                    "wield_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
            }
            ("POST", "/use") => {
                let item = params
                    .get("item")
                    .cloned()
                    .or_else(|| parse_json_field(&body, "item"));
                let Some(body) = await_bot_reply(
                    &mut stream,
                    &tx,
                    &pending_replies,
                    "BOT_USE",
                    ApiCommand::Use(item),
                    Duration::from_secs(2),
                    "use_timeout",
                ) else {
                    return;
                };
                response = body;
                response_type = "application/json";
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
                    let clipped: String = trimmed.chars().take(256).collect();
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
            write_http_response(&mut stream, "200 OK", response_type, &response);
        } else {
            write_http_response(&mut stream, "400 Bad Request", "text/plain", "bad request");
        }
    }
}
