//! `onejudge` drives a **simulated interaction and evaluation loop** on top of
//! [`oneharness`](https://github.com/nickderobertis/oneharness): take a skill or
//! agent, drive it through a multi-turn conversation with a simulated user, and
//! score the resulting transcript with natural-language (judge) verdicts and
//! tool-event queries.
//!
//! The layering: `oneharness` (one invocation → one JSON report) → **`onejudge`**
//! (interaction + judging loop) → higher-level test frameworks (e.g.
//! [`skilltest`](https://github.com/nickderobertis/skilltest), which adds cases,
//! evals-as-assertions, and SDKs).
//!
//! # The pieces
//!
//! - [`Provider`] is the boundary — `onejudge` never talks to a model directly.
//!   Every model call goes through `oneharness`. [`OneharnessProvider`] shells out
//!   to the `oneharness` CLI; [`CommandProvider`] speaks a small JSON-lines
//!   protocol (see `docs/protocol.md`) for the deterministic test doubles and any
//!   custom backend; and [`SplitProvider`] composes a skill-running provider with a
//!   separate judge / simulated-user provider (e.g. run the skill on one harness
//!   and judge on another).
//! - [`Engine`] runs a [`Conversation`] (a [`Skill`] plus an initial input and an
//!   optional [`SimulatedUser`]) into a [`Transcript`], bounded by `max_turns` /
//!   `done_when` / the skill declaring itself done, threading one caller-owned
//!   session name across turns (the provider degrades gracefully where a harness
//!   cannot continue a session).
//! - [`Transcript`] carries each turn plus the normalized [`ToolEvent`]s the skill
//!   took, so the judge — and a [`ToolQuery`] — can reason over *what the skill
//!   did*, not just what it said.
//! - [`Report`] is onejudge's own versioned contract ([`SCHEMA_VERSION`]): a
//!   serializable bundle of the transcript, verdicts, and usage that downstream
//!   SDKs compose over and re-export (see `docs/contract.md`).
//! - [`SpawnHook`] is the seam for an **in-process embedder**: onejudge offers
//!   every process it is about to create, so the embedder can place it in a group
//!   it owns (a POSIX process group, a Windows job object) and still terminate the
//!   whole harness tree on cancellation. The embedder owns the group; onejudge only
//!   reports what a hook said it did, on [`Report::processes`] (see
//!   `docs/spawn-hook.md`). Install it on each provider, or — driving the run
//!   through the CLI's plan — on the plan itself (`Plan::with_spawn_hook`, under
//!   the `cli` feature), which reaches both sides of a two-party run.
//!
//! # Example
//!
//! ```no_run
//! use onejudge::{Conversation, Engine, OneharnessProvider, Settings, Skill, SimulatedUser};
//!
//! let provider = OneharnessProvider::new();
//! // Harness/model selection lives in oneharness's config files, not here.
//! let settings = Settings::new();
//! let engine = Engine::new(&provider, settings);
//!
//! let skill = Skill::new("greeter", "./skills/greeter", "Greet the user warmly.");
//! let user = SimulatedUser::new("A curious first-time visitor.")
//!     .done_when("the assistant has answered the visitor's question");
//! let outcome = engine.run(&Conversation::multi_turn(skill, "hi", user))?;
//!
//! let verdict = engine.judge_boolean("the reply was welcoming", &outcome.transcript)?;
//! println!("{:?}: {}", verdict.value, verdict.reason);
//! # Ok::<(), onejudge::Error>(())
//! ```

#[cfg(feature = "cli")]
pub mod cli;
mod command;
mod engine;
mod error;
mod oneharness;
mod provider;
mod report;
#[cfg(feature = "skill")]
mod skill;
mod spawn;
mod split;
mod stream;
mod telemetry;
mod transcript;
mod usage;

pub use command::CommandProvider;
pub use engine::{Conversation, Engine, Outcome, Settings, SimulatedUser, Skill, StreamEvent};
pub use error::{Error, ProviderErrorKind, Result};
pub use oneharness::OneharnessProvider;
pub use provider::{
    build_assessment_prompt, build_judge_prompt, build_respond_prompt, build_supervisor_prompt,
    build_user_prompt, latest_or_inline, latest_user_message, parse_supervisor, parse_verdict,
    render_transcript, Assessment, AssistantTurn, JudgeKind, JudgeQuery, JudgeValue, JudgeVerdict,
    Provider, SkillRef, SupervisorOutcome, SupervisorQuery, SupervisorTurn, UserTurn,
};
pub use report::{NamedVerdict, Report, SCHEMA_VERSION};
#[cfg(feature = "sdk-schema")]
pub mod sdk_schema;
#[cfg(feature = "skill")]
pub use skill::{load_skill, Frontmatter, SkillDefinition};
pub use spawn::{SharedSpawnHook, SpawnContext, SpawnHook, SpawnedProcess};
pub use split::SplitProvider;
#[doc(hidden)]
pub use telemetry::InvocationTelemetry;
pub use telemetry::{
    CandidateAttempt, FellThrough, HarnessAttribution, PartyTelemetry, SessionLink, Telemetry,
    TelemetryRole,
};
pub use transcript::{Message, Role, ToolEvent, ToolQuery, Transcript};
pub use usage::Usage;
