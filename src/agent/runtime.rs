use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::api_client::{fetch_chat, fetch_observation, parse_http_url};
use super::chat::{advance_chat_cursor, apply_chat_goal_commands, sender_allowed};
use super::provider::{request_decision, LlmApi, ProviderConfig, ToolCall};
use super::narration::action_narration;
use super::planner::progression_recommendations;
use super::policy::{
    idle_decision_due, is_external_game_tool, is_player_instruction_turn,
    navigation_blocks_planning, needs_chat_action_followup, tool_is_available, tool_is_relevant,
    tool_settle_delay,
};
use super::prompt::agent_instructions;
use super::state::{AgentPhase, AgentState, PendingChatMessage};
use super::telemetry::{DecisionTelemetry, TelemetryPublisher};
use super::tools::{execute_tool, post_chat_status, tool_definitions};
use super::util::clip_chars;

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub api_base: String,
    pub api_token: String,
    pub llm_url: String,
    pub llm_api: LlmApi,
    pub llm_api_key: String,
    pub model: String,
    pub bot_name: String,
    pub allowed_senders: Vec<String>,
    pub passive: bool,
    pub autonomous: bool,
    pub observe_radius: i32,
    pub interval_ms: u64,
    pub idle_interval_ms: u64,
    pub temperature: f32,
    pub max_tokens: u32,
    pub reasoning_effort: Option<String>,
    pub initial_goal: Option<String>,
    pub state_file: Option<PathBuf>,
    pub max_tool_calls: usize,
    pub narrate_actions: bool,
    pub narration_cooldown_secs: u64,
}

pub fn run_agent_loop(cfg: AgentConfig) -> Result<()> {
    let api_base = parse_http_url(&cfg.api_base).context("parse bot API base")?;
    let llm_url = parse_http_url(&cfg.llm_url).context("parse LLM URL")?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("build HTTP client")?;
    let provider = ProviderConfig {
        url: llm_url,
        api: cfg.llm_api,
        api_key: cfg.llm_api_key.clone(),
        model: cfg.model.clone(),
        temperature: cfg.temperature,
        max_tokens: cfg.max_tokens,
        reasoning_effort: cfg.reasoning_effort.clone(),
    };
    let mut state = load_state(cfg.state_file.as_deref())?;
    if state.current_goal.is_none() {
        if let Some(goal) = cfg
            .initial_goal
            .as_deref()
            .map(str::trim)
            .filter(|goal| !goal.is_empty())
        {
            state.set_configured_goal(goal, None)?;
        }
    }

    let autonomous = cfg.autonomous && !cfg.passive;
    let mut decision_telemetry = DecisionTelemetry::idle(state.tick);
    let mut telemetry = TelemetryPublisher::new(
        &api_base,
        &cfg.api_token,
        &cfg.model,
        &cfg.bot_name,
        cfg.passive,
        autonomous,
    );
    let tools = tool_definitions(cfg.passive, autonomous);
    let mut unavailable_tools = HashSet::<String>::new();
    let instructions = agent_instructions(cfg.passive, autonomous);
    let tick_interval = Duration::from_millis(cfg.interval_ms.max(250));
    let idle_interval = Duration::from_millis(cfg.idle_interval_ms.max(cfg.interval_ms.max(250)));
    let mut next_idle_decision = Instant::now();
    let mut next_goal_decision = Instant::now();
    let mut next_chat_decision = Instant::now();
    let mut next_threat_decision = Instant::now();
    let mut chat_action_followup_due = false;
    let mut reported_mod_schema: Option<u32> = None;
    let narration_cooldown = Duration::from_secs(cfg.narration_cooldown_secs.max(1));
    let mut next_narration = Instant::now();
    let mut last_narration: Option<(String, Instant)> = None;

    println!(
        "agent started: model={} api={:?} tools={} autonomous={} narrate={} cooldown={}s state_file={}",
        cfg.model,
        cfg.llm_api,
        tools.len(),
        autonomous,
        cfg.narrate_actions,
        narration_cooldown.as_secs(),
        cfg.state_file
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "disabled".to_string())
    );
    telemetry.publish(&state, &decision_telemetry);

    loop {
        let started = Instant::now();
        state.tick = state.tick.saturating_add(1);
        state.transition(AgentPhase::Observing, "polling bot state");
        telemetry.publish_if_due(
            &state,
            &decision_telemetry,
            Duration::from_secs(5),
        );

        let (chats, chat_cursor) = match fetch_chat(
            &client,
            &api_base,
            &cfg.api_token,
            state.last_seen_chat_id,
        ) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("agent chat poll failed: {error:#}");
                (Vec::new(), state.last_seen_chat_id)
            }
        };
        if chat_cursor < state.last_seen_chat_id {
            println!(
                "agent chat cursor reset: server_last={} saved_last={}",
                chat_cursor, state.last_seen_chat_id
            );
        }
        state.last_seen_chat_id = advance_chat_cursor(state.last_seen_chat_id, chat_cursor);
        let chats: Vec<PendingChatMessage> = chats
            .into_iter()
            .filter(|chat| {
                (cfg.bot_name.trim().is_empty()
                    || !chat.from.eq_ignore_ascii_case(cfg.bot_name.trim()))
                    && sender_allowed(&cfg.allowed_senders, &chat.from)
            })
            .collect();
        let received_new_chat = !chats.is_empty();
        apply_chat_goal_commands(&mut state, &chats);
        state.enqueue_chat(chats);

        match fetch_observation(
            &client,
            &api_base,
            &cfg.api_token,
            cfg.observe_radius.clamp(1, 8),
        ) {
            Ok(observation) => {
                if reported_mod_schema != Some(observation.server_mod_schema_version) {
                    if observation.server_mod_schema_version < 2 {
                        eprintln!(
                            "agent detected old or missing llm_bot schema {}; collect tools are disabled until the world mod is updated",
                            observation.server_mod_schema_version
                        );
                    } else if observation.server_mod_schema_version < 6 {
                        eprintln!(
                            "agent detected llm_bot schema {}; native mine, collect_blocks, and gather_resource are disabled until the world mod is updated",
                            observation.server_mod_schema_version
                        );
                    } else if observation.server_mod_schema_version < 7 {
                        eprintln!(
                            "agent detected llm_bot schema {}; chest deposit and withdrawal are disabled until the world mod is updated",
                            observation.server_mod_schema_version
                        );
                    } else if observation.server_mod_schema_version < 8 {
                        eprintln!(
                            "agent detected llm_bot schema {}; furnace and crafting tools are disabled until the world mod is updated",
                            observation.server_mod_schema_version
                        );
                    } else {
                        println!(
                            "agent detected llm_bot schema {}",
                            observation.server_mod_schema_version
                        );
                    }
                    reported_mod_schema = Some(observation.server_mod_schema_version);
                }
                state.update_observation(observation)
            }
            Err(error) => {
                let message = format!("observation failed: {error:#}");
                state.record_action_result("observe", false, &message);
                eprintln!("{message}");
                telemetry.publish(&state, &decision_telemetry);
                save_state(&state, cfg.state_file.as_deref());
                sleep_remaining(started, tick_interval);
                continue;
            }
        }

        let now = Instant::now();
        let has_nearby_threat = !cfg.passive && !state.observation.hostiles.is_empty();
        let has_active_work =
            state.current_goal.is_some() || state.current_objective.is_some();
        let navigation_busy =
            navigation_blocks_planning(&state.observation.controller.navigation.status);
        let should_decide = received_new_chat
            || (!state.pending_chat.is_empty() && now >= next_chat_decision)
            || (chat_action_followup_due && now >= next_chat_decision)
            || (has_nearby_threat && now >= next_threat_decision)
            || state.tick == 1
            || (!navigation_busy && has_active_work && now >= next_goal_decision)
            || (!navigation_busy
                && idle_decision_due(
                    autonomous,
                    state.current_objective.is_some(),
                    now,
                    next_idle_decision,
                ));
        if !should_decide {
            state.transition(AgentPhase::Idle, "waiting for a goal or new chat");
            telemetry.publish(&state, &decision_telemetry);
            save_state(&state, cfg.state_file.as_deref());
            sleep_remaining(started, tick_interval);
            continue;
        }

        state.transition(AgentPhase::Planning, "requesting the next tool action");
        let continuing_player_instruction = chat_action_followup_due;
        let player_instruction_turn = is_player_instruction_turn(
            !state.pending_chat.is_empty(),
            continuing_player_instruction,
        );
        let mut prompt_state = state.prompt_view();
        let recommendations = progression_recommendations(&state.observation);
        if !recommendations.is_empty() {
            if let Some(view) = prompt_state.as_object_mut() {
                view.insert(
                    "progression_recommendations".to_string(),
                    serde_json::to_value(recommendations).unwrap_or_else(|_| json!([])),
                );
            }
        }
        let mut input_view = json!({"state": prompt_state});
        if let Some(context) = input_view.as_object_mut() {
            if !cfg.allowed_senders.is_empty() {
                context.insert(
                    "authorized_players".to_string(),
                    json!(&cfg.allowed_senders),
                );
            }
            if continuing_player_instruction {
                context.insert("continue_after_reply".to_string(), json!(true));
            }
        }
        let input = input_view.to_string();

        let decision_tools: Vec<serde_json::Value> = tools
            .iter()
            .filter(|tool| {
                let name = tool
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                tool_is_available(
                    name,
                    !state.pending_chat.is_empty(),
                    state.observation.server_mod_schema_version,
                    &unavailable_tools,
                ) && tool_is_relevant(name, &state, player_instruction_turn)
            })
            .cloned()
            .collect();

        decision_telemetry = DecisionTelemetry::requesting(
            state.tick,
            input.len(),
            decision_tools.len(),
        );
        telemetry.publish(&state, &decision_telemetry);
        let decision_started = Instant::now();
        let decision = match request_decision(
            &client,
            &provider,
            &instructions,
            &input,
            &decision_tools,
        ) {
            Ok(decision) => decision,
            Err(error) => {
                let message = format!("LLM request failed: {error:#}");
                state.record_action_result("plan", false, &message);
                decision_telemetry = DecisionTelemetry::failed(
                    state.tick,
                    input.len(),
                    decision_tools.len(),
                    decision_started.elapsed(),
                    &message,
                );
                eprintln!("{message}");
                telemetry.publish(&state, &decision_telemetry);
                save_state(&state, cfg.state_file.as_deref());
                next_idle_decision = Instant::now() + idle_interval;
                next_goal_decision = Instant::now() + idle_interval;
                next_chat_decision = Instant::now() + idle_interval;
                next_threat_decision = Instant::now() + Duration::from_secs(2);
                sleep_remaining(started, tick_interval);
                continue;
            }
        };
        let decision_latency = decision_started.elapsed();
        let returned_tool_calls = decision.tool_calls.len();
        state.record_usage(&decision.usage);
        println!(
            "agent decision: tool_calls={} tools={} context_bytes={} input_tokens={} cached_input_tokens={} output_tokens={} incomplete={}",
            decision.tool_calls.len(),
            decision_tools.len(),
            input.len(),
            decision.usage.input,
            decision.usage.cached_input,
            decision.usage.output,
            decision.incomplete_reason.as_deref().unwrap_or("no")
        );

        let mut calls = decision.tool_calls;
        if state.pending_chat.is_empty() {
            let before = calls.len();
            calls.retain(|call| call.name.trim() != "say");
            if calls.len() != before {
                eprintln!("agent ignored say call because there is no new player chat");
            }
        }
        calls.retain(|call| !unavailable_tools.contains(call.name.trim()));
        if calls.is_empty() {
            if let Some(reason) = decision.incomplete_reason.as_deref() {
                let message = format!(
                    "model output was incomplete ({reason}) before a tool call; increase --max-tokens (try 768 or 1024) or reduce reasoning effort"
                );
                state.record_action_result("plan", false, &message);
                eprintln!("agent decision incomplete: {message}");
            } else if let Some(text) = decision.text.as_deref() {
                if !state.pending_chat.is_empty() {
                    calls.push(ToolCall {
                        id: "text-fallback".to_string(),
                        name: "say".to_string(),
                        arguments: json!({"message": clip_chars(text.trim(), 300)}),
                    });
                } else {
                    state.record_action_result("plan", true, &clip_chars(text.trim(), 500));
                }
            } else {
                state.record_action_result("plan", false, "model returned no tool call or text");
            }
        }
        if !state.pending_chat.is_empty()
            && !calls.is_empty()
            && !calls.iter().any(|call| call.name.trim() == "say")
        {
            if let Some(chat) = state.pending_chat.front() {
                calls.insert(
                    0,
                    ToolCall {
                        id: "chat-ack-fallback".to_string(),
                        name: "say".to_string(),
                        arguments: json!({
                            "message": format!(
                                "Got it, {}. I'll factor that into what I'm doing.",
                                chat.from
                            )
                        }),
                    },
                );
            }
        }
        calls.sort_by_key(|call| is_external_game_tool(&call.name));
        let calls: Vec<ToolCall> = calls
            .into_iter()
            .take(cfg.max_tool_calls.clamp(1, 8))
            .collect();
        decision_telemetry = DecisionTelemetry::ready(
            state.tick,
            input.len(),
            decision_tools.len(),
            returned_tool_calls,
            &calls,
            decision_latency,
            &decision.usage,
            decision.incomplete_reason.as_deref(),
        );
        telemetry.publish(&state, &decision_telemetry);

        let mut external_action_taken = false;
        let mut external_action_succeeded = false;
        let mut plan_changed = false;
        let mut action_settle_delay = idle_interval;
        let mut spoke_this_decision = false;
        let selected_call_count = calls.len();
        for (call_index, call) in calls.into_iter().enumerate() {
            if external_action_taken {
                break;
            }
            decision_telemetry.start_tool(&call, call_index + 1, selected_call_count);
            telemetry.publish(&state, &decision_telemetry);
            let tool_started = Instant::now();
            let execution = execute_tool(
                &client,
                &api_base,
                &cfg.api_token,
                &mut state,
                &call,
                cfg.passive,
                autonomous,
                player_instruction_turn,
            );
            println!(
                "agent tool: name={} ok={} result={}",
                execution.name, execution.ok, execution.result
            );
            state.record_tool_result(
                &execution.name,
                &call.arguments,
                execution.ok,
                &execution.result,
            );
            decision_telemetry.finish_tool(
                &execution.name,
                execution.ok,
                &execution.result,
                tool_started.elapsed(),
            );
            telemetry.publish(&state, &decision_telemetry);
            if !execution.ok && execution.result.contains("unsupported_command") {
                eprintln!(
                    "agent disabled unsupported tool '{}' until restart",
                    execution.name
                );
                unavailable_tools.insert(execution.name.clone());
            }
            if execution.ok && execution.name == "say" {
                if let Some(message) = call
                    .arguments
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                {
                    state.record_bot_chat(&cfg.bot_name, message, "assistant");
                }
                state.clear_pending_chat();
                spoke_this_decision = true;
                next_narration = Instant::now() + narration_cooldown;
            }
            if execution.ok
                && matches!(
                    execution.name.as_str(),
                    "set_goal" | "finish_goal" | "set_objective" | "finish_objective"
                )
            {
                plan_changed = true;
            }
            if cfg.narrate_actions
                && execution.ok
                && execution.external_action
                && !spoke_this_decision
            {
                if let Some(message) = action_narration(&call) {
                    let now = Instant::now();
                    let repeat_after = narration_cooldown.saturating_mul(3);
                    let repeated_too_soon = last_narration.as_ref().is_some_and(|(previous, at)| {
                        previous == &message && now.duration_since(*at) < repeat_after
                    });
                    if now >= next_narration && !repeated_too_soon {
                        match post_chat_status(&client, &api_base, &cfg.api_token, &message) {
                            Ok(_) => {
                                println!("agent narration: {message}");
                                state.record_bot_chat(&cfg.bot_name, &message, "status");
                                last_narration = Some((message, now));
                                next_narration = now + narration_cooldown;
                            }
                            Err(error) => eprintln!("agent narration failed: {error:#}"),
                        }
                    }
                }
            }
            if execution.external_action {
                action_settle_delay = tool_settle_delay(&execution.name, execution.ok);
                external_action_succeeded |= execution.ok;
            }
            external_action_taken |= execution.external_action;
        }
        decision_telemetry.finish();

        chat_action_followup_due = needs_chat_action_followup(
            player_instruction_turn,
            spoke_this_decision,
            external_action_succeeded,
        );

        if !external_action_taken && state.phase == AgentPhase::Planning {
            state.transition(AgentPhase::Idle, "model chose no external action");
        }
        next_goal_decision = Instant::now()
            + if external_action_taken {
                action_settle_delay
            } else if plan_changed
                && (state.current_goal.is_some() || state.current_objective.is_some())
            {
                tick_interval
            } else if spoke_this_decision
                && (state.current_goal.is_some() || state.current_objective.is_some())
            {
                tick_interval
            } else {
                idle_interval
            };
        next_idle_decision = Instant::now() + idle_interval;
        next_chat_decision = Instant::now()
            + if state.pending_chat.is_empty() {
                tick_interval
            } else {
                idle_interval
            };
        next_threat_decision = Instant::now()
            + if has_nearby_threat {
                if external_action_taken {
                    action_settle_delay.max(Duration::from_secs(2))
                } else {
                    Duration::from_secs(2)
                }
            } else {
                idle_interval
            };
        telemetry.publish(&state, &decision_telemetry);
        save_state(&state, cfg.state_file.as_deref());
        sleep_remaining(started, tick_interval);
    }
}

fn load_state(path: Option<&Path>) -> Result<AgentState> {
    match path {
        Some(path) if path.exists() => AgentState::load(path),
        _ => Ok(AgentState::default()),
    }
}

fn save_state(state: &AgentState, path: Option<&Path>) {
    if let Some(path) = path {
        if let Err(error) = state.save(path) {
            eprintln!("save agent state failed: {error:#}");
        }
    }
}

fn sleep_remaining(started: Instant, interval: Duration) {
    let elapsed = started.elapsed();
    if elapsed < interval {
        sleep(interval - elapsed);
    }
}
