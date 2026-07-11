//! [`ApiJudgeProvider`]: a [`Provider`] that talks to a model **API directly** —
//! Anthropic Messages or OpenAI Chat Completions — with no harness in between.
//! Useful as a cheap, harness-free judge and simulated user (compose it with a
//! skill-running provider via [`SplitProvider`](crate::SplitProvider)), or as a
//! standalone text provider.
//!
//! The network is a seam: [`ApiJudgeProvider`] is generic over an
//! [`HttpTransport`], and everything above the wire — building each vendor's
//! request, parsing its response, extracting the verdict — is pure and
//! deterministically unit-tested against a fake transport. The bundled
//! `UreqTransport` (a blocking rustls client) is gated behind the optional
//! `ureq-transport` feature so the core crate carries no TLS stack; a consumer can
//! instead implement [`HttpTransport`] over whatever HTTP client they already use.

use serde_json::{json, Value};

use crate::error::{Error, ProviderErrorKind, Result};
use crate::provider::{
    build_judge_prompt, build_user_prompt, parse_verdict, AssistantTurn, JudgeQuery, JudgeVerdict,
    Provider, SkillRef, UserTurn,
};
use crate::transcript::{Message, Role};
use crate::usage::Usage;

// ---------------------------------------------------------------------------
// The transport seam
// ---------------------------------------------------------------------------

/// A completed HTTP response: the status code and the raw response body. A
/// non-2xx status is a *completed* response (returned here), not a transport
/// error — [`ApiJudgeProvider`] classifies it into a
/// [`ProviderErrorKind`](crate::ProviderErrorKind).
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The raw response body.
    pub body: String,
}

/// A transport-level failure — the request never produced an HTTP response
/// (could not connect, timed out, TLS error). Distinct from a non-2xx status,
/// which is a completed [`HttpResponse`].
#[derive(Debug)]
pub struct HttpError {
    /// Human-readable specifics.
    pub message: String,
    /// Whether the failure was a timeout, so the provider can classify it as
    /// [`ProviderErrorKind::Timeout`](crate::ProviderErrorKind::Timeout).
    pub timeout: bool,
}

impl HttpError {
    /// A non-timeout transport failure with `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timeout: false,
        }
    }

    /// A timeout transport failure with `message`.
    #[must_use]
    pub fn timed_out(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            timeout: true,
        }
    }
}

/// How [`ApiJudgeProvider`] sends a request. Implement this over any HTTP client
/// (or use the bundled `UreqTransport` via the `ureq-transport` feature).
pub trait HttpTransport {
    /// POST `body` (already-serialized JSON) to `url` with `headers`. Return the
    /// response status and body for **any** completed request — including 4xx /
    /// 5xx — and only [`Err`] on a transport-level failure.
    ///
    /// # Errors
    /// [`HttpError`] when the request never produced an HTTP response.
    fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> std::result::Result<HttpResponse, HttpError>;
}

// ---------------------------------------------------------------------------
// Vendors
// ---------------------------------------------------------------------------

/// Which model API [`ApiJudgeProvider`] speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiVendor {
    /// Anthropic Messages API (`POST /v1/messages`).
    Anthropic,
    /// OpenAI Chat Completions API (`POST /v1/chat/completions`).
    OpenAI,
}

impl ApiVendor {
    /// The default API base URL for this vendor.
    #[must_use]
    fn default_base_url(self) -> &'static str {
        match self {
            ApiVendor::Anthropic => "https://api.anthropic.com",
            ApiVendor::OpenAI => "https://api.openai.com",
        }
    }

    /// A human label used in error messages.
    fn label(self) -> &'static str {
        match self {
            ApiVendor::Anthropic => "anthropic",
            ApiVendor::OpenAI => "openai",
        }
    }
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// A [`Provider`] backed by a direct model API. Generic over the
/// [`HttpTransport`] that performs the request.
pub struct ApiJudgeProvider<T> {
    transport: T,
    vendor: ApiVendor,
    api_key: String,
    base_url: String,
    max_tokens: u32,
}

/// The internal, vendor-agnostic shape of one chat request.
struct Chat {
    /// System framing (Anthropic `system` / an OpenAI `system` message).
    system: Option<String>,
    /// The conversation turns, each `(role, content)` with `role` in
    /// `{"user", "assistant"}`.
    messages: Vec<(&'static str, String)>,
    /// Ask the vendor to return a JSON object (OpenAI `response_format`).
    json_object: bool,
}

impl<T: HttpTransport> ApiJudgeProvider<T> {
    /// A provider talking to `vendor` with `api_key` over `transport`. The base
    /// URL defaults to the vendor's public endpoint and `max_tokens` to 1024;
    /// override either with the builders.
    pub fn new(vendor: ApiVendor, api_key: impl Into<String>, transport: T) -> Self {
        Self {
            transport,
            vendor,
            api_key: api_key.into(),
            base_url: vendor.default_base_url().to_string(),
            max_tokens: 1024,
        }
    }

    /// Override the API base URL (e.g. a proxy, a gateway, or a test server).
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the `max_tokens` cap sent on each completion (default 1024).
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// The vendor this provider speaks to.
    #[must_use]
    pub fn vendor(&self) -> ApiVendor {
        self.vendor
    }

    /// The API base URL in effect.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // --- request construction -------------------------------------------------

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.vendor {
            ApiVendor::Anthropic => format!("{base}/v1/messages"),
            ApiVendor::OpenAI => format!("{base}/v1/chat/completions"),
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
        match self.vendor {
            ApiVendor::Anthropic => {
                headers.push(("x-api-key".to_string(), self.api_key.clone()));
                headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
            }
            ApiVendor::OpenAI => {
                headers.push((
                    "authorization".to_string(),
                    format!("Bearer {}", self.api_key),
                ));
            }
        }
        headers
    }

    fn body(&self, model: &str, chat: &Chat) -> Value {
        match self.vendor {
            ApiVendor::Anthropic => {
                let messages: Vec<Value> = chat
                    .messages
                    .iter()
                    .map(|(role, content)| json!({"role": role, "content": content}))
                    .collect();
                let mut body = json!({
                    "model": model,
                    "max_tokens": self.max_tokens,
                    "messages": messages,
                });
                if let Some(system) = &chat.system {
                    body["system"] = json!(system);
                }
                body
            }
            ApiVendor::OpenAI => {
                let mut messages: Vec<Value> = Vec::new();
                if let Some(system) = &chat.system {
                    messages.push(json!({"role": "system", "content": system}));
                }
                for (role, content) in &chat.messages {
                    messages.push(json!({"role": role, "content": content}));
                }
                let mut body = json!({ "model": model, "messages": messages });
                if chat.json_object {
                    body["response_format"] = json!({"type": "json_object"});
                }
                body
            }
        }
    }

    /// Send one chat request and return the reply text plus any usage.
    fn call(&self, op: &str, model: &str, chat: &Chat) -> Result<(String, Option<Usage>)> {
        let url = self.endpoint();
        let owned_headers = self.headers();
        let header_refs: Vec<(&str, &str)> = owned_headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let body = serde_json::to_string(&self.body(model, chat))
            .map_err(|e| Error::provider(op, format!("could not encode request: {e}")))?;

        let response = self
            .transport
            .post_json(&url, &header_refs, &body)
            .map_err(|e| {
                let kind = if e.timeout {
                    ProviderErrorKind::Timeout
                } else {
                    ProviderErrorKind::Other
                };
                Error::provider_classified(
                    op,
                    format!("HTTP transport failed: {}", e.message),
                    kind,
                )
            })?;

        if !(200..300).contains(&response.status) {
            return Err(Error::provider_classified(
                op,
                format!(
                    "{} API returned {}: {}",
                    self.vendor.label(),
                    response.status,
                    error_message(&response.body)
                ),
                classify_status(response.status),
            ));
        }

        let value: Value = serde_json::from_str(&response.body).map_err(|e| {
            Error::provider_classified(
                op,
                format!("{} response was not valid JSON: {e}", self.vendor.label()),
                ProviderErrorKind::Protocol,
            )
        })?;
        self.extract(op, &value)
    }

    /// Pull the reply text and usage out of a successful vendor response.
    fn extract(&self, op: &str, value: &Value) -> Result<(String, Option<Usage>)> {
        let (text, usage) = match self.vendor {
            ApiVendor::Anthropic => {
                let text = value
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                            .filter_map(|b| b.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                let usage = value.get("usage").and_then(|u| {
                    make_usage(
                        u.get("input_tokens").and_then(Value::as_u64),
                        u.get("output_tokens").and_then(Value::as_u64),
                    )
                });
                (text, usage)
            }
            ApiVendor::OpenAI => {
                let text = value
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|c| c.first())
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let usage = value.get("usage").and_then(|u| {
                    make_usage(
                        u.get("prompt_tokens").and_then(Value::as_u64),
                        u.get("completion_tokens").and_then(Value::as_u64),
                    )
                });
                (text, usage)
            }
        };

        if text.is_empty() {
            return Err(Error::provider_classified(
                op,
                format!(
                    "{} response carried no reply text; got: {value}",
                    self.vendor.label()
                ),
                ProviderErrorKind::Protocol,
            ));
        }
        Ok((text, usage))
    }
}

/// Map an API message role onto the wire role the vendors accept: everything that
/// is not an assistant turn is sent as a user turn (the engine only produces
/// user/assistant; a stray system turn folds harmlessly into user content).
fn wire_role(role: Role) -> &'static str {
    match role {
        Role::Assistant => "assistant",
        Role::User | Role::System => "user",
    }
}

fn to_chat_messages(messages: &[Message]) -> Vec<(&'static str, String)> {
    messages
        .iter()
        .map(|m| (wire_role(m.role), m.content.clone()))
        .collect()
}

/// Build a [`Usage`] from the two token counts, or `None` if neither is present.
fn make_usage(input_tokens: Option<u64>, output_tokens: Option<u64>) -> Option<Usage> {
    let usage = Usage {
        input_tokens,
        output_tokens,
        cost_usd: None,
    };
    (!usage.is_empty()).then_some(usage)
}

/// Classify an HTTP status into a [`ProviderErrorKind`].
fn classify_status(status: u16) -> ProviderErrorKind {
    match status {
        401 | 403 => ProviderErrorKind::Auth,
        402 => ProviderErrorKind::Quota,
        404 => ProviderErrorKind::ModelNotFound,
        429 => ProviderErrorKind::RateLimit,
        400 => ProviderErrorKind::Protocol,
        500..=599 => ProviderErrorKind::Overloaded,
        _ => ProviderErrorKind::Other,
    }
}

/// Extract a human-readable message from an error body (`{"error":{"message":..}}`
/// for both vendors), falling back to a truncated view of the raw body.
fn error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(message) = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            return message.to_string();
        }
    }
    let trimmed = body.trim();
    trimmed.chars().take(300).collect()
}

impl<T: HttpTransport> Provider for ApiJudgeProvider<T> {
    fn respond(
        &self,
        _platform: &str,
        model: &str,
        skill: &SkillRef<'_>,
        messages: &[Message],
        _session: Option<&str>,
    ) -> Result<AssistantTurn> {
        // Stateless: no native session, so the whole transcript is re-sent as the
        // message array and the skill instructions go in as the system prompt.
        let chat = Chat {
            system: Some(skill.instructions.to_string()),
            messages: to_chat_messages(messages),
            json_object: false,
        };
        let (message, usage) = self.call("respond", model, &chat)?;
        Ok(AssistantTurn {
            message,
            done: false,
            usage,
            events: Vec::new(),
        })
    }

    fn simulate_user(
        &self,
        model: &str,
        persona: &str,
        messages: &[Message],
        _session: Option<&str>,
    ) -> Result<UserTurn> {
        let chat = Chat {
            system: None,
            messages: vec![("user", build_user_prompt(persona, messages))],
            json_object: false,
        };
        let (message, usage) = self.call("user", model, &chat)?;
        Ok(UserTurn {
            message,
            stop: false,
            usage,
        })
    }

    fn judge(
        &self,
        model: &str,
        query: &JudgeQuery<'_>,
        messages: &[Message],
    ) -> Result<JudgeVerdict> {
        let chat = Chat {
            system: None,
            messages: vec![("user", build_judge_prompt(query, messages))],
            json_object: true,
        };
        let (text, usage) = self.call("judge", model, &chat)?;
        let mut verdict = parse_verdict(query.kind, "api:judge", &text)?;
        verdict.usage = usage;
        Ok(verdict)
    }
}

// ---------------------------------------------------------------------------
// The bundled ureq transport (optional `ureq-transport` feature)
// ---------------------------------------------------------------------------

/// A blocking, rustls-backed [`HttpTransport`] built on `ureq` — the batteries-
/// included client behind the `ureq-transport` feature, so a consumer can reach
/// Anthropic / OpenAI with no extra wiring.
#[cfg(feature = "ureq-transport")]
#[derive(Debug, Clone, Copy)]
pub struct UreqTransport;

#[cfg(feature = "ureq-transport")]
impl Default for UreqTransport {
    fn default() -> Self {
        Self
    }
}

#[cfg(feature = "ureq-transport")]
impl UreqTransport {
    /// A new transport.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "ureq-transport")]
impl HttpTransport for UreqTransport {
    fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> std::result::Result<HttpResponse, HttpError> {
        // Treat a non-2xx as a normal response (not an error) so the provider can
        // classify it uniformly.
        let mut request = ureq::post(url).config().http_status_as_error(false).build();
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        match request.send(body) {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let body = response
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| HttpError::new(format!("could not read response body: {e}")))?;
                Ok(HttpResponse { status, body })
            }
            Err(ureq::Error::Timeout(_)) => Err(HttpError::timed_out("request timed out")),
            Err(e) => Err(HttpError::new(e.to_string())),
        }
    }
}

#[cfg(feature = "ureq-transport")]
impl ApiJudgeProvider<UreqTransport> {
    /// A provider that talks to Anthropic over the bundled [`UreqTransport`].
    #[must_use]
    pub fn anthropic(api_key: impl Into<String>) -> Self {
        Self::new(ApiVendor::Anthropic, api_key, UreqTransport::new())
    }

    /// A provider that talks to OpenAI over the bundled [`UreqTransport`].
    #[must_use]
    pub fn openai(api_key: impl Into<String>) -> Self {
        Self::new(ApiVendor::OpenAI, api_key, UreqTransport::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{JudgeKind, JudgeValue};
    use std::cell::RefCell;

    /// A deterministic transport: returns a preset status/body (or a transport
    /// failure) and records the request it was handed, so tests can assert both
    /// the request shape and the response handling — offline, no HTTP.
    struct FakeTransport {
        status: u16,
        body: String,
        fail: Option<HttpError>,
        seen: RefCell<Option<Seen>>,
    }

    struct Seen {
        url: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl FakeTransport {
        fn ok(body: &str) -> Self {
            Self::with_status(200, body)
        }

        fn with_status(status: u16, body: &str) -> Self {
            Self {
                status,
                body: body.to_string(),
                fail: None,
                seen: RefCell::new(None),
            }
        }

        fn failing(error: HttpError) -> Self {
            Self {
                status: 0,
                body: String::new(),
                fail: Some(error),
                seen: RefCell::new(None),
            }
        }

        fn seen(&self) -> std::cell::Ref<'_, Option<Seen>> {
            self.seen.borrow()
        }
    }

    impl HttpTransport for FakeTransport {
        fn post_json(
            &self,
            url: &str,
            headers: &[(&str, &str)],
            body: &str,
        ) -> std::result::Result<HttpResponse, HttpError> {
            *self.seen.borrow_mut() = Some(Seen {
                url: url.to_string(),
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
                body: body.to_string(),
            });
            if let Some(err) = &self.fail {
                return Err(HttpError {
                    message: err.message.clone(),
                    timeout: err.timeout,
                });
            }
            Ok(HttpResponse {
                status: self.status,
                body: self.body.clone(),
            })
        }
    }

    fn anthropic(transport: FakeTransport) -> ApiJudgeProvider<FakeTransport> {
        ApiJudgeProvider::new(ApiVendor::Anthropic, "sk-ant", transport)
    }

    fn openai(transport: FakeTransport) -> ApiJudgeProvider<FakeTransport> {
        ApiJudgeProvider::new(ApiVendor::OpenAI, "sk-oai", transport)
    }

    fn skill_ref() -> SkillRef<'static> {
        SkillRef {
            name: "s",
            dir: "/s",
            instructions: "Be a terse assistant.",
        }
    }

    fn boolean_query() -> JudgeQuery<'static> {
        JudgeQuery {
            kind: JudgeKind::Boolean,
            criterion: "the reply was polite",
            scale: None,
        }
    }

    fn numeric_query() -> JudgeQuery<'static> {
        JudgeQuery {
            kind: JudgeKind::Numeric,
            criterion: "warmth",
            scale: Some((1.0, 5.0)),
        }
    }

    fn has_header(seen: &Seen, key: &str, value: &str) -> bool {
        seen.headers.iter().any(|(k, v)| k == key && v == value)
    }

    #[test]
    fn defaults_and_builders_configure_vendor_and_base_url() {
        let a = anthropic(FakeTransport::ok("{}"));
        assert_eq!(a.vendor(), ApiVendor::Anthropic);
        assert_eq!(a.base_url(), "https://api.anthropic.com");
        assert_eq!(
            openai(FakeTransport::ok("{}")).base_url(),
            "https://api.openai.com"
        );
        let rebased = openai(FakeTransport::ok("{}"))
            .with_max_tokens(64)
            .with_base_url("http://proxy.local/");
        assert_eq!(rebased.base_url(), "http://proxy.local/");
        // The trailing slash is trimmed when the endpoint is built.
        assert!(rebased.endpoint().ends_with("/v1/chat/completions"));
        assert!(!rebased.endpoint().contains("//v1"));
    }

    #[test]
    fn anthropic_respond_builds_request_and_parses_reply() {
        let provider = anthropic(FakeTransport::ok(
            r#"{"content":[{"type":"text","text":"pong"}],"usage":{"input_tokens":7,"output_tokens":1}}"#,
        ));
        let turn = provider
            .respond(
                "claude-code",
                "claude-x",
                &skill_ref(),
                &[Message::user("ping")],
                None,
            )
            .unwrap();
        assert_eq!(turn.message, "pong");
        assert_eq!(turn.usage.unwrap().input_tokens, Some(7));
        assert!(turn.events.is_empty());

        let seen = provider.transport.seen();
        let seen = seen.as_ref().unwrap();
        assert!(seen.url.ends_with("/v1/messages"));
        assert!(has_header(seen, "x-api-key", "sk-ant"));
        assert!(has_header(seen, "anthropic-version", "2023-06-01"));
        assert!(seen.body.contains("\"system\":\"Be a terse assistant.\""));
        assert!(seen.body.contains("\"content\":\"ping\""));
    }

    #[test]
    fn openai_respond_builds_request_and_parses_reply() {
        let provider = openai(FakeTransport::ok(
            r#"{"choices":[{"message":{"content":"hi there"}}],"usage":{"prompt_tokens":4,"completion_tokens":2}}"#,
        ));
        // A prior assistant turn exercises the assistant-role mapping too.
        let turn = provider
            .respond(
                "x",
                "gpt-x",
                &skill_ref(),
                &[
                    Message::user("hi"),
                    Message::assistant("earlier reply"),
                    Message::user("again"),
                ],
                None,
            )
            .unwrap();
        assert_eq!(turn.message, "hi there");
        let usage = turn.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(4));
        assert_eq!(usage.output_tokens, Some(2));

        let seen = provider.transport.seen();
        let seen = seen.as_ref().unwrap();
        assert!(seen.url.ends_with("/v1/chat/completions"));
        assert!(has_header(seen, "authorization", "Bearer sk-oai"));
        assert!(seen.body.contains("\"role\":\"system\""));
        assert!(seen.body.contains("\"role\":\"assistant\""));
    }

    #[test]
    fn openai_error_status_is_classified_with_its_label() {
        let provider = openai(FakeTransport::with_status(
            500,
            r#"{"error":{"message":"boom"}}"#,
        ));
        let err = provider.judge("m", &boolean_query(), &[]).unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Overloaded));
        assert!(err.to_string().contains("openai API returned 500"));
    }

    #[test]
    fn anthropic_judge_parses_boolean_verdict() {
        let provider = anthropic(FakeTransport::ok(
            r#"{"content":[{"type":"text","text":"{\"value\": true, \"reason\": \"courteous\"}"}]}"#,
        ));
        let verdict = provider.judge("m", &boolean_query(), &[]).unwrap();
        assert_eq!(verdict.value, JudgeValue::Bool(true));
        assert_eq!(verdict.reason, "courteous");
    }

    #[test]
    fn openai_judge_requests_json_object_and_parses_numeric() {
        let provider = openai(FakeTransport::ok(
            r#"{"choices":[{"message":{"content":"{\"value\": 4, \"reason\": \"warm\"}"}}]}"#,
        ));
        let verdict = provider.judge("m", &numeric_query(), &[]).unwrap();
        assert_eq!(verdict.value, JudgeValue::Number(4.0));
        let seen = provider.transport.seen();
        assert!(seen
            .as_ref()
            .unwrap()
            .body
            .contains("\"response_format\":{\"type\":\"json_object\"}"));
    }

    #[test]
    fn simulate_user_sends_the_persona_and_reads_the_reply() {
        let provider = anthropic(FakeTransport::ok(
            r#"{"content":[{"type":"text","text":"And by Friday?"}]}"#,
        ));
        let turn = provider
            .simulate_user(
                "m",
                "A hurried shopper.",
                &[Message::assistant("Sure.")],
                None,
            )
            .unwrap();
        assert_eq!(turn.message, "And by Friday?");
        assert!(!turn.stop);
        let seen = provider.transport.seen();
        assert!(seen.as_ref().unwrap().body.contains("A hurried shopper."));
    }

    #[test]
    fn http_error_statuses_classify() {
        let cases = [
            (401, ProviderErrorKind::Auth),
            (403, ProviderErrorKind::Auth),
            (402, ProviderErrorKind::Quota),
            (404, ProviderErrorKind::ModelNotFound),
            (429, ProviderErrorKind::RateLimit),
            (400, ProviderErrorKind::Protocol),
            (503, ProviderErrorKind::Overloaded),
            (418, ProviderErrorKind::Other),
        ];
        for (status, expected) in cases {
            let provider = anthropic(FakeTransport::with_status(
                status,
                r#"{"error":{"message":"nope"}}"#,
            ));
            let err = provider.judge("m", &boolean_query(), &[]).unwrap_err();
            assert_eq!(err.kind(), Some(expected), "status {status}");
            assert!(err.to_string().contains("nope"));
        }
    }

    #[test]
    fn transport_failures_classify_timeout_vs_other() {
        let timed_out = anthropic(FakeTransport::failing(HttpError::timed_out("slow")));
        assert_eq!(
            timed_out
                .judge("m", &boolean_query(), &[])
                .unwrap_err()
                .kind(),
            Some(ProviderErrorKind::Timeout)
        );
        let refused = anthropic(FakeTransport::failing(HttpError::new("connection refused")));
        assert_eq!(
            refused
                .judge("m", &boolean_query(), &[])
                .unwrap_err()
                .kind(),
            Some(ProviderErrorKind::Other)
        );
    }

    #[test]
    fn malformed_and_empty_responses_are_protocol_errors() {
        let not_json = anthropic(FakeTransport::ok("not json"));
        assert_eq!(
            not_json
                .respond("x", "m", &skill_ref(), &[Message::user("hi")], None)
                .unwrap_err()
                .kind(),
            Some(ProviderErrorKind::Protocol)
        );
        let no_text = anthropic(FakeTransport::ok(r#"{"content":[]}"#));
        assert_eq!(
            no_text
                .respond("x", "m", &skill_ref(), &[Message::user("hi")], None)
                .unwrap_err()
                .kind(),
            Some(ProviderErrorKind::Protocol)
        );
        let no_choices = openai(FakeTransport::ok(r#"{"choices":[]}"#));
        assert_eq!(
            no_choices
                .respond("x", "m", &skill_ref(), &[Message::user("hi")], None)
                .unwrap_err()
                .kind(),
            Some(ProviderErrorKind::Protocol)
        );
    }

    #[test]
    fn error_message_falls_back_to_raw_body() {
        // No `error.message` field: the raw (truncated) body is surfaced.
        assert_eq!(error_message("plain text failure"), "plain text failure");
        assert_eq!(error_message(r#"{"error":{"message":"bad"}}"#), "bad");
    }

    #[test]
    fn api_provider_is_not_session_capable() {
        let provider = anthropic(FakeTransport::ok("{}"));
        assert!(!provider.session_capable("claude-code"));
    }
}

// The bundled ureq transport, proven over a REAL local socket (no TLS: plain
// http to 127.0.0.1) so the transport glue is exercised without a live API. Runs
// only when the `ureq-transport` feature is built (CI's `http` tier); the
// deterministic gate covers the provider logic above via `FakeTransport`.
#[cfg(all(test, feature = "ureq-transport"))]
mod ureq_socket_tests {
    use super::*;
    use crate::provider::{JudgeKind, JudgeValue};
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;

    /// Serve exactly one HTTP response on a fresh localhost port, returning the
    /// base URL and the server thread's handle.
    fn serve_once(
        status_line: &'static str,
        body: &'static str,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn ureq_transport_judges_over_a_real_socket() {
        let (base, handle) = serve_once(
            "HTTP/1.1 200 OK",
            r#"{"content":[{"type":"text","text":"{\"value\": true, \"reason\": \"ok\"}"}],"usage":{"input_tokens":5,"output_tokens":2}}"#,
        );
        let provider = ApiJudgeProvider::anthropic("test-key").with_base_url(base);
        let verdict = provider
            .judge(
                "claude-x",
                &JudgeQuery {
                    kind: JudgeKind::Boolean,
                    criterion: "ok",
                    scale: None,
                },
                &[],
            )
            .unwrap();
        assert_eq!(verdict.value, JudgeValue::Bool(true));
        assert_eq!(verdict.usage.unwrap().input_tokens, Some(5));
        handle.join().unwrap();
    }

    #[test]
    fn ureq_transport_surfaces_a_non_2xx_status() {
        let (base, handle) = serve_once(
            "HTTP/1.1 401 Unauthorized",
            r#"{"error":{"message":"bad key"}}"#,
        );
        let provider = ApiJudgeProvider::anthropic("bad").with_base_url(base);
        let err = provider
            .respond(
                "x",
                "m",
                &SkillRef {
                    name: "s",
                    dir: "/s",
                    instructions: "hi",
                },
                &[Message::user("hi")],
                None,
            )
            .unwrap_err();
        assert_eq!(err.kind(), Some(ProviderErrorKind::Auth));
        assert!(err.to_string().contains("bad key"));
        handle.join().unwrap();
    }
}
