use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::thread::sleep;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub api_base: String,
    pub api_token: String,
    pub llm_url: String,
    pub model: String,
    pub bot_name: String,
    pub passive: bool,
    pub observe_radius: i32,
    pub interval_ms: u64,
    pub temperature: f32,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentAction {
    pub action: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub steps: Option<f32>,
}

impl Default for AgentAction {
    fn default() -> Self {
        Self {
            action: "stop".to_string(),
            target: None,
            message: None,
            direction: None,
            steps: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct ObservationView {
    position: [i32; 3],
    facing: String,
    obstacles: ObstaclesView,
    players: Vec<PlayerView>,
    nearby_nodes: Vec<NodeView>,
    goal: Option<String>,
    chat: Vec<ChatMessageView>,
}

#[derive(Clone, Debug, Serialize)]
struct ObstaclesView {
    front: String,
    left: String,
    right: String,
    back: String,
}

#[derive(Clone, Debug, Serialize)]
struct PlayerView {
    name: String,
    dx: i32,
    dy: i32,
    dz: i32,
}

#[derive(Clone, Debug, Serialize)]
struct NodeView {
    name: String,
    dx: i32,
    dy: i32,
    dz: i32,
}

#[derive(Clone, Debug, Serialize)]
struct ChatMessageView {
    id: u64,
    from: String,
    msg: String,
}

pub fn run_agent_loop(cfg: AgentConfig) -> Result<()> {
    let api_base = Url::parse(&cfg.api_base).context("parse api base")?;
    let llm_url = Url::parse(&cfg.llm_url).context("parse llm url")?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("build http client")?;

    let mut last_action = String::from("stop");
    let mut last_announced_action = String::from("stop");
    let mut last_seen_chat_id = 0u64;
    let mut pending_chats: VecDeque<ChatMessageView> = VecDeque::new();
    let mut recent_say_messages: VecDeque<String> = VecDeque::new();
    let mut chat_cooldown_until: Option<Instant> = None;
    let narration_interval = Duration::from_secs(5);
    let mut next_narration_at = Instant::now();
    let chat_poll_interval = Duration::from_secs(2);
    let mut next_chat_poll = Instant::now();
    let tick = Duration::from_millis(cfg.interval_ms.max(50));
    let observe_interval = Duration::from_secs(15);
    let mut next_observe = Instant::now();
    let mut last_obs_view_json = String::new();
    let mut last_obs_raw_json = String::new();
    let mut last_obs_view: Option<ObservationView> = None;
    let mut force_observe = true;
    let mut pending_observe = true;
    let mut next_tick = Instant::now();

    loop {
        let now = Instant::now();
        let mut chat_entries = Vec::new();
        if now >= next_chat_poll {
            next_chat_poll = now + chat_poll_interval;
            let chat_raw = http_get(
                &client,
                &api_base,
                &format!("/chat?since={}&limit=500", last_seen_chat_id),
                &cfg.api_token,
            )
            .context("chat")?;
            println!("chat_raw: {}", chat_raw);
            if detect_chat_rate_limit(&chat_raw) {
                let until = Instant::now() + Duration::from_secs(12);
                chat_cooldown_until =
                    Some(chat_cooldown_until.map_or(until, |prev| prev.max(until)));
                println!("chat rate limit detected; pausing replies for 12s");
            }
            let (entries, _chat_last_id) = parse_chat_log(&chat_raw).context("parse chat")?;
            chat_entries = entries;
        }
        let bot_name = cfg.bot_name.trim();
        for entry in &chat_entries {
            if entry.id > last_seen_chat_id {
                if !bot_name.is_empty() && entry.from.eq_ignore_ascii_case(bot_name) {
                    continue;
                }
                if recent_say_messages.iter().any(|msg| msg == &entry.msg) {
                    continue;
                }
                if !pending_chats.iter().any(|pending| pending.id == entry.id) {
                    pending_chats.push_back(entry.clone());
                }
            }
        }
        println!(
            "chat_entries={} pending_chats={}",
            chat_entries.len(),
            pending_chats.len()
        );
        if let Some(max_id) = chat_entries.iter().map(|entry| entry.id).max() {
            if max_id > last_seen_chat_id {
                last_seen_chat_id = max_id;
            }
        }

        let now = Instant::now();
        if (force_observe || pending_observe) && now >= next_observe {
            let obs_raw = match http_get(
                &client,
                &api_base,
                &format!("/observe_server?radius={}", cfg.observe_radius),
                &cfg.api_token,
            ) {
                Ok(body) => body,
                Err(err) => {
                    println!("observe_server failed, falling back to /observe: {}", err);
                    http_get(
                        &client,
                        &api_base,
                        &format!("/observe?radius={}", cfg.observe_radius),
                        &cfg.api_token,
                    )
                    .context("observe")?
                }
            };
            println!("raw_obs: {}", obs_raw);
            last_obs_raw_json = obs_raw.clone();
            let obs_view =
                build_observation_view(&obs_raw, chat_entries).context("build observation view")?;
            last_obs_view_json =
                serde_json::to_string(&obs_view).context("serialize observation")?;
            last_obs_view = Some(obs_view);
            println!("obs_view: {}", last_obs_view_json);
            next_observe = now + observe_interval;
            force_observe = false;
            pending_observe = false;
        } else if let Some(obs_view) = &mut last_obs_view {
            obs_view.chat = chat_entries;
            last_obs_view_json =
                serde_json::to_string(obs_view).context("serialize observation")?;
        }
        let obs_view_json = last_obs_view_json.clone();
        let obs_raw_json = if last_obs_raw_json.is_empty() {
            obs_view_json.clone()
        } else {
            last_obs_raw_json.clone()
        };
        let Some(obs_view) = last_obs_view.clone() else {
            continue;
        };

        let now = Instant::now();
        if let Some(until) = chat_cooldown_until {
            if now >= until {
                chat_cooldown_until = None;
            }
        }
        let chat_cooldown_active = chat_cooldown_until
            .map(|until| now < until)
            .unwrap_or(false);
        if let Some(chat_item) = pending_chats.front().cloned() {
            if chat_cooldown_active {
                println!("chat cooldown active; deferring reply");
                continue;
            }
            let action = resolve_chat_action(
                &client,
                &llm_url,
                &cfg,
                &obs_view_json,
                &last_action,
                &chat_item,
            )
            .context("resolve chat action")?;
            println!("chat_action: {:?}", action);
            if action.action == "say" {
                if let Some(message) = action.message.as_deref() {
                    if !message.is_empty() {
                        recent_say_messages.push_back(message.to_string());
                        if recent_say_messages.len() > 10 {
                            recent_say_messages.pop_front();
                        }
                    }
                }
                if let Err(err) = execute_action(&client, &api_base, &cfg.api_token, &action) {
                    println!("agent chat action failed: {}", err);
                } else {
                    last_action = action.action.clone();
                    last_seen_chat_id = last_seen_chat_id.max(chat_item.id);
                    pending_chats.pop_front();
                    pending_observe = true;
                }
            } else {
                println!("chat action not say; retrying next tick");
            }
        } else {
            let prompt = if cfg.passive {
                build_passive_prompt(&obs_raw_json, &last_action)
            } else {
                build_prompt(&obs_view_json, &last_action)
            };
            let reply = call_llm(&client, &llm_url, &cfg, &prompt).context("llm")?;
            println!("llm_reply: {}", reply);

            let parsed = parse_action(&reply).unwrap_or_default();
            println!("action_parsed: {:?}", parsed);
            let action = if cfg.passive {
                let candidate = validate_say_with_limit(parsed, 300);
                if candidate.action == "say" {
                    candidate
                } else if let Some(message) = sanitize_llm_message(&reply) {
                    validate_say_with_limit(
                        AgentAction {
                            action: "say".to_string(),
                            target: None,
                            message: Some(message),
                            direction: None,
                            steps: None,
                        },
                        300,
                    )
                } else {
                    AgentAction::default()
                }
            } else {
                validate_action(parsed, &obs_view)
            };
            println!("action_validated: {:?}", action);

            if cfg.passive {
                if action.action == "say" {
                    if let Err(err) = execute_action(&client, &api_base, &cfg.api_token, &action) {
                        println!("agent action failed: {}", err);
                    } else {
                        last_action = action.action.clone();
                        if let Some(message) = action.message.as_deref() {
                            if !message.is_empty() {
                                recent_say_messages.push_back(message.to_string());
                                if recent_say_messages.len() > 10 {
                                    recent_say_messages.pop_front();
                                }
                            }
                        }
                    }
                }
            } else if let Err(err) = execute_action(&client, &api_base, &cfg.api_token, &action) {
                println!("agent action failed: {}", err);
            } else {
                last_action = action.action.clone();
                if action.action != "stop" {
                    pending_observe = true;
                }
                if action.action != "stop" && action.action != "say" {
                    let announce = format_action_message(&action);
                    if !announce.is_empty() {
                        last_announced_action = announce;
                    }
                }
                if action.action != "stop" && action.action != "say" {
                    let now = Instant::now();
                    if now >= next_narration_at {
                        if let Err(err) = maybe_post_narration(
                            &client,
                            &api_base,
                            &llm_url,
                            &cfg,
                            &obs_view_json,
                            &action,
                            chat_cooldown_until,
                            &mut recent_say_messages,
                        ) {
                            println!("narration failed: {}", err);
                        } else {
                            next_narration_at = now + narration_interval;
                        }
                    }
                }
            }
        }

        next_tick += tick;
        let now = Instant::now();
        if next_tick > now {
            sleep(next_tick - now);
        } else {
            next_tick = now;
        }
    }
}

fn build_prompt(observation_json: &str, last_action: &str) -> String {
    let rules = r#"You control a Luanti bot.
Available actions (JSON only):
{"action":"stop"}
{"action":"move","direction":"forward","steps":1}
{"action":"move","direction":"left","steps":2}
{"action":"follow","target":"<player>"}
{"action":"attack","target":"<player>"}
{"action":"say","message":"<short message>"}

Rules:
- Output exactly one JSON object.
- Do not include extra text.
- Avoid spamming {"action":"stop"}; prefer moving when safe.
- direction must be one of: forward, backward, left, right.
- steps must be an integer 1-3.
- target must match a name from observation.players.
- Obstacles may be ignored; moving even if marked "solid" is allowed.
- You can use say to respond briefly to recent chat or describe your next action.
- If no players are visible, roam by moving in any safe direction.
- If observation.chat has a player question, answer with {"action":"say","message":"..."}.
"#;

    format!(
        "{}\nLastAction: {}\nObservation: {}",
        rules, last_action, observation_json
    )
}

fn build_passive_prompt(observation_json: &str, last_action: &str) -> String {
    let rules = r#"You control a Luanti bot in passive mode.
Think deeply about what you should say.
Summarize what you see and what you would like to do next.

Only allowed action (JSON only):
{"action":"say","message":"<short summary>"}

Rules:
- Output exactly one JSON object.
- Do not include extra text.
- Use first person (I/me).
- You may use up to 300 characters.
"#;

    format!(
        "{}\nLastAction: {}\nObservation: {}",
        rules, last_action, observation_json
    )
}

fn build_chat_prompt(observation_json: &str, last_action: &str, chat: &ChatMessageView) -> String {
    let rules = r#"You control a Luanti bot.
You must respond to the chat message with a say action.

Only allowed action (JSON only):
{"action":"say","message":"<short reply>"}

Rules:
- Output exactly one JSON object.
- Do not include extra text.
- Keep the reply under 120 characters.
- Use first person (I/me) and present tense.
"#;

    let from = serde_json::to_string(&chat.from).unwrap_or_else(|_| "\"\"".to_string());
    let msg = serde_json::to_string(&chat.msg).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "{}\nLastAction: {}\nObservation: {}\nIncomingChat: {{\"from\":{},\"msg\":{}}}",
        rules, last_action, observation_json, from, msg
    )
}

fn build_chat_retry_prompt(
    observation_json: &str,
    last_action: &str,
    chat: &ChatMessageView,
) -> String {
    let rules = r#"You must output a single JSON object that is a say action.
Only allowed output:
{"action":"say","message":"<short reply>"}

Rules:
- Output exactly one JSON object.
- No extra text.
- Do not repeat what the user said.
- Keep the reply under 120 characters.
"#;

    let from = serde_json::to_string(&chat.from).unwrap_or_else(|_| "\"\"".to_string());
    let msg = serde_json::to_string(&chat.msg).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "{}\nLastAction: {}\nObservation: {}\nIncomingChat: {{\"from\":{},\"msg\":{}}}",
        rules, last_action, observation_json, from, msg
    )
}

fn build_narration_prompt(observation_json: &str, action: &AgentAction) -> String {
    let action_json = serde_json::to_string(action).unwrap_or_else(|_| "{}".to_string());
    let rules = r#"You control a Luanti bot.
Write a single short chat message describing what you are doing and what you see.

Rules:
- Output only the message text, no JSON.
- Keep it under 120 characters.
- Use first person (I/me) and present tense.
- Do not repeat player chat.
"#;
    format!(
        "{}\nObservation: {}\nAction: {}",
        rules, observation_json, action_json
    )
}

fn resolve_chat_action(
    client: &Client,
    llm_url: &Url,
    cfg: &AgentConfig,
    observation_json: &str,
    last_action: &str,
    chat: &ChatMessageView,
) -> Result<AgentAction> {
    let prompt = build_chat_prompt(observation_json, last_action, chat);
    let reply = call_llm(client, llm_url, cfg, &prompt).context("llm chat")?;
    println!("llm_chat_reply: {}", reply);
    if let Some(action) = parse_chat_say_action(&reply) {
        return Ok(action);
    }

    let retry = build_chat_retry_prompt(observation_json, last_action, chat);
    let retry_reply = call_llm(client, llm_url, cfg, &retry).context("llm chat retry")?;
    println!("llm_chat_retry_reply: {}", retry_reply);
    if let Some(action) = parse_chat_say_action(&retry_reply) {
        return Ok(action);
    }

    if let Some(message) =
        sanitize_llm_message(&retry_reply).or_else(|| sanitize_llm_message(&reply))
    {
        return Ok(AgentAction {
            action: "say".to_string(),
            target: None,
            message: Some(message),
            direction: None,
            steps: None,
        });
    }

    Ok(AgentAction::default())
}

fn maybe_post_narration(
    client: &Client,
    api_base: &Url,
    llm_url: &Url,
    cfg: &AgentConfig,
    observation_json: &str,
    action: &AgentAction,
    chat_cooldown_until: Option<Instant>,
    recent_say_messages: &mut VecDeque<String>,
) -> Result<()> {
    if let Some(until) = chat_cooldown_until {
        if Instant::now() < until {
            return Ok(());
        }
    }
    let prompt = build_narration_prompt(observation_json, action);
    let reply = call_llm(client, llm_url, cfg, &prompt).context("llm narration")?;
    println!("llm_narration_reply: {}", reply);
    let Some(message) = sanitize_llm_message(&reply) else {
        return Ok(());
    };
    if recent_say_messages.iter().any(|msg| msg == &message) {
        return Ok(());
    }
    post_chat_message(client, api_base, &cfg.api_token, &message)?;
    recent_say_messages.push_back(message);
    if recent_say_messages.len() > 10 {
        recent_say_messages.pop_front();
    }
    Ok(())
}

fn parse_chat_say_action(reply: &str) -> Option<AgentAction> {
    let parsed = parse_action(reply)?;
    if parsed.action.to_lowercase() == "say" {
        let action = validate_say(parsed);
        if action.action == "say" {
            return Some(action);
        }
    }
    None
}

fn sanitize_llm_message(reply: &str) -> Option<String> {
    let mut text = reply.trim();
    if text.is_empty() {
        return None;
    }
    if text.starts_with("```") {
        text = text.trim_start_matches('`').trim();
        if let Some(end) = text.rfind("```") {
            text = text[..end].trim();
        }
    }
    if text.starts_with('"') && text.ends_with('"') && text.len() >= 2 {
        text = &text[1..text.len() - 1];
    }
    let clipped = if text.len() > 120 { &text[..120] } else { text };
    let clipped = clipped.trim();
    if clipped.is_empty() {
        None
    } else {
        Some(clipped.to_string())
    }
}

fn detect_chat_rate_limit(raw: &str) -> bool {
    raw.contains("You cannot send more messages")
}

fn parse_sender_colon_message(message: &str) -> Option<(String, String)> {
    let mut parts = message.splitn(2, ':');
    let name = parts.next()?.trim();
    let rest = parts.next()?.trim();
    if name.is_empty() || rest.is_empty() {
        return None;
    }
    Some((name.to_string(), rest.to_string()))
}

fn parse_sender_angle_message(message: &str) -> Option<(String, String)> {
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

fn build_observation_view(raw: &str, chat: Vec<ChatMessageView>) -> Result<ObservationView> {
    let value: serde_json::Value = serde_json::from_str(raw).context("parse raw observe json")?;
    let position = parse_position(value.get("position")).unwrap_or([0, 0, 0]);
    let facing = value
        .get("facing")
        .and_then(|v| v.as_str())
        .unwrap_or("north")
        .to_string();
    let obstacles = parse_obstacles(value.get("obstacles"));
    let players = parse_players(value.get("items"));
    let nearby_nodes = parse_nearby_nodes(value.get("nodes"), position);
    let goal = value
        .get("goal")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(ObservationView {
        position,
        facing,
        obstacles,
        players,
        nearby_nodes,
        goal,
        chat,
    })
}

fn parse_chat_log(raw: &str) -> Result<(Vec<ChatMessageView>, u64)> {
    let value: serde_json::Value = serde_json::from_str(raw).context("parse chat json")?;
    let last_id = value.get("last").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut entries = Vec::new();
    let Some(messages) = value.get("messages").and_then(|v| v.as_array()) else {
        return Ok((entries, last_id));
    };
    for message in messages {
        let id = message.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut from = message
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let mut msg = message
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if from.is_empty() {
            if let Some((name, rest)) =
                parse_sender_colon_message(&msg).or_else(|| parse_sender_angle_message(&msg))
            {
                from = name;
                msg = rest;
            }
        }
        if from.is_empty() || msg.is_empty() {
            continue;
        }
        if from.starts_with("-!-") || msg.contains("Invalid command") {
            continue;
        }
        if msg == "ok" || msg.starts_with("You cannot send more messages") {
            continue;
        }
        entries.push(ChatMessageView { id, from, msg });
    }
    entries.sort_by_key(|entry| entry.id);
    if entries.len() > 6 {
        entries = entries.split_off(entries.len() - 6);
    }
    Ok((entries, last_id))
}

fn parse_position(value: Option<&serde_json::Value>) -> Option<[i32; 3]> {
    let arr = value?.as_array()?;
    if arr.len() < 3 {
        return None;
    }
    let x = arr.get(0)?.as_i64()? as i32;
    let y = arr.get(1)?.as_i64()? as i32;
    let z = arr.get(2)?.as_i64()? as i32;
    Some([x, y, z])
}

fn parse_obstacles(value: Option<&serde_json::Value>) -> ObstaclesView {
    let get = |key: &str| {
        value
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    };
    ObstaclesView {
        front: get("front"),
        left: get("left"),
        right: get("right"),
        back: get("back"),
    }
}

fn parse_players(value: Option<&serde_json::Value>) -> Vec<PlayerView> {
    let mut players = Vec::new();
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return players;
    };
    for item in items {
        let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("item");
        if kind != "player" {
            continue;
        }
        let name = match item.get("name").and_then(|v| v.as_str()) {
            Some(value) if !value.trim().is_empty() => value.to_string(),
            _ => continue,
        };
        let dx = item.get("dx").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let dy = item.get("dy").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let dz = item.get("dz").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        players.push(PlayerView { name, dx, dy, dz });
    }
    players.sort_by(|a, b| {
        let da = a.dx.abs() + a.dy.abs() + a.dz.abs();
        let db = b.dx.abs() + b.dy.abs() + b.dz.abs();
        da.cmp(&db).then_with(|| a.name.cmp(&b.name))
    });
    players.truncate(8);
    players
}

fn parse_nearby_nodes(value: Option<&serde_json::Value>, origin: [i32; 3]) -> Vec<NodeView> {
    let mut nodes = Vec::new();
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return nodes;
    };
    for item in items {
        let name = match item.get("name").and_then(|v| v.as_str()) {
            Some(value) if !value.trim().is_empty() => value.to_string(),
            _ => continue,
        };
        if name == "air" || name == "ignore" {
            continue;
        }
        let pos = item.get("pos").and_then(|v| v.as_array());
        let Some(pos) = pos else {
            continue;
        };
        if pos.len() < 3 {
            continue;
        }
        let x = pos
            .get(0)
            .and_then(|v| v.as_i64())
            .unwrap_or(origin[0] as i64) as i32;
        let y = pos
            .get(1)
            .and_then(|v| v.as_i64())
            .unwrap_or(origin[1] as i64) as i32;
        let z = pos
            .get(2)
            .and_then(|v| v.as_i64())
            .unwrap_or(origin[2] as i64) as i32;
        let dx = x - origin[0];
        let dy = y - origin[1];
        let dz = z - origin[2];
        if dx.abs() > 1 || dy.abs() > 1 || dz.abs() > 1 {
            continue;
        }
        nodes.push(NodeView { name, dx, dy, dz });
    }
    nodes.sort_by(|a, b| {
        let da = a.dx.abs() + a.dy.abs() + a.dz.abs();
        let db = b.dx.abs() + b.dy.abs() + b.dz.abs();
        da.cmp(&db).then_with(|| a.name.cmp(&b.name))
    });
    nodes.truncate(12);
    nodes
}

fn parse_action(reply: &str) -> Option<AgentAction> {
    let trimmed = reply.trim();
    if let Ok(action) = serde_json::from_str::<AgentAction>(trimmed) {
        return Some(action);
    }
    let extracted = extract_json_object(trimmed)?;
    serde_json::from_str::<AgentAction>(&extracted).ok()
}

fn extract_json_object(input: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut start = None;
    for (idx, ch) in input.char_indices() {
        if ch == '{' {
            if depth == 0 {
                start = Some(idx);
            }
            depth += 1;
        } else if ch == '}' {
            if depth > 0 {
                depth -= 1;
                if depth == 0 {
                    let begin = start?;
                    return Some(input[begin..=idx].to_string());
                }
            }
        }
    }
    None
}

fn validate_action(action: AgentAction, obs: &ObservationView) -> AgentAction {
    let name = action.action.to_lowercase();
    match name.as_str() {
        "stop" => AgentAction::default(),
        "move" => validate_move(action, obs),
        "attack" => validate_target(action, obs, "attack"),
        "follow" => validate_target(action, obs, "follow"),
        "say" => validate_say(action),
        _ => AgentAction::default(),
    }
}

fn validate_move(action: AgentAction, obs: &ObservationView) -> AgentAction {
    let dir = action
        .direction
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let dir = match dir.as_str() {
        "forward" | "backward" | "left" | "right" => dir,
        _ => return AgentAction::default(),
    };
    let steps = action.steps.unwrap_or(1.0);
    let steps = if steps.is_finite() { steps } else { 1.0 };
    let steps = steps.round().clamp(1.0, 3.0);
    AgentAction {
        action: "move".to_string(),
        target: None,
        message: None,
        direction: Some(dir),
        steps: Some(steps),
    }
}

fn validate_target(action: AgentAction, obs: &ObservationView, verb: &str) -> AgentAction {
    let raw = action.target.unwrap_or_default();
    let target = raw.trim();
    if target.is_empty() {
        return AgentAction::default();
    }
    let Some(found) = obs
        .players
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(target))
        .map(|p| p.name.clone())
    else {
        return AgentAction::default();
    };
    AgentAction {
        action: verb.to_string(),
        target: Some(found),
        message: None,
        direction: None,
        steps: None,
    }
}

fn validate_say(action: AgentAction) -> AgentAction {
    validate_say_with_limit(action, 120)
}

fn validate_say_with_limit(action: AgentAction, max_len: usize) -> AgentAction {
    let message = action.message.unwrap_or_default();
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return AgentAction::default();
    }
    let clipped = if trimmed.len() > max_len {
        trimmed[..max_len].to_string()
    } else {
        trimmed.to_string()
    };
    AgentAction {
        action: "say".to_string(),
        target: None,
        message: Some(clipped),
        direction: None,
        steps: None,
    }
}

fn execute_action(
    client: &Client,
    api_base: &Url,
    token: &str,
    action: &AgentAction,
) -> Result<()> {
    let name = action.action.to_lowercase();
    match name.as_str() {
        "stop" => http_post(client, api_base, "/stop", token, "").map(|_| ()),
        "move" => post_move(client, api_base, token, action),
        "follow" => post_target(client, api_base, token, "/follow", action.target.as_deref()),
        "attack" => post_target(client, api_base, token, "/attack", action.target.as_deref()),
        "say" => post_chat_message(
            client,
            api_base,
            token,
            action.message.as_deref().unwrap_or(""),
        ),
        _ => http_post(client, api_base, "/stop", token, "").map(|_| ()),
    }
}

fn post_move(client: &Client, api_base: &Url, token: &str, action: &AgentAction) -> Result<()> {
    if let Some(dir) = action.direction.as_deref() {
        let steps = action.steps.unwrap_or(1.0);
        let query = format!("?direction={}&steps={}", url_escape(dir), steps);
        let response = http_post(client, api_base, &format!("/move{}", query), token, "")?;
        println!(
            "move sent: direction={} steps={} response={}",
            dir, steps, response
        );
        return Ok(());
    }
    http_post(client, api_base, "/stop", token, "").map(|_| ())
}

fn post_target(
    client: &Client,
    api_base: &Url,
    token: &str,
    path: &str,
    target: Option<&str>,
) -> Result<()> {
    let target = target.unwrap_or("").trim();
    if target.is_empty() {
        return http_post(client, api_base, "/stop", token, "").map(|_| ());
    }
    let query = format!("?target={}", url_escape(target));
    http_post(client, api_base, &format!("{}{}", path, query), token, "").map(|_| ())
}

fn post_chat_message(client: &Client, api_base: &Url, token: &str, message: &str) -> Result<()> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return http_post(client, api_base, "/stop", token, "").map(|_| ());
    }
    let body = json!({"message": trimmed});
    let response = http_post_json(client, api_base, "/chat", token, &body)?;
    println!("chat sent: message={} response={}", trimmed, response);
    Ok(())
}

fn call_llm(client: &Client, llm_url: &Url, cfg: &AgentConfig, prompt: &str) -> Result<String> {
    let base_body = json!({
        "model": cfg.model,
        "temperature": cfg.temperature,
        "max_tokens": cfg.max_tokens,
        "messages": [
            {"role": "system", "content": "You are a precise JSON policy."},
            {"role": "user", "content": prompt}
        ]
    });
    let mut constrained_body = base_body.clone();
    if let Some(obj) = constrained_body.as_object_mut() {
        obj.insert(
            "response_format".to_string(),
            json!({"type": "json_object"}),
        );
    }

    let (status, response) = post_json(client, llm_url, &constrained_body).context("llm post")?;
    if status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY {
        let (status, response) =
            post_json(client, llm_url, &base_body).context("llm post fallback")?;
        ensure_success(status, &response).context("llm response")?;
        return parse_llm_response(&response);
    }
    ensure_success(status, &response).context("llm response")?;
    parse_llm_response(&response)
}

fn parse_llm_response(body: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct ChatResponse {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: Message,
    }
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }

    let resp: ChatResponse = serde_json::from_str(body).context("parse llm json")?;
    let content = resp
        .choices
        .get(0)
        .map(|c| c.message.content.trim().to_string())
        .unwrap_or_default();
    if content.is_empty() {
        bail!("empty llm response content");
    }
    Ok(content)
}

fn http_get(client: &Client, base: &Url, path: &str, token: &str) -> Result<String> {
    let url = base.join(path).context("join api url")?;
    let mut req = client.get(url);
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let response = req.send().context("send get request")?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    ensure_success(status, &body).context("get response")?;
    Ok(body)
}

fn http_post(client: &Client, base: &Url, path: &str, token: &str, body: &str) -> Result<String> {
    let url = base.join(path).context("join api url")?;
    let mut req = client.post(url);
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    if !body.is_empty() {
        req = req
            .header("Content-Type", "application/json")
            .body(body.to_string());
    }
    let response = req.send().context("send post request")?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    ensure_success(status, &body).context("post response")?;
    Ok(body)
}

fn http_post_json(
    client: &Client,
    base: &Url,
    path: &str,
    token: &str,
    body: &serde_json::Value,
) -> Result<String> {
    let url = base.join(path).context("join api url")?;
    let mut req = client.post(url).json(body);
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let response = req.send().context("send post request")?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    ensure_success(status, &body).context("post response")?;
    Ok(body)
}

fn format_action_message(action: &AgentAction) -> String {
    let name = action.action.to_lowercase();
    match name.as_str() {
        "move" => {
            let dir = action
                .direction
                .clone()
                .unwrap_or_else(|| "forward".to_string());
            let steps = action.steps.unwrap_or(1.0).round().clamp(1.0, 3.0);
            format!("Moving {} x{}", dir, steps as i32)
        }
        "follow" => {
            let target = action
                .target
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            format!("Following {}", target)
        }
        "attack" => {
            let target = action
                .target
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            format!("Attacking {}", target)
        }
        _ => "".to_string(),
    }
}

fn post_json(client: &Client, url: &Url, body: &serde_json::Value) -> Result<(StatusCode, String)> {
    let response = client
        .post(url.clone())
        .json(body)
        .send()
        .context("send json request")?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    Ok((status, body))
}

fn ensure_success(status: StatusCode, body: &str) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    bail!("http status {}: {}", status, body)
}

fn url_escape(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
