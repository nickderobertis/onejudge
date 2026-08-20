//! Rust-owned JSON Schema roots consumed by generated language SDKs.

use schemars::{generate::SchemaSettings, JsonSchema, Schema};
use serde::Serialize;

use crate::{
    cli::{Config, FailureReport},
    Observation, Report, StreamEvent,
};

/// The deterministic bundle of onejudge's public SDK input/output contracts.
#[derive(Debug, Serialize)]
pub struct SdkSchemaBundle {
    /// YAML run-config input accepted by `onejudge run`.
    pub run_config: Schema,
    /// Versioned JSON report emitted by `onejudge run --format json`.
    pub report: Schema,
    /// One live tool-event envelope emitted during a streaming run.
    pub stream_event: Schema,
    /// One live observation of a run in progress — a turn's opening, a tool event,
    /// a party's reply, or a turn's close — as an in-process embedder receives it.
    pub observation: Schema,
    /// Versioned JSON document emitted instead of a report when a `--format json`
    /// run fails, carrying the classified error and the harness attribution the
    /// run had recorded.
    pub failure_report: Schema,
}

/// Generate a schema for a serialized output value.
#[must_use]
pub fn schema_for_serialize<T: ?Sized + JsonSchema>() -> Schema {
    SchemaSettings::default()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>()
}

/// Build the named schema bundle in stable field order.
#[must_use]
pub fn bundle() -> SdkSchemaBundle {
    SdkSchemaBundle {
        run_config: schemars::schema_for!(Config),
        report: schema_for_serialize::<Report>(),
        stream_event: schema_for_serialize::<StreamEvent<'static>>(),
        observation: schema_for_serialize::<Observation<'static>>(),
        failure_report: schema_for_serialize::<FailureReport>(),
    }
}
