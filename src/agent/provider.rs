use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LlmApi {
    Auto,
    ChatCompletions,
    Responses,
}

impl FromStr for LlmApi {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "chat" | "chat-completions" | "chat_completions" => Ok(Self::ChatCompletions),
            "responses" | "response" => Ok(Self::Responses),
            other => Err(format!(
                "invalid LLM API '{other}'; use auto, chat-completions, or responses"
            )),
        }
    }
}

impl LlmApi {
    fn resolve(self, url: &Url) -> Self {
        match self {
            Self::Auto if url.path().trim_end_matches('/').ends_with("/responses") => {
                Self::Responses
            }
            Self::Auto => Self::ChatCompletions,
            explicit => explicit,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub url: Url,
    pub api: LlmApi,
    pub api_key: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TokenUsage {
    pub input: u64,
    #[serde(default)]
    pub cached_input: u64,
    pub output: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Default)]
pub struct ModelDecision {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub incomplete_reason: Option<String>,
}

pub fn request_decision(
    client: &Client,
    cfg: &ProviderConfig,
    instructions: &str,
    input: &str,
    tools: &[Value],
) -> Result<ModelDecision> {
    match cfg.api.resolve(&cfg.url) {
        LlmApi::Responses => request_responses(client, cfg, instructions, input, tools),
        LlmApi::ChatCompletions | LlmApi::Auto => {
            request_chat_completions(client, cfg, instructions, input, tools)
        }
    }
}

fn request_chat_completions(
    client: &Client,
    cfg: &ProviderConfig,
    instructions: &str,
    input: &str,
    tools: &[Value],
) -> Result<ModelDecision> {
    let wrapped_tools: Vec<Value> = tools
        .iter()
        .map(|tool| json!({"type": "function", "function": tool}))
        .collect();
    let mut body = json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": instructions},
            {"role": "user", "content": input}
        ],
        "tools": wrapped_tools,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "max_completion_tokens": cfg.max_tokens,
    });
    if cfg.temperature.is_finite() {
        body["temperature"] = json!(cfg.temperature.clamp(0.0, 2.0));
    }
    if let Some(effort) = normalized_reasoning_effort(cfg.reasoning_effort.as_deref()) {
        body["reasoning_effort"] = json!(effort);
    }

    let (mut status, mut response) = post_json(client, cfg, &body)?;
    if is_compatibility_error(status) {
        let mut modern_compatibility = body.clone();
        if let Some(object) = modern_compatibility.as_object_mut() {
            object.remove("temperature");
            object.remove("parallel_tool_calls");
        }
        (status, response) = post_json(client, cfg, &modern_compatibility)
            .context("retry chat completions without optional sampling fields")?;
    }
    if is_compatibility_error(status) {
        let mut compatibility = body.clone();
        if let Some(object) = compatibility.as_object_mut() {
            object.remove("reasoning_effort");
            object.remove("max_completion_tokens");
            object.insert("max_tokens".to_string(), json!(cfg.max_tokens));
            object.remove("parallel_tool_calls");
        }
        remove_strict_from_chat_tools(&mut compatibility);
        (status, response) = post_json(client, cfg, &compatibility)
            .context("retry chat completions with local-server compatibility fields")?;
    }
    if is_compatibility_error(status) {
        return request_legacy_json(
            client,
            cfg,
            instructions,
            input,
            tools,
            status,
            &response,
        );
    }
    ensure_success(status, &response)?;
    parse_chat_response(&response)
}

fn request_responses(
    client: &Client,
    cfg: &ProviderConfig,
    instructions: &str,
    input: &str,
    tools: &[Value],
) -> Result<ModelDecision> {
    let response_tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut flattened = tool.clone();
            if let Some(object) = flattened.as_object_mut() {
                object.insert("type".to_string(), json!("function"));
            }
            flattened
        })
        .collect();
    let mut body = json!({
        "model": cfg.model,
        "instructions": instructions,
        "input": input,
        "tools": response_tools,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "max_output_tokens": cfg.max_tokens,
    });
    if let Some(effort) = normalized_reasoning_effort(cfg.reasoning_effort.as_deref()) {
        body["reasoning"] = json!({"effort": effort});
    }
    if is_official_openai_endpoint(&cfg.url) {
        body["prompt_cache_key"] = json!(prompt_cache_key(
            &cfg.model,
            instructions,
            &response_tools
        ));
    }

    let (mut status, mut response) = post_json(client, cfg, &body)?;
    if is_compatibility_error(status) {
        let mut compatibility = body.clone();
        if let Some(object) = compatibility.as_object_mut() {
            object.remove("reasoning");
            object.remove("parallel_tool_calls");
        }
        remove_strict_from_response_tools(&mut compatibility);
        (status, response) = post_json(client, cfg, &compatibility)
            .context("retry responses request with compatibility fields")?;
    }
    ensure_success(status, &response)?;
    parse_responses_response(&response)
}

fn request_legacy_json(
    client: &Client,
    cfg: &ProviderConfig,
    instructions: &str,
    input: &str,
    tools: &[Value],
    previous_status: StatusCode,
    previous_response: &str,
) -> Result<ModelDecision> {
    let legacy_instruction = legacy_instructions(instructions, tools);
    let body = json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": legacy_instruction},
            {"role": "user", "content": input}
        ],
        "temperature": cfg.temperature.clamp(0.0, 2.0),
        "max_tokens": cfg.max_tokens,
        "response_format": {"type": "json_object"}
    });
    let (mut status, mut response) = post_json(client, cfg, &body)
        .with_context(|| format!("native tools rejected with {previous_status}: {previous_response}"))?;
    if is_compatibility_error(status) {
        let mut plain = body;
        if let Some(object) = plain.as_object_mut() {
            object.remove("response_format");
        }
        (status, response) = post_json(client, cfg, &plain).context("legacy JSON fallback")?;
    }
    ensure_success(status, &response)?;
    let mut decision = parse_chat_response(&response)?;
    if decision.tool_calls.is_empty() {
        if let Some(text) = decision.text.as_deref() {
            if let Some(arguments) = extract_json_object(text) {
                let name = arguments
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("stop")
                    .to_string();
                decision.tool_calls.push(ToolCall {
                    id: "legacy-json".to_string(),
                    name,
                    arguments,
                });
            }
        }
    }
    Ok(decision)
}

fn legacy_instructions(instructions: &str, tools: &[Value]) -> String {
    let catalog = serde_json::to_string(tools).unwrap_or_else(|_| "[]".to_string());
    format!(
        "{instructions}\nNative function calling is unavailable. Return exactly one JSON object with `action` set to an available tool name and its schema fields at the top level. Available tool schemas: {catalog}"
    )
}

fn parse_chat_response(body: &str) -> Result<ModelDecision> {
    let value: Value = serde_json::from_str(body).context("parse chat-completions response")?;
    let message = value
        .pointer("/choices/0/message")
        .context("chat-completions response has no first message")?;
    let text = message
        .get("content")
        .and_then(content_text)
        .filter(|text| !text.trim().is_empty());
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in calls.iter().enumerate() {
            let function = call.get("function").unwrap_or(call);
            let Some(name) = function.get("name").and_then(Value::as_str) else {
                continue;
            };
            let arguments = parse_arguments(function.get("arguments"));
            tool_calls.push(ToolCall {
                id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call-{index}")),
                name: name.to_string(),
                arguments,
            });
        }
    }
    Ok(ModelDecision {
        text,
        tool_calls,
        usage: parse_usage(&value, false),
        incomplete_reason: chat_incomplete_reason(&value),
    })
}

fn parse_responses_response(body: &str) -> Result<ModelDecision> {
    let value: Value = serde_json::from_str(body).context("parse Responses API response")?;
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for (index, item) in output.iter().enumerate() {
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "function_call" => {
                    let Some(name) = item.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    tool_calls.push(ToolCall {
                        id: item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call-{index}")),
                        name: name.to_string(),
                        arguments: parse_arguments(item.get("arguments")),
                    });
                }
                "message" => {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            if matches!(
                                part.get("type").and_then(Value::as_str),
                                Some("output_text") | Some("text")
                            ) {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    text_parts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if text_parts.is_empty() {
        if let Some(text) = value.get("output_text").and_then(Value::as_str) {
            text_parts.push(text.to_string());
        }
    }
    let text = (!text_parts.is_empty()).then(|| text_parts.join("\n"));
    Ok(ModelDecision {
        text,
        tool_calls,
        usage: parse_usage(&value, true),
        incomplete_reason: responses_incomplete_reason(&value),
    })
}

fn chat_incomplete_reason(value: &Value) -> Option<String> {
    match value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
    {
        Some("length") => Some("max_output_tokens".to_string()),
        Some(reason) if !matches!(reason, "stop" | "tool_calls" | "function_call") => {
            Some(reason.to_string())
        }
        _ => None,
    }
}

fn responses_incomplete_reason(value: &Value) -> Option<String> {
    if value.get("status").and_then(Value::as_str) != Some("incomplete") {
        return None;
    }
    Some(
        value
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            .unwrap_or("incomplete")
            .to_string(),
    )
}

fn content_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    let parts = value.as_array()?;
    let joined = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

fn parse_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(raw)) => serde_json::from_str(raw).unwrap_or_else(|_| json!({})),
        Some(Value::Object(_)) => value.cloned().unwrap_or_else(|| json!({})),
        _ => json!({}),
    }
}

fn parse_usage(value: &Value, responses: bool) -> TokenUsage {
    let usage = value.get("usage").unwrap_or(&Value::Null);
    let input_key = if responses { "input_tokens" } else { "prompt_tokens" };
    let output_key = if responses {
        "output_tokens"
    } else {
        "completion_tokens"
    };
    let input = usage.get(input_key).and_then(Value::as_u64).unwrap_or(0);
    let cached_input = if responses {
        usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    } else {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let output = usage.get(output_key).and_then(Value::as_u64).unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input.saturating_add(output));
    TokenUsage {
        input,
        cached_input,
        output,
        total,
    }
}

fn is_official_openai_endpoint(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.openai.com"))
}

fn prompt_cache_key(model: &str, instructions: &str, tools: &[Value]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"luanti-agent-prompt-v1\0");
    hash.update(model.as_bytes());
    hash.update(b"\0");
    hash.update(instructions.as_bytes());
    hash.update(b"\0");
    hash.update(serde_json::to_vec(tools).unwrap_or_default());
    let suffix = hash
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("luanti-agent-{suffix}")
}

fn normalized_reasoning_effort(value: Option<&str>) -> Option<&str> {
    match value.map(str::trim) {
        Some("") | None => None,
        Some(value) => Some(value),
    }
}

fn authenticated_request(client: &Client, cfg: &ProviderConfig) -> RequestBuilder {
    let mut request = client
        .post(cfg.url.clone())
        .header("User-Agent", "luanti-proto-bot/0.1");
    if !cfg.api_key.trim().is_empty() {
        request = request.bearer_auth(cfg.api_key.trim());
    }
    request
}

fn post_json(
    client: &Client,
    cfg: &ProviderConfig,
    body: &Value,
) -> Result<(StatusCode, String)> {
    let response = authenticated_request(client, cfg)
        .json(body)
        .send()
        .context("send LLM request")?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    Ok((status, body))
}

fn ensure_success(status: StatusCode, body: &str) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        bail!("LLM HTTP status {status}: {body}")
    }
}

fn is_compatibility_error(status: StatusCode) -> bool {
    status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY
}

fn remove_strict_from_chat_tools(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools {
        if let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) {
            function.remove("strict");
        }
    }
}

fn remove_strict_from_response_tools(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools {
        if let Some(object) = tool.as_object_mut() {
            object.remove("strict");
        }
    }
}

fn extract_json_object(input: &str) -> Option<Value> {
    if let Ok(value @ Value::Object(_)) = serde_json::from_str(input.trim()) {
        return Some(value);
    }
    let start = input.find('{')?;
    let end = input.rfind('}')?;
    serde_json::from_str(&input[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_tool_calls_and_usage() {
        let body = r#"{
            "choices":[{"message":{"content":null,"tool_calls":[{
                "id":"call_1","type":"function","function":{"name":"move","arguments":"{\"direction\":\"forward\",\"steps\":2}"}
            }]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}
        }"#;
        let decision = parse_chat_response(body).unwrap();
        assert_eq!(decision.tool_calls[0].name, "move");
        assert_eq!(decision.tool_calls[0].arguments["steps"], 2);
        assert_eq!(decision.usage.total, 14);
        assert_eq!(decision.usage.cached_input, 0);
    }

    #[test]
    fn parses_responses_tool_calls_and_usage() {
        let body = r#"{
            "output":[{"type":"function_call","call_id":"call_2","name":"sleep","arguments":"{\"radius\":6}"}],
            "usage":{"input_tokens":12,"input_tokens_details":{"cached_tokens":8},"output_tokens":3,"total_tokens":15}
        }"#;
        let decision = parse_responses_response(body).unwrap();
        assert_eq!(decision.tool_calls[0].name, "sleep");
        assert_eq!(decision.tool_calls[0].arguments["radius"], 6);
        assert_eq!(decision.usage.input, 12);
        assert_eq!(decision.usage.cached_input, 8);
    }

    #[test]
    fn reports_chat_output_limit() {
        let body = r#"{
            "choices":[{"finish_reason":"length","message":{"content":"thinking"}}],
            "usage":{"prompt_tokens":10,"completion_tokens":256,"total_tokens":266}
        }"#;
        let decision = parse_chat_response(body).unwrap();
        assert_eq!(decision.incomplete_reason.as_deref(), Some("max_output_tokens"));
    }

    #[test]
    fn reports_responses_output_limit() {
        let body = r#"{
            "status":"incomplete",
            "incomplete_details":{"reason":"max_output_tokens"},
            "output":[],
            "usage":{"input_tokens":10,"output_tokens":256,"total_tokens":266}
        }"#;
        let decision = parse_responses_response(body).unwrap();
        assert_eq!(decision.incomplete_reason.as_deref(), Some("max_output_tokens"));
    }

    #[test]
    fn auto_detects_responses_endpoint() {
        let url = Url::parse("https://api.openai.com/v1/responses").unwrap();
        assert_eq!(LlmApi::Auto.resolve(&url), LlmApi::Responses);
    }

    #[test]
    fn cache_key_is_stable_for_one_prompt_profile() {
        let tools = vec![json!({"name":"move"})];
        let first = prompt_cache_key("gpt-5-nano", "instructions", &tools);
        assert_eq!(first, prompt_cache_key("gpt-5-nano", "instructions", &tools));
        assert_ne!(first, prompt_cache_key("gpt-5-nano", "changed", &tools));
        assert!(first.len() <= 64);
    }

    #[test]
    fn only_official_openai_requests_get_cache_routing() {
        assert!(is_official_openai_endpoint(
            &Url::parse("https://api.openai.com/v1/responses").unwrap()
        ));
        assert!(!is_official_openai_endpoint(
            &Url::parse("http://127.0.0.1:8080/v1/responses").unwrap()
        ));
    }

    #[test]
    fn legacy_fallback_keeps_the_filtered_tool_contract() {
        let tools = vec![json!({
            "name":"move",
            "parameters":{"type":"object","properties":{"steps":{"type":"number"}}}
        })];
        let instructions = legacy_instructions("base", &tools);
        assert!(instructions.contains("Available tool schemas"));
        assert!(instructions.contains("\"name\":\"move\""));
        assert!(instructions.contains("\"steps\""));
    }
}
