use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::api_client::{fetch_chat, fetch_observation, parse_http_url};
use super::candidates::{
    generate_candidates, ActionCandidate, ActionRisk, CandidateContext,
};
use super::chat::{advance_chat_cursor, apply_chat_goal_commands, sender_allowed};
use super::decision::{TokenUsage, ToolCall};
use super::jev::{JevClient, JevConfig};
use super::jev_policy::{JevPolicy, JevPolicyDecision};
use super::provider::{request_decision, LlmApi, ProviderConfig};
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
    pub controller: ControllerConfig,
    pub bot_name: String,
    pub allowed_senders: Vec<String>,
    pub passive: bool,
    pub autonomous: bool,
    pub observe_radius: i32,
    pub interval_ms: u64,
    pub idle_interval_ms: u64,
    pub initial_goal: Option<String>,
    pub state_file: Option<PathBuf>,
    pub max_tool_calls: usize,
    pub narrate_actions: bool,
    pub narration_cooldown_secs: u64,
}

#[derive(Clone, Debug)]
pub enum ControllerConfig {
    Llm(LlmControllerConfig),
    Jev(JevControllerConfig),
}

#[derive(Clone, Debug)]
pub struct LlmControllerConfig {
    pub url: String,
    pub api: LlmApi,
    pub api_key: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug)]
pub struct JevControllerConfig {
    pub url: String,
    pub api_key: String,
    pub model: String,
    pub minimum_confidence: f64,
    pub timeout_secs: u64,
    /// False runs the complete policy path but suppresses all game actions.
    pub execute: bool,
}

enum Controller {
    Llm {
        provider: ProviderConfig,
        tools: Vec<serde_json::Value>,
        instructions: String,
    },
    Jev {
        policy: JevPolicy,
        execute: bool,
    },
}

impl Controller {
    fn kind(&self) -> &'static str {
        match self {
            Self::Llm { .. } => "llm",
            Self::Jev { .. } => "jev",
        }
    }

    fn model(&self) -> &str {
        match self {
            Self::Llm { provider, .. } => &provider.model,
            Self::Jev { policy, .. } => policy.model(),
        }
    }

    fn execution_enabled(&self) -> bool {
        match self {
            Self::Llm { .. } => true,
            Self::Jev { execute, .. } => *execute,
        }
    }
}

struct ControllerDecision {
    calls: Vec<ToolCall>,
    text: Option<String>,
    usage: TokenUsage,
    incomplete_reason: Option<String>,
    context_bytes: usize,
    offered_actions: usize,
    returned_calls: usize,
    made_request: bool,
    jev: Option<JevPolicyDecision>,
    jev_candidate: Option<ActionCandidate>,
}

pub fn run_agent_loop(cfg: AgentConfig) -> Result<()> {
    let api_base = parse_http_url(&cfg.api_base).context("parse bot API base")?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("build HTTP client")?;
    let autonomous = cfg.autonomous && !cfg.passive;
    let controller = match &cfg.controller {
        ControllerConfig::Llm(llm) => Controller::Llm {
            provider: ProviderConfig {
                url: parse_http_url(&llm.url).context("parse LLM URL")?,
                api: llm.api,
                api_key: llm.api_key.clone(),
                model: llm.model.clone(),
                temperature: llm.temperature,
                max_tokens: llm.max_tokens,
                reasoning_effort: llm.reasoning_effort.clone(),
            },
            tools: tool_definitions(cfg.passive, autonomous),
            instructions: agent_instructions(cfg.passive, autonomous),
        },
        ControllerConfig::Jev(jev) => {
            let jev_config = JevConfig::new(jev.api_key.clone())
                .context("configure Jev")?
                .with_endpoint(parse_http_url(&jev.url).context("parse Jev URL")?)
                .context("configure Jev endpoint")?
                .with_model(jev.model.clone())
                .context("configure Jev model")?
                .with_timeout(Duration::from_secs(jev.timeout_secs))
                .context("configure Jev timeout")?;
            let policy = JevPolicy::new(
                JevClient::new(jev_config).context("build Jev client")?,
                jev.minimum_confidence,
            )?;
            Controller::Jev {
                policy,
                execute: jev.execute,
            }
        }
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

    let mut decision_telemetry = DecisionTelemetry::idle(state.tick);
    decision_telemetry.set_policy(controller.kind());
    let mut telemetry = TelemetryPublisher::new(
        &api_base,
        &cfg.api_token,
        controller.model(),
        &cfg.bot_name,
        cfg.passive,
        autonomous,
    );
    let mut unavailable_tools = HashSet::<String>::new();
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
        "agent started: policy={} model={} execution={} autonomous={} narrate={} cooldown={}s state_file={}",
        controller.kind(),
        controller.model(),
        controller.execution_enabled(),
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

        state.transition(AgentPhase::Planning, "requesting the next policy action");
        let continuing_player_instruction = chat_action_followup_due;
        let player_instruction_turn = is_player_instruction_turn(
            !state.pending_chat.is_empty(),
            continuing_player_instruction,
        );
        let decision_started = Instant::now();
        let request_context_bytes;
        let offered_actions;
        let decision_result: Result<ControllerDecision> = match &controller {
            Controller::Llm {
                provider,
                tools,
                instructions,
            } => {
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
                request_context_bytes = input.len();
                offered_actions = decision_tools.len();
                decision_telemetry = DecisionTelemetry::requesting(
                    state.tick,
                    request_context_bytes,
                    offered_actions,
                );
                decision_telemetry.set_policy(controller.kind());
                telemetry.publish(&state, &decision_telemetry);
                request_decision(&client, provider, instructions, &input, &decision_tools).map(
                    |decision| {
                        let returned_calls = decision.tool_calls.len();
                        ControllerDecision {
                            calls: decision.tool_calls,
                            text: decision.text,
                            usage: decision.usage,
                            incomplete_reason: decision.incomplete_reason,
                            context_bytes: request_context_bytes,
                            offered_actions,
                            returned_calls,
                            made_request: true,
                            jev: None,
                            jev_candidate: None,
                        }
                    },
                )
            }
            Controller::Jev { policy, .. } => {
                let candidates = generate_candidates(CandidateContext {
                    state: &state,
                    allowed_senders: &cfg.allowed_senders,
                    bot_name: &cfg.bot_name,
                    passive: cfg.passive,
                    autonomous,
                    player_instruction_turn,
                    unavailable_tools: &unavailable_tools,
                });
                request_context_bytes = super::jev_policy::compact_state(
                    &state,
                    &cfg.allowed_senders,
                )
                .to_string()
                .len();
                offered_actions = candidates.len();
                decision_telemetry = DecisionTelemetry::requesting(
                    state.tick,
                    request_context_bytes,
                    offered_actions,
                );
                decision_telemetry.set_policy(controller.kind());
                telemetry.publish(&state, &decision_telemetry);
                policy
                    .decide(&state, &cfg.allowed_senders, &candidates)
                    .and_then(|jev| {
                        let selected = jev
                            .selected(&candidates)
                            .context("selected Jev candidate disappeared")?;
                        anyhow::ensure!(
                            selected.generated_tick == state.tick,
                            "selected Jev candidate is stale"
                        );
                        let calls = selected.to_tool_call().into_iter().collect::<Vec<_>>();
                        let jev_candidate = selected.clone();
                        let returned_calls = usize::from(!matches!(
                            selected.action,
                            super::candidates::CandidateAction::Wait
                        ));
                        Ok(ControllerDecision {
                            calls,
                            text: None,
                            usage: jev.usage.clone(),
                            incomplete_reason: None,
                            context_bytes: jev.context_bytes,
                            offered_actions: candidates.len(),
                            returned_calls,
                            made_request: !jev.deterministic,
                            jev: Some(jev),
                            jev_candidate: Some(jev_candidate),
                        })
                    })
            }
        };
        let mut decision = match decision_result {
            Ok(decision) => decision,
            Err(error) => {
                let message = format!("{} policy request failed: {error:#}", controller.kind());
                state.record_action_result("plan", false, &message);
                decision_telemetry = DecisionTelemetry::failed(
                    state.tick,
                    request_context_bytes,
                    offered_actions,
                    decision_started.elapsed(),
                    &message,
                );
                decision_telemetry.set_policy(controller.kind());
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
        if decision.made_request {
            state.record_usage(&decision.usage);
        }
        println!(
            "agent decision: policy={} selected_calls={} candidates={} context_bytes={} input_tokens={} output_tokens={} incomplete={}",
            controller.kind(),
            decision.calls.len(),
            decision.offered_actions,
            decision.context_bytes,
            decision.usage.input,
            decision.usage.output,
            decision.incomplete_reason.as_deref().unwrap_or("no")
        );
        let mut calls = std::mem::take(&mut decision.calls);
        if state.pending_chat.is_empty() {
            let before = calls.len();
            calls.retain(|call| call.name.trim() != "say");
            if calls.len() != before {
                eprintln!("agent ignored say call because there is no new player chat");
            }
        }
        calls.retain(|call| !unavailable_tools.contains(call.name.trim()));
        if calls.is_empty() {
            if let Some(jev) = decision.jev.as_ref() {
                let message = jev
                    .fallback_reason
                    .as_deref()
                    .map(|reason| format!("Jev chose wait: {reason}"))
                    .unwrap_or_else(|| "Jev chose wait and will observe again".to_string());
                state.record_action_result("plan", true, &message);
            } else if let Some(reason) = decision.incomplete_reason.as_deref() {
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
        if controller.execution_enabled() {
            if let Some(candidate) = decision
                .jev_candidate
                .as_ref()
                .filter(|candidate| candidate.risk == ActionRisk::Material)
            {
                let preflight = refresh_and_validate_jev_candidate(
                    &client,
                    &api_base,
                    &cfg,
                    &mut state,
                    &unavailable_tools,
                    autonomous,
                    player_instruction_turn,
                    candidate,
                );
                let blocked_reason = match preflight {
                    Ok(true) => None,
                    Ok(false) => Some(format!(
                        "material candidate '{}' was no longer valid in the refreshed observation",
                        candidate.id
                    )),
                    Err(error) => Some(format!(
                        "could not refresh observation before material action '{}': {error:#}",
                        candidate.id
                    )),
                };
                if let Some(reason) = blocked_reason {
                    calls.retain(|call| !is_external_game_tool(&call.name));
                    apply_jev_wait_fallback(&mut decision, &reason);
                    state.record_action_result("plan", false, &reason);
                    eprintln!("Jev execution blocked: {reason}");
                }
            }
        }
        if let Some(jev) = decision.jev.as_ref() {
            println!(
                "jev decision: proposed={} proposed_probability={:.3} selected={} selected_probability={:.3} confidence={:.3} threshold={:.3} deterministic={} fallback={}",
                jev.proposed_id,
                jev.proposed_probability,
                jev.selected_id,
                jev.selected_probability,
                jev.confidence,
                jev.required_confidence,
                jev.deterministic,
                jev.fallback_reason.as_deref().unwrap_or("none"),
            );
        }
        if !state.pending_chat.is_empty()
            && controller.execution_enabled()
            && (decision.jev.is_some() || !calls.is_empty())
            && !calls.iter().any(|call| call.name.trim() == "say")
        {
            if let Some(chat) = state.pending_chat.front() {
                let message = if decision.jev.is_some() && calls.is_empty() {
                    format!(
                        "I can't safely act on that yet, {}. I'll keep observing.",
                        chat.from
                    )
                } else {
                    format!(
                        "Got it, {}. I'll factor that into what I'm doing.",
                        chat.from
                    )
                };
                calls.insert(
                    0,
                    ToolCall {
                        id: "chat-ack-fallback".to_string(),
                        name: "say".to_string(),
                        arguments: json!({"message": message}),
                    },
                );
            }
        }
        calls.sort_by_key(|call| is_external_game_tool(&call.name));
        let call_limit = match &controller {
            Controller::Jev { .. } => 2,
            Controller::Llm { .. } => cfg.max_tool_calls.clamp(1, 8),
        };
        let mut calls: Vec<ToolCall> = calls
            .into_iter()
            .take(call_limit)
            .collect();
        decision_telemetry = DecisionTelemetry::ready(
            state.tick,
            decision.context_bytes,
            decision.offered_actions,
            decision.returned_calls,
            &calls,
            decision_latency,
            &decision.usage,
            decision.incomplete_reason.as_deref(),
        );
        decision_telemetry.set_policy(controller.kind());
        if let (
            Some(jev),
            Controller::Jev {
                policy,
                execute,
            },
        ) = (decision.jev.as_ref(), &controller)
        {
            decision_telemetry.set_jev(jev, policy.minimum_confidence(), *execute);
        }
        telemetry.publish(&state, &decision_telemetry);
        if !controller.execution_enabled() {
            if let Some(jev) = decision.jev.as_ref() {
                let pending_messages = state.pending_chat.len();
                state.record_action_result(
                    "plan",
                    true,
                    &format!(
                        "Jev dry run selected '{}' with confidence {:.3}; suppressed {} action(s) and shadow-consumed {} pending message(s), which remain in conversation history",
                        jev.selected_id,
                        jev.confidence,
                        calls.len(),
                        pending_messages,
                    ),
                );
            }
            suppress_dry_run_actions(&mut state, &mut calls);
        }

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
            state.transition(AgentPhase::Idle, "policy chose no external action");
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

fn refresh_and_validate_jev_candidate(
    client: &Client,
    api_base: &reqwest::Url,
    cfg: &AgentConfig,
    state: &mut AgentState,
    unavailable_tools: &HashSet<String>,
    autonomous: bool,
    player_instruction_turn: bool,
    selected: &ActionCandidate,
) -> Result<bool> {
    let observation = fetch_observation(
        client,
        api_base,
        &cfg.api_token,
        cfg.observe_radius.clamp(1, 8),
    )
    .context("refresh bot observation for Jev preflight")?;
    state.update_observation(observation);
    let refreshed = generate_candidates(CandidateContext {
        state,
        allowed_senders: &cfg.allowed_senders,
        bot_name: &cfg.bot_name,
        passive: cfg.passive,
        autonomous,
        player_instruction_turn,
        unavailable_tools,
    });
    Ok(refreshed
        .iter()
        .any(|candidate| candidate.action == selected.action && candidate.risk == selected.risk))
}

fn apply_jev_wait_fallback(decision: &mut ControllerDecision, reason: &str) {
    let Some(jev) = decision.jev.as_mut() else {
        return;
    };
    jev.selected_id = "wait".to_string();
    jev.selected_probability = jev.probabilities.get("wait").copied().unwrap_or_default();
    jev.fallback_reason = Some(match jev.fallback_reason.take() {
        Some(existing) => format!("{existing}; {reason}"),
        None => reason.to_string(),
    });
}

fn suppress_dry_run_actions(state: &mut AgentState, calls: &mut Vec<ToolCall>) {
    // A shadow decision consumes the pending queue once so it cannot trigger a
    // paid evaluation on every tick. enqueue_chat already persisted the text
    // in conversation_history, so the trace remains available for inspection.
    state.clear_pending_chat();
    calls.clear();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_discards_actions_once_but_keeps_chat_history() {
        let mut state = AgentState::default();
        state.enqueue_chat([PendingChatMessage {
            id: 7,
            from: "Alice".to_string(),
            message: "mine coal".to_string(),
        }]);
        let mut calls = vec![ToolCall {
            id: "candidate".to_string(),
            name: "gather_resource".to_string(),
            arguments: json!({"node": "mcl_core:stone_with_coal"}),
        }];

        suppress_dry_run_actions(&mut state, &mut calls);

        assert!(calls.is_empty());
        assert!(state.pending_chat.is_empty());
        assert_eq!(state.conversation_history.len(), 1);
        assert_eq!(state.conversation_history[0].message, "mine coal");
    }

    #[test]
    fn preflight_fallback_reports_wait_probability() {
        let mut decision = ControllerDecision {
            calls: Vec::new(),
            text: None,
            usage: TokenUsage::default(),
            incomplete_reason: None,
            context_bytes: 0,
            offered_actions: 2,
            returned_calls: 1,
            made_request: true,
            jev: Some(JevPolicyDecision {
                proposed_id: "mine".to_string(),
                selected_id: "mine".to_string(),
                confidence: 0.8,
                required_confidence: 0.70,
                proposed_probability: 0.8,
                selected_probability: 0.8,
                probabilities: std::collections::BTreeMap::from([
                    ("mine".to_string(), 0.8),
                    ("wait".to_string(), 0.2),
                ]),
                usage: TokenUsage::default(),
                fallback_reason: None,
                deterministic: false,
                context_bytes: 0,
            }),
            jev_candidate: None,
        };

        apply_jev_wait_fallback(&mut decision, "stale observation");

        let jev = decision.jev.unwrap();
        assert_eq!(jev.selected_id, "wait");
        assert_eq!(jev.selected_probability, 0.2);
        assert_eq!(jev.fallback_reason.as_deref(), Some("stale observation"));
    }
}
