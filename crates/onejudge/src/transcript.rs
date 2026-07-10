//! The conversation model: the transcript that flows between the engine and the
//! provider and is ultimately judged, plus the tool-event query primitive that
//! lets a consumer assert on *what the skill did*, not just what it said.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The (real or simulated) user driving the skill.
    User,
    /// The skill / assistant under test.
    Assistant,
    /// System-level framing, if a provider surfaces it.
    System,
}

impl Role {
    /// The capitalized label used when rendering a transcript into a prompt.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        }
    }
}

/// One normalized tool-call / action event the skill took during a turn, lifted
/// from `oneharness`'s `events` array (its `--events` output). Harness-agnostic,
/// so a consumer can inspect shell commands, file edits, and tool uses across any
/// harness, not just the final text. `input` is the structured, tool-shaped args
/// so a consumer can match on the command string or file path without re-parsing.
///
/// `input` is free-form JSON, so `Message`/`Transcript` are `PartialEq` but not
/// `Eq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolEvent {
    /// `tool_call` (the skill invoked a tool) or `tool_result` (the observation).
    pub kind: String,
    /// Normalized tool name where knowable (e.g. `bash`, `edit_file`); `None` for
    /// a `tool_result` or when the harness did not name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Structured tool arguments (the command, the file path); `None` when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// The result/observation text, when the transcript exposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Position within the run, so ordering ("did X before Y") is expressible.
    #[serde(default)]
    pub index: usize,
}

impl ToolEvent {
    /// A compact one-line summary for inlining into a judge prompt: the name and
    /// a truncated view of the structured input, never the raw output (which can
    /// be arbitrarily large). Keeps the judge token-budget-aware while still
    /// letting it reason over *what ran*.
    #[must_use]
    pub fn summary(&self) -> String {
        let name = self.name.as_deref().unwrap_or(&self.kind);
        match &self.input {
            Some(input) => {
                let mut rendered = compact_json(input);
                truncate(&mut rendered, 200);
                format!("{name}({rendered})")
            }
            None => name.to_string(),
        }
    }
}

/// A single turn in the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who produced the turn.
    pub role: Role,
    /// The turn's text.
    pub content: String,
    /// The normalized tool events the skill took producing this turn (assistant
    /// turns only, and only when the harness exposed a tool transcript). Empty
    /// otherwise. Surfaced for post-hoc analysis, streamed live, and rendered
    /// into the transcript the judge sees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<ToolEvent>,
}

impl Message {
    /// Build a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            events: Vec::new(),
        }
    }

    /// Build an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            events: Vec::new(),
        }
    }

    /// Attach the turn's normalized tool events (builder style).
    #[must_use]
    pub fn with_events(mut self, events: Vec<ToolEvent>) -> Self {
        self.events = events;
        self
    }
}

/// An ordered list of messages.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// The turns in order, oldest first.
    pub messages: Vec<Message>,
}

impl Transcript {
    /// Start a transcript from the initial user input given to the skill.
    pub fn from_input(input: impl Into<String>) -> Self {
        Self {
            messages: vec![Message::user(input)],
        }
    }

    /// Append a message.
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Number of assistant turns produced so far.
    #[must_use]
    pub fn assistant_turns(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count()
    }

    /// Every tool event across every turn, in order.
    pub fn tool_events(&self) -> impl Iterator<Item = &ToolEvent> {
        self.messages.iter().flat_map(|m| m.events.iter())
    }

    /// How many tool events across the transcript match `query`. The primitive a
    /// `did` / `did_not` / `tool_call` assertion is built on — it reads the
    /// `events` array directly, no judge call or mock/spy setup required.
    #[must_use]
    pub fn count_tool_events(&self, query: &ToolQuery) -> usize {
        self.tool_events().filter(|e| query.matches(e)).count()
    }

    /// Whether the skill took at least one tool event matching `query`.
    #[must_use]
    pub fn did(&self, query: &ToolQuery) -> bool {
        self.count_tool_events(query) > 0
    }
}

/// A declarative match over the transcript's tool events. Every set field must
/// hold for an event to match; an all-default query matches every `tool_call`.
///
/// This is the reusable primitive behind an events-backed assertion: a consumer
/// (or skilltest's `did` / `did_not` evals) constructs a query and asks the
/// [`Transcript`] whether it matched — without a judge call, a mock, or a spy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolQuery {
    /// Require this exact normalized tool name (e.g. `bash`).
    pub name: Option<String>,
    /// Require the compact-JSON rendering of the event's `input` to contain this
    /// substring (e.g. a command fragment like `git commit`).
    pub input_contains: Option<String>,
    /// Match `tool_result` events too; by default only `tool_call`s match, since
    /// an assertion about behavior is about what the skill *invoked*.
    pub include_results: bool,
}

impl ToolQuery {
    /// A query for a specific tool name.
    pub fn tool(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..Self::default()
        }
    }

    /// Narrow to events whose structured input contains `needle`.
    #[must_use]
    pub fn with_input_contains(mut self, needle: impl Into<String>) -> Self {
        self.input_contains = Some(needle.into());
        self
    }

    /// Whether `event` satisfies every set field of this query.
    #[must_use]
    pub fn matches(&self, event: &ToolEvent) -> bool {
        if !self.include_results && event.kind != "tool_call" {
            return false;
        }
        if let Some(name) = &self.name {
            if event.name.as_deref() != Some(name.as_str()) {
                return false;
            }
        }
        if let Some(needle) = &self.input_contains {
            let haystack = event.input.as_ref().map(compact_json).unwrap_or_default();
            if !haystack.contains(needle) {
                return false;
            }
        }
        true
    }
}

/// Serialize a JSON value compactly (no pretty whitespace), for matching and
/// summaries. Falls back to the debug form on the (unreachable for valid
/// `Value`) serialize error.
fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
}

/// Truncate `s` in place to at most `max` chars, appending an ellipsis marker.
fn truncate(s: &mut String, max: usize) {
    if s.chars().count() > max {
        let cut: String = s.chars().take(max).collect();
        *s = format!("{cut}…");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, input: Value) -> ToolEvent {
        ToolEvent {
            kind: "tool_call".into(),
            name: Some(name.into()),
            input: Some(input),
            output: None,
            index: 0,
        }
    }

    #[test]
    fn builders_and_turn_count() {
        let mut t = Transcript::from_input("hi");
        t.push(
            Message::assistant("hello").with_events(vec![call("bash", json!({"command": "ls"}))]),
        );
        t.push(Message::user("again"));
        t.push(Message::assistant("done"));
        assert_eq!(t.assistant_turns(), 2);
        assert_eq!(t.tool_events().count(), 1);
    }

    #[test]
    fn tool_query_matches_name_and_input() {
        let mut t = Transcript::default();
        t.push(Message::assistant("committing").with_events(vec![
            call("bash", json!({"command": "git commit -m x"})),
            call("bash", json!({"command": "ls"})),
        ]));
        assert_eq!(t.count_tool_events(&ToolQuery::tool("bash")), 2);
        assert!(t.did(&ToolQuery::tool("bash").with_input_contains("git commit")));
        assert_eq!(
            t.count_tool_events(&ToolQuery::tool("bash").with_input_contains("git commit")),
            1
        );
        assert!(!t.did(&ToolQuery::tool("edit_file")));
    }

    #[test]
    fn results_excluded_unless_requested() {
        let result = ToolEvent {
            kind: "tool_result".into(),
            name: None,
            input: None,
            output: Some("ok".into()),
            index: 1,
        };
        let mut t = Transcript::default();
        t.push(Message::assistant("x").with_events(vec![result]));
        let default = ToolQuery::default();
        assert_eq!(t.count_tool_events(&default), 0);
        let including = ToolQuery {
            include_results: true,
            ..ToolQuery::default()
        };
        assert_eq!(t.count_tool_events(&including), 1);
    }

    #[test]
    fn event_summary_is_compact_and_truncates() {
        let short = call("bash", json!({"command": "ls"}));
        assert_eq!(short.summary(), r#"bash({"command":"ls"})"#);

        let long = call("bash", json!({"command": "x".repeat(500)}));
        let summary = long.summary();
        assert!(summary.contains('…'));
        assert!(summary.starts_with("bash("));
        assert!(summary.chars().count() <= 210);

        let no_input = ToolEvent {
            kind: "tool_result".into(),
            name: None,
            input: None,
            output: None,
            index: 0,
        };
        assert_eq!(no_input.summary(), "tool_result");
    }

    #[test]
    fn role_labels_and_events_skip_when_empty() {
        assert_eq!(Role::User.label(), "User");
        assert_eq!(Role::System.label(), "System");
        let json = serde_json::to_string(&Message::user("hi")).unwrap();
        assert!(!json.contains("events"));
    }
}
