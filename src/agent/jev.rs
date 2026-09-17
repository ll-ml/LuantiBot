//! Minimal blocking client for TypeSafe's System One API.
//!
//! The client deliberately models only `choice` questions.  The bot can turn
//! its currently valid, fully-parameterized actions into choice criteria and
//! keep all execution and safety checks in ordinary Rust code.

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::thread::sleep;
use std::time::Duration;

pub const DEFAULT_JEV_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
pub const DEFAULT_JEV_MODEL: &str = "jev-latest";
pub const DEFAULT_JEV_TIMEOUT: Duration = Duration::from_secs(15);

const MAX_CHOICE_OPTIONS: usize = 255;
const ERROR_BODY_LIMIT: usize = 8 * 1024;
const PROBABILITY_SUM_TOLERANCE: f64 = 0.02;
const MAX_RATE_LIMIT_RETRIES: u32 = 2;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(5);
const RETRY_BASE_DELAY: Duration = Duration::from_millis(250);

pub type ChoiceQuestions = BTreeMap<String, ChoiceQuestion>;
pub type ChoiceAnswers = BTreeMap<String, ChoiceAnswer>;
pub type JevResult<T> = Result<T, JevError>;

/// Configuration for a System One endpoint.
///
/// Its custom `Debug` implementation intentionally redacts the API key.
#[derive(Clone)]
pub struct JevConfig {
    endpoint: Url,
    api_key: String,
    model: String,
    timeout: Duration,
}

impl JevConfig {
    pub fn new(api_key: impl Into<String>) -> JevResult<Self> {
        let endpoint = Url::parse(DEFAULT_JEV_ENDPOINT)
            .map_err(|error| JevError::InvalidConfig(format!("invalid default endpoint: {error}")))?;
        let api_key = api_key.into();
        let config = Self {
            endpoint,
            api_key: api_key.trim().to_string(),
            model: DEFAULT_JEV_MODEL.to_string(),
            timeout: DEFAULT_JEV_TIMEOUT,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_endpoint(mut self, endpoint: Url) -> JevResult<Self> {
        self.endpoint = endpoint;
        self.validate()?;
        Ok(self)
    }

    pub fn with_model(mut self, model: impl Into<String>) -> JevResult<Self> {
        self.model = model.into().trim().to_string();
        self.validate()?;
        Ok(self)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> JevResult<Self> {
        self.timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn validate(&self) -> JevResult<()> {
        if !matches!(self.endpoint.scheme(), "http" | "https") {
            return Err(JevError::InvalidConfig(
                "Jev endpoint must use http or https".to_string(),
            ));
        }
        if self.api_key.trim().is_empty() {
            return Err(JevError::InvalidConfig(
                "Jev API key cannot be empty".to_string(),
            ));
        }
        if self.model.trim().is_empty() {
            return Err(JevError::InvalidConfig(
                "Jev model cannot be empty".to_string(),
            ));
        }
        if self.timeout.is_zero() {
            return Err(JevError::InvalidConfig(
                "Jev request timeout must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for JevConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JevConfig")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Clone)]
pub struct JevClient {
    http: Client,
    config: JevConfig,
}

impl JevClient {
    pub fn new(config: JevConfig) -> JevResult<Self> {
        config.validate()?;
        let http = Client::builder()
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|source| JevError::Transport {
                operation: "build HTTP client",
                source,
            })?;
        Ok(Self { http, config })
    }

    pub fn config(&self) -> &JevConfig {
        &self.config
    }

    /// Evaluates all choice questions in one System One request.
    ///
    /// `state` may serialize to a JSON string, object, or array. Question and
    /// option identifiers are application-owned IDs; the chosen option should
    /// be resolved back to a typed action and revalidated before execution.
    pub fn evaluate_choices<T>(
        &self,
        state: &T,
        questions: &ChoiceQuestions,
    ) -> JevResult<JevResponse>
    where
        T: Serialize + ?Sized,
    {
        validate_questions(questions)?;
        let state = serde_json::to_value(state).map_err(JevError::SerializeState)?;
        validate_state(&state)?;

        let request = SystemOneRequest {
            state: &state,
            model: &self.config.model,
            questions,
        };
        for attempt in 0..=MAX_RATE_LIMIT_RETRIES {
            let response = self
                .http
                .post(self.config.endpoint.clone())
                .bearer_auth(&self.config.api_key)
                .timeout(self.config.timeout)
                .json(&request)
                .send()
                .map_err(|source| JevError::Transport {
                    operation: "send System One request",
                    source,
                })?;

            let status = response.status();
            let retry_delay = rate_limit_retry_delay(status, response.headers(), attempt);
            let body = response.text().map_err(|source| JevError::Transport {
                operation: "read System One response body",
                source,
            })?;
            if attempt < MAX_RATE_LIMIT_RETRIES {
                if let Some(delay) = retry_delay {
                    sleep(delay);
                    continue;
                }
            }
            return decode_response(status, &body, questions);
        }
        unreachable!("bounded Jev retry loop must return")
    }
}

impl fmt::Debug for JevClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JevClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChoiceQuestionType {
    Choice,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChoiceQuestion {
    #[serde(rename = "type")]
    pub question_type: ChoiceQuestionType,
    pub instructions: Value,
    pub criteria: BTreeMap<String, Option<String>>,
}

impl ChoiceQuestion {
    pub fn new(
        instructions: impl Into<String>,
        criteria: BTreeMap<String, Option<String>>,
    ) -> Self {
        Self {
            question_type: ChoiceQuestionType::Choice,
            instructions: Value::String(instructions.into()),
            criteria,
        }
    }

}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JevResponse {
    pub model: String,
    pub answers: ChoiceAnswers,
    pub usage: JevUsage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChoiceAnswerType {
    Choice,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChoiceAnswer {
    #[serde(rename = "type")]
    pub answer_type: ChoiceAnswerType,
    pub choice: String,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}

impl ChoiceAnswer {
    pub fn selected_probability(&self) -> Option<f64> {
        self.probabilities.get(&self.choice).copied()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct JevUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug)]
pub enum JevError {
    InvalidConfig(String),
    InvalidRequest(String),
    SerializeState(serde_json::Error),
    Transport {
        operation: &'static str,
        source: reqwest::Error,
    },
    Http {
        status: StatusCode,
        body: String,
    },
    Decode {
        source: serde_json::Error,
        body: String,
    },
    InvalidResponse(String),
}

impl fmt::Display for JevError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => {
                write!(formatter, "invalid Jev configuration: {message}")
            }
            Self::InvalidRequest(message) => write!(formatter, "invalid Jev request: {message}"),
            Self::SerializeState(source) => write!(formatter, "serialize Jev state: {source}"),
            Self::Transport { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::Http { status, body } => {
                write!(formatter, "Jev HTTP status {status}: {body}")
            }
            Self::Decode { source, body } => {
                write!(formatter, "decode Jev response: {source}; body: {body}")
            }
            Self::InvalidResponse(message) => write!(formatter, "invalid Jev response: {message}"),
        }
    }
}

impl Error for JevError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SerializeState(source) => Some(source),
            Self::Transport { source, .. } => Some(source),
            Self::Decode { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Serialize)]
struct SystemOneRequest<'a> {
    state: &'a Value,
    model: &'a str,
    questions: &'a ChoiceQuestions,
}

fn validate_state(state: &Value) -> JevResult<()> {
    if matches!(state, Value::String(_) | Value::Object(_) | Value::Array(_)) {
        Ok(())
    } else {
        Err(JevError::InvalidRequest(
            "state must serialize to a JSON string, object, or array".to_string(),
        ))
    }
}

fn validate_questions(questions: &ChoiceQuestions) -> JevResult<()> {
    if questions.is_empty() {
        return Err(JevError::InvalidRequest(
            "at least one choice question is required".to_string(),
        ));
    }

    for (question_id, question) in questions {
        if question_id.trim().is_empty() {
            return Err(JevError::InvalidRequest(
                "question IDs cannot be empty".to_string(),
            ));
        }
        if !matches!(
            &question.instructions,
            Value::String(_) | Value::Object(_) | Value::Array(_)
        ) {
            return Err(JevError::InvalidRequest(format!(
                "question '{question_id}' instructions must be a string, object, or array"
            )));
        }
        if let Value::String(instructions) = &question.instructions {
            if instructions.trim().is_empty() {
                return Err(JevError::InvalidRequest(format!(
                    "question '{question_id}' instructions cannot be empty"
                )));
            }
        }
        if !(2..=MAX_CHOICE_OPTIONS).contains(&question.criteria.len()) {
            return Err(JevError::InvalidRequest(format!(
                "question '{question_id}' must contain between 2 and {MAX_CHOICE_OPTIONS} options"
            )));
        }
        for (option, description) in &question.criteria {
            if option.trim().is_empty() {
                return Err(JevError::InvalidRequest(format!(
                    "question '{question_id}' contains an empty option ID"
                )));
            }
            if description
                .as_deref()
                .is_some_and(|description| description.trim().is_empty())
            {
                return Err(JevError::InvalidRequest(format!(
                    "question '{question_id}' option '{option}' has an empty description; use null instead"
                )));
            }
        }
    }
    Ok(())
}

fn decode_response(
    status: StatusCode,
    body: &str,
    questions: &ChoiceQuestions,
) -> JevResult<JevResponse> {
    if !status.is_success() {
        return Err(JevError::Http {
            status,
            body: body_excerpt(body),
        });
    }

    let response: JevResponse = serde_json::from_str(body).map_err(|source| JevError::Decode {
        source,
        body: body_excerpt(body),
    })?;
    validate_response(&response, questions)?;
    Ok(response)
}

fn rate_limit_retry_delay(
    status: StatusCode,
    headers: &HeaderMap,
    attempt: u32,
) -> Option<Duration> {
    if status != StatusCode::TOO_MANY_REQUESTS && status.as_u16() != 529 {
        return None;
    }
    let server_delay = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .map(|delay| delay.min(MAX_RETRY_AFTER));
    Some(server_delay.unwrap_or_else(|| RETRY_BASE_DELAY.saturating_mul(1 << attempt.min(4))))
}

fn validate_response(response: &JevResponse, questions: &ChoiceQuestions) -> JevResult<()> {
    if response.model.trim().is_empty() {
        return Err(JevError::InvalidResponse(
            "response model cannot be empty".to_string(),
        ));
    }
    if response.answers.len() != questions.len() {
        return Err(JevError::InvalidResponse(format!(
            "expected {} answers, received {}",
            questions.len(),
            response.answers.len()
        )));
    }

    for (question_id, question) in questions {
        let answer = response.answers.get(question_id).ok_or_else(|| {
            JevError::InvalidResponse(format!("missing answer for question '{question_id}'"))
        })?;
        if !question.criteria.contains_key(&answer.choice) {
            return Err(JevError::InvalidResponse(format!(
                "question '{question_id}' selected unknown option '{}'",
                answer.choice
            )));
        }
        if !(answer.confidence.is_finite() && (0.0..=1.0).contains(&answer.confidence)) {
            return Err(JevError::InvalidResponse(format!(
                "question '{question_id}' returned invalid confidence {}",
                answer.confidence
            )));
        }
        if answer.probabilities.len() != question.criteria.len()
            || answer
                .probabilities
                .keys()
                .any(|option| !question.criteria.contains_key(option))
        {
            return Err(JevError::InvalidResponse(format!(
                "question '{question_id}' did not return probabilities for exactly the offered options"
            )));
        }

        let mut probability_sum = 0.0;
        for (option, probability) in &answer.probabilities {
            if !(probability.is_finite() && (0.0..=1.0).contains(probability)) {
                return Err(JevError::InvalidResponse(format!(
                    "question '{question_id}' option '{option}' returned invalid probability {probability}"
                )));
            }
            probability_sum += probability;
        }
        if (probability_sum - 1.0).abs() > PROBABILITY_SUM_TOLERANCE {
            return Err(JevError::InvalidResponse(format!(
                "question '{question_id}' probabilities sum to {probability_sum:.6}, not 1"
            )));
        }
    }
    Ok(())
}

fn body_excerpt(body: &str) -> String {
    if body.is_empty() {
        return "<empty body>".to_string();
    }

    let mut chars = body.chars();
    let excerpt: String = chars.by_ref().take(ERROR_BODY_LIMIT).collect();
    if chars.next().is_some() {
        format!("{excerpt}…<truncated>")
    } else {
        excerpt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    fn questions() -> ChoiceQuestions {
        BTreeMap::from([(
            "next_action".to_string(),
            ChoiceQuestion::new(
                "Which safe action best advances the current goal?",
                BTreeMap::from([
                    (
                        "attack_mob_17".to_string(),
                        Some("Defend against the nearby hostile mob".to_string()),
                    ),
                    (
                        "wait".to_string(),
                        Some("Take no action and observe again".to_string()),
                    ),
                ]),
            ),
        )])
    }

    #[test]
    fn serializes_official_choice_request_shape() {
        let questions = questions();
        let state = json!({
            "health": 14,
            "goal": "survive",
            "nearby_hostiles": [{"id": 17, "distance": 2.4}]
        });
        let request = SystemOneRequest {
            state: &state,
            model: "jev-latest",
            questions: &questions,
        };

        assert_eq!(
            serde_json::to_value(request).expect("request should serialize"),
            json!({
                "state": {
                    "health": 14,
                    "goal": "survive",
                    "nearby_hostiles": [{"id": 17, "distance": 2.4}]
                },
                "model": "jev-latest",
                "questions": {
                    "next_action": {
                        "type": "choice",
                        "instructions": "Which safe action best advances the current goal?",
                        "criteria": {
                            "attack_mob_17": "Defend against the nearby hostile mob",
                            "wait": "Take no action and observe again"
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn parses_and_validates_choice_response() {
        let body = json!({
            "model": "jev-1.13.0",
            "answers": {
                "next_action": {
                    "type": "choice",
                    "choice": "attack_mob_17",
                    "probabilities": {
                        "attack_mob_17": 0.82,
                        "wait": 0.18
                    },
                    "confidence": 0.71
                }
            },
            "usage": {"input_tokens": 246, "output_tokens": 32}
        })
        .to_string();

        let response = decode_response(StatusCode::OK, &body, &questions())
            .expect("valid response should parse");
        let answer = &response.answers["next_action"];
        assert_eq!(answer.choice, "attack_mob_17");
        assert_eq!(answer.selected_probability(), Some(0.82));
        assert_eq!(answer.confidence, 0.71);
        assert_eq!(response.usage.input_tokens, 246);
    }

    #[test]
    #[ignore = "requires permission to bind a loopback TCP socket"]
    fn client_sends_bearer_auth_and_official_request_over_http() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("mock server address");
        let (request_tx, request_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set read timeout");
            let request = read_http_request(&mut stream);
            request_tx.send(request).expect("capture request");
            let body = json!({
                "model": "jev-latest",
                "answers": {
                    "next_action": {
                        "type": "choice",
                        "choice": "wait",
                        "probabilities": {
                            "attack_mob_17": 0.1,
                            "wait": 0.9
                        },
                        "confidence": 0.8
                    }
                },
                "usage": {"input_tokens": 12, "output_tokens": 3}
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let endpoint = Url::parse(&format!("http://{address}/v1/systemone"))
            .expect("mock endpoint URL");
        let config = JevConfig::new("test-secret")
            .unwrap()
            .with_endpoint(endpoint)
            .unwrap();
        let client = JevClient::new(config).unwrap();

        let response = client
            .evaluate_choices(&json!({"health": 20}), &questions())
            .expect("mock evaluation");
        let request = request_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("captured request");
        server.join().expect("mock server thread");

        let header_end = find_bytes(&request, b"\r\n\r\n").expect("HTTP headers") + 4;
        let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
        assert!(headers.starts_with("post /v1/systemone http/1.1"));
        assert!(headers.contains("authorization: bearer test-secret"));
        let body: Value = serde_json::from_slice(&request[header_end..]).expect("request JSON");
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["state"]["health"], 20);
        assert_eq!(body["questions"]["next_action"]["type"], "choice");
        assert_eq!(response.answers["next_action"].choice, "wait");
    }

    #[test]
    fn rejects_choice_that_was_not_offered() {
        let body = json!({
            "model": "jev-latest",
            "answers": {
                "next_action": {
                    "type": "choice",
                    "choice": "teleport",
                    "probabilities": {"attack_mob_17": 0.4, "wait": 0.6},
                    "confidence": 0.2
                }
            },
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
        .to_string();

        let error = decode_response(StatusCode::OK, &body, &questions())
            .expect_err("unknown choice should be rejected");
        assert!(error.to_string().contains("unknown option 'teleport'"));
    }

    #[test]
    fn includes_status_and_bounded_body_in_http_errors() {
        let body = format!("validation failed: {}", "x".repeat(ERROR_BODY_LIMIT + 100));
        let error = decode_response(StatusCode::UNPROCESSABLE_ENTITY, &body, &questions())
            .expect_err("non-success status should fail");
        let message = error.to_string();
        assert!(message.contains("422 Unprocessable Entity"));
        assert!(message.contains("validation failed"));
        assert!(message.contains("<truncated>"));
        assert!(message.len() < body.len());
    }

    #[test]
    fn config_debug_output_redacts_api_key() {
        let config = JevConfig::new("super-secret-key").expect("valid config");
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret-key"));
    }

    #[test]
    fn rejects_non_document_state_shape_before_networking() {
        assert!(matches!(
            validate_state(&json!(42)),
            Err(JevError::InvalidRequest(_))
        ));
    }

    #[test]
    fn retries_only_rate_limit_and_overload_statuses() {
        let headers = HeaderMap::new();
        assert_eq!(
            rate_limit_retry_delay(StatusCode::TOO_MANY_REQUESTS, &headers, 0),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            rate_limit_retry_delay(StatusCode::from_u16(529).unwrap(), &headers, 1),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            rate_limit_retry_delay(StatusCode::UNAUTHORIZED, &headers, 0),
            None
        );
    }

    fn read_http_request(stream: &mut impl Read) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut expected_length = None;
        loop {
            let read = stream.read(&mut buffer).expect("read request");
            assert!(read > 0, "request ended before its body was complete");
            request.extend_from_slice(&buffer[..read]);
            if expected_length.is_none() {
                if let Some(header_index) = find_bytes(&request, b"\r\n\r\n") {
                    let header_end = header_index + 4;
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .expect("content-length header");
                    expected_length = Some(header_end + content_length);
                }
            }
            if expected_length.is_some_and(|length| request.len() >= length) {
                request.truncate(expected_length.unwrap());
                return request;
            }
        }
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
