use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::{StatusCode, Url};
use serde_json::Value;

use super::util::clip_chars;

use super::chat::parse_embedded_sender;
use super::state::{parse_observation, ObservationSnapshot, PendingChatMessage};

pub(super) fn fetch_observation(
    client: &Client,
    api_base: &Url,
    token: &str,
    radius: i32,
) -> Result<ObservationSnapshot> {
    let server_path = format!("/observe_server?radius={radius}");
    let raw = match http_get(client, api_base, &server_path, token) {
        Ok(raw) => raw,
        Err(server_error) => {
            let fallback_path = format!("/observe?radius={radius}");
            http_get(client, api_base, &fallback_path, token).with_context(|| {
                format!("server observation unavailable ({server_error:#}); fallback failed")
            })?
        }
    };
    parse_observation(&raw)
}

pub(super) fn fetch_chat(
    client: &Client,
    api_base: &Url,
    token: &str,
    since: u64,
) -> Result<(Vec<PendingChatMessage>, u64)> {
    let raw = http_get(
        client,
        api_base,
        &format!("/chat?since={since}&limit=100"),
        token,
    )?;
    let value: serde_json::Value = serde_json::from_str(&raw).context("parse chat response")?;
    let cursor = value
        .get("last")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(since);
    let mut messages = Vec::new();
    for item in value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = item
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let mut from = item
            .get("from")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let mut message = item
            .get("msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if from.is_empty() {
            if let Some((parsed_from, parsed_message)) = parse_embedded_sender(&message) {
                from = parsed_from;
                message = parsed_message;
            }
        }
        if id > since
            && !from.is_empty()
            && !message.is_empty()
            && !message.contains("Invalid command")
            && !message.starts_with("You cannot send more messages")
        {
            messages.push(PendingChatMessage { id, from, message });
        }
    }
    messages.sort_by_key(|message| message.id);
    if messages.len() > 12 {
        messages = messages.split_off(messages.len() - 12);
    }
    Ok((messages, cursor))
}

pub(super) fn parse_http_url(value: &str) -> Result<Url> {
    let value = value.trim();
    let normalized = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    Url::parse(&normalized).with_context(|| format!("invalid URL '{value}'"))
}

fn http_get(client: &Client, base: &Url, path: &str, token: &str) -> Result<String> {
    let url = base.join(path).with_context(|| format!("join bot API path {path}"))?;
    let response = authenticated(client.get(url), token)
        .send()
        .with_context(|| format!("GET {path}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    anyhow::ensure!(status.is_success(), "bot API {path} returned {status}: {body}");
    Ok(body)
}

pub(super) fn post(
    client: &Client,
    base: &Url,
    token: &str,
    path: &str,
    body: Option<Value>,
) -> Result<String> {
    let url = base.join(path).with_context(|| format!("join bot API path {path}"))?;
    let mut request = authenticated(client.post(url), token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().with_context(|| format!("POST {path}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    api_response_body(path, status, &body)
}

pub(super) fn post_query(
    client: &Client,
    base: &Url,
    token: &str,
    path: &str,
    params: &[(&str, String)],
) -> Result<String> {
    let mut url = base.join(path).with_context(|| format!("join bot API path {path}"))?;
    {
        let mut query = url.query_pairs_mut();
        for (name, value) in params {
            query.append_pair(name, value);
        }
    }
    let response = authenticated(client.post(url), token)
        .send()
        .with_context(|| format!("POST {path}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    api_response_body(path, status, &body)
}

pub(super) fn api_response_body(
    path: &str,
    status: StatusCode,
    body: &str,
) -> Result<String> {
    if !status.is_success() {
        bail!("bot API {path} returned {status}: {body}");
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Ok("ok".to_string());
    }
    if let Ok(payload) = serde_json::from_str::<Value>(trimmed) {
        if payload.get("ok").and_then(Value::as_bool) == Some(false) {
            let reason = payload
                .get("error")
                .or_else(|| payload.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("request_failed");
            bail!("bot API {path} reported failure: {reason} ({trimmed})");
        }
    }
    Ok(clip_chars(trimmed, 500))
}

fn authenticated(request: RequestBuilder, token: &str) -> RequestBuilder {
    if token.trim().is_empty() {
        request
    } else {
        request.bearer_auth(token.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_api_auth_trims_tokens_and_omits_empty_tokens() {
        let client = Client::new();
        for token in ["", "   "] {
            let request = authenticated(client.get("http://127.0.0.1:9123/chat"), token)
                .build()
                .unwrap();
            assert!(!request.headers().contains_key(reqwest::header::AUTHORIZATION));
        }
        let request = authenticated(
            client.get("http://127.0.0.1:9123/chat"),
            " local-bot-token ",
        )
        .build()
        .unwrap();
        assert_eq!(
            request.headers()[reqwest::header::AUTHORIZATION],
            "Bearer local-bot-token"
        );
    }

    #[test]
    fn action_responses_preserve_plain_text_and_report_http_errors() {
        assert_eq!(api_response_body("/move", StatusCode::OK, " OK\n").unwrap(), "OK");
        assert_eq!(api_response_body("/move", StatusCode::OK, " \n").unwrap(), "ok");
        let error = api_response_body("/move", StatusCode::UNAUTHORIZED, "unauthorized")
            .unwrap_err();
        assert!(error.to_string().contains("401 Unauthorized"));
    }

    #[test]
    fn accepts_host_port_as_http_url() {
        assert_eq!(
            parse_http_url("127.0.0.1:9123").unwrap().as_str(),
            "http://127.0.0.1:9123/"
        );
    }

    #[test]
    fn semantic_api_failure_is_not_reported_as_tool_success() {
        let error = api_response_body(
            "/mine",
            StatusCode::OK,
            r#"{"ok":false,"status":"no_block"}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("no_block"));
    }
}
