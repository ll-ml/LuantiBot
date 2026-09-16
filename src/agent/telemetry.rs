use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::{blocking::Client, Url};
use serde::Serialize;
use serde_json::{json, Value};

use super::provider::{TokenUsage, ToolCall};
use super::state::AgentState;
use super::util::clip_chars;

const TELEMETRY_HEARTBEAT: Duration = Duration::from_secs(2);
const RECENT_TOOL_LIMIT: usize = 12;

#[derive(Clone, Debug, Serialize)]
pub(super) struct DecisionTelemetry {
    status: String,
    tick: u64,
    context_bytes: usize,
    offered_tools: usize,
    returned_tool_calls: usize,
    selected_tools: Vec<ToolCallTelemetry>,
    request_latency_ms: Option<u64>,
    usage: Option<TokenUsage>,
    incomplete_reason: Option<String>,
    error: Option<String>,
    active_tool: Option<ActiveToolTelemetry>,
    last_execution: Option<ToolExecutionTelemetry>,
}

#[derive(Clone, Debug, Serialize)]
struct ToolCallTelemetry {
    name: String,
    arguments: Value,
}

#[derive(Clone, Debug, Serialize)]
struct ActiveToolTelemetry {
    index: usize,
    total: usize,
    name: String,
    arguments: Value,
}

#[derive(Clone, Debug, Serialize)]
struct ToolExecutionTelemetry {
    name: String,
    ok: bool,
    result: String,
    latency_ms: u64,
}

impl DecisionTelemetry {
    pub fn idle(tick: u64) -> Self {
        Self {
            status: "idle".to_string(),
            tick,
            context_bytes: 0,
            offered_tools: 0,
            returned_tool_calls: 0,
            selected_tools: Vec::new(),
            request_latency_ms: None,
            usage: None,
            incomplete_reason: None,
            error: None,
            active_tool: None,
            last_execution: None,
        }
    }

    pub fn requesting(tick: u64, context_bytes: usize, offered_tools: usize) -> Self {
        Self {
            status: "requesting".to_string(),
            tick,
            context_bytes,
            offered_tools,
            ..Self::idle(tick)
        }
    }

    pub fn ready(
        tick: u64,
        context_bytes: usize,
        offered_tools: usize,
        returned_tool_calls: usize,
        selected_tools: &[ToolCall],
        request_latency: Duration,
        usage: &TokenUsage,
        incomplete_reason: Option<&str>,
    ) -> Self {
        Self {
            status: if selected_tools.is_empty() {
                "completed".to_string()
            } else {
                "ready".to_string()
            },
            tick,
            context_bytes,
            offered_tools,
            returned_tool_calls,
            selected_tools: selected_tools.iter().map(ToolCallTelemetry::from).collect(),
            request_latency_ms: Some(duration_millis(request_latency)),
            usage: Some(usage.clone()),
            incomplete_reason: incomplete_reason.map(|reason| clip_chars(reason, 300)),
            error: None,
            active_tool: None,
            last_execution: None,
        }
    }

    pub fn failed(
        tick: u64,
        context_bytes: usize,
        offered_tools: usize,
        request_latency: Duration,
        error: &str,
    ) -> Self {
        Self {
            status: "failed".to_string(),
            tick,
            context_bytes,
            offered_tools,
            request_latency_ms: Some(duration_millis(request_latency)),
            error: Some(clip_chars(error, 500)),
            ..Self::idle(tick)
        }
    }

    pub fn start_tool(&mut self, call: &ToolCall, index: usize, total: usize) {
        self.status = "executing".to_string();
        self.active_tool = Some(ActiveToolTelemetry {
            index,
            total,
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
    }

    pub fn finish_tool(&mut self, name: &str, ok: bool, result: &str, latency: Duration) {
        self.status = "ready".to_string();
        self.active_tool = None;
        self.last_execution = Some(ToolExecutionTelemetry {
            name: name.to_string(),
            ok,
            result: clip_chars(result, 500),
            latency_ms: duration_millis(latency),
        });
    }

    pub fn finish(&mut self) {
        self.status = "completed".to_string();
        self.active_tool = None;
    }
}

impl From<&ToolCall> for ToolCallTelemetry {
    fn from(call: &ToolCall) -> Self {
        Self {
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        }
    }
}

struct PendingTelemetry {
    payload: Mutex<Option<String>>,
    ready: Condvar,
}

pub(super) struct TelemetryPublisher {
    pending: Option<Arc<PendingTelemetry>>,
    model: String,
    bot_name: String,
    passive: bool,
    autonomous: bool,
    last_queued_at: Option<Instant>,
}

impl TelemetryPublisher {
    pub fn new(
        api_base: &Url,
        api_token: &str,
        model: &str,
        bot_name: &str,
        passive: bool,
        autonomous: bool,
    ) -> Self {
        let worker = api_base
            .join("/agent/telemetry")
            .context("join telemetry API endpoint")
            .and_then(|endpoint| start_worker(endpoint, api_token.to_string()));
        let pending = match worker {
            Ok(pending) => Some(pending),
            Err(error) => {
                eprintln!("agent telemetry disabled: {error:#}");
                None
            }
        };
        Self {
            pending,
            model: model.to_string(),
            bot_name: bot_name.to_string(),
            passive,
            autonomous,
            last_queued_at: None,
        }
    }

    pub fn publish(&mut self, state: &AgentState, decision: &DecisionTelemetry) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let snapshot = build_snapshot(
            state,
            decision,
            &self.model,
            &self.bot_name,
            self.passive,
            self.autonomous,
        );
        let Ok(payload) = serde_json::to_string(&snapshot) else {
            return;
        };
        let mut slot = pending
            .payload
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let replaced = slot.replace(payload);
        drop(slot);
        drop(replaced);
        pending.ready.notify_one();
        self.last_queued_at = Some(Instant::now());
    }

    pub fn publish_if_due(
        &mut self,
        state: &AgentState,
        decision: &DecisionTelemetry,
        interval: Duration,
    ) {
        let due = self
            .last_queued_at
            .is_none_or(|last| last.elapsed() >= interval);
        if due {
            self.publish(state, decision);
        }
    }
}

fn start_worker(endpoint: Url, api_token: String) -> Result<Arc<PendingTelemetry>> {
    let client = Client::builder()
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(2))
        .pool_max_idle_per_host(1)
        .build()
        .context("build telemetry HTTP client")?;
    let pending = Arc::new(PendingTelemetry {
        payload: Mutex::new(None),
        ready: Condvar::new(),
    });
    let worker_pending = Arc::clone(&pending);
    thread::Builder::new()
        .name("agent-telemetry".to_string())
        .spawn(move || run_worker(worker_pending, client, endpoint, api_token))
        .context("spawn telemetry publisher")?;
    Ok(pending)
}

fn run_worker(
    pending: Arc<PendingTelemetry>,
    client: Client,
    endpoint: Url,
    api_token: String,
) {
    let mut last_payload: Option<String> = None;
    let mut warned = false;

    loop {
        let queued_payload = {
            let mut slot = pending
                .payload
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if slot.is_none() {
                let (waiting_slot, _) = pending
                    .ready
                    .wait_timeout(slot, TELEMETRY_HEARTBEAT)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                slot = waiting_slot;
            }
            slot.take()
        };
        let payload = queued_payload.or_else(|| last_payload.clone());
        let Some(payload) = payload else {
            continue;
        };
        last_payload = Some(payload.clone());

        match post_snapshot(&client, &endpoint, &api_token, payload) {
            Ok(()) => {
                if warned {
                    println!("agent telemetry connection restored");
                    warned = false;
                }
            }
            Err(error) => {
                if !warned {
                    eprintln!("agent telemetry unavailable: {error:#}");
                    warned = true;
                }
            }
        }
    }
}

fn post_snapshot(client: &Client, endpoint: &Url, api_token: &str, payload: String) -> Result<()> {
    let mut request = client
        .post(endpoint.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(payload);
    if !api_token.trim().is_empty() {
        request = request.bearer_auth(api_token.trim());
    }
    let response = request.send().context("POST /agent/telemetry")?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    anyhow::ensure!(status.is_success(), "bot API returned {status}: {body}");
    Ok(())
}

fn build_snapshot(
    state: &AgentState,
    decision: &DecisionTelemetry,
    model: &str,
    bot_name: &str,
    passive: bool,
    autonomous: bool,
) -> Value {
    let observation = &state.observation;
    let recent_tools: Vec<Value> = state
        .action_history
        .iter()
        .rev()
        .take(RECENT_TOOL_LIMIT)
        .map(|record| {
            json!({
                "tick": record.tick,
                "name": record.action,
                "arguments": record.arguments,
                "ok": record.ok,
                "result": record.result,
                "position": record.position,
            })
        })
        .collect();
    let visible_players: Vec<&str> = observation
        .players
        .iter()
        .filter_map(|player| player.name.as_deref())
        .take(8)
        .collect();

    json!({
        "schema_version": 1,
        "bot_name": bot_name,
        "model": model,
        "mode": {
            "autonomous": autonomous,
            "passive": passive,
        },
        "tick": state.tick,
        "phase": state.phase,
        "phase_reason": state.phase_reason,
        "mission": state.current_goal,
        "objective": state.current_objective,
        "last_action": state.last_action,
        "last_result": state.last_result,
        "consecutive_failures": state.consecutive_failures,
        "usage": state.usage,
        "world": {
            "position": observation.position,
            "facing": observation.facing,
            "health": observation.health,
            "hunger_available": observation.hunger_available,
            "hunger": observation.hunger,
            "saturation": observation.saturation,
            "visible_players": visible_players,
            "hostiles": observation.hostiles.len(),
            "mobs": observation.mobs.len(),
            "nearby_items": observation.nearby_items.len(),
            "nearby_nodes": observation.nearby_nodes.len(),
            "inventory_stacks": observation.inventory.main.len(),
            "follow_enabled": observation.controller.follow_enabled,
            "follow_target": observation.controller.follow_target,
            "move_active": observation.controller.move_active,
            "move_target": observation.controller.move_target,
            "navigation": observation.controller.navigation,
        },
        "decision": decision,
        "recent_tools": recent_tools,
    })
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_bounded_to_recent_activity() {
        let mut state = AgentState::default();
        for index in 0..20 {
            state.tick = index;
            state.record_tool_result("move", &json!({"steps": index}), true, "OK");
        }
        let snapshot = build_snapshot(
            &state,
            &DecisionTelemetry::idle(state.tick),
            "test-model",
            "Bot",
            false,
            true,
        );

        assert_eq!(snapshot["recent_tools"].as_array().unwrap().len(), 12);
        assert_eq!(snapshot["recent_tools"][0]["tick"], 19);
        assert_eq!(snapshot["model"], "test-model");
        assert_eq!(snapshot["mode"]["autonomous"], true);
    }

    #[test]
    fn decision_trace_exposes_calls_without_provider_credentials() {
        let calls = vec![ToolCall {
            id: "provider-call-id".to_string(),
            name: "move".to_string(),
            arguments: json!({"direction": "forward"}),
        }];
        let decision = DecisionTelemetry::ready(
            4,
            1200,
            8,
            1,
            &calls,
            Duration::from_millis(250),
            &TokenUsage {
                input: 10,
                cached_input: 2,
                output: 3,
                total: 13,
            },
            None,
        );
        let value = serde_json::to_value(decision).unwrap();

        assert_eq!(value["status"], "ready");
        assert_eq!(value["selected_tools"][0]["name"], "move");
        assert_eq!(value["request_latency_ms"], 250);
        assert!(!value.to_string().contains("provider-call-id"));
    }
}
