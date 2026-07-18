//! Rust-owned JSON Schema roots consumed by generated language SDKs.

use schemars::{generate::SchemaSettings, JsonSchema, Schema};
use serde::Serialize;

use crate::{cli::Config, Report, StreamEvent};

/// The deterministic bundle of onejudge's public SDK input/output contracts.
#[derive(Debug, Serialize)]
pub struct SdkSchemaBundle {
    /// YAML run-config input accepted by `onejudge run`.
    pub run_config: Schema,
    /// Versioned JSON report emitted by `onejudge run --format json`.
    pub report: Schema,
    /// One live tool-event envelope emitted during a streaming run.
    pub stream_event: Schema,
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
    }
}
