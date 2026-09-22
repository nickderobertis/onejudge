//! Rust-owned JSON Schema roots consumed by generated language SDKs, and the
//! command-provider frame schemas onejudge registers in a `onemessagebus` registry.

use std::collections::BTreeMap;

use onemessagebus::sdk_schema::RegistryDocument;
use onemessagebus::{BundleVersion, Message, Registry, RegistryError, SchemaBundle, SchemaId};
use schemars::{generate::SchemaSettings, JsonSchema, Schema};
use serde::Serialize;

use crate::{
    cli::{Config, FailureReport},
    note::Note,
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
    /// The command-provider request frames (`docs/protocol.md`), one per operation,
    /// keyed by the id each is registered under: `agent.onejudge-frame.<op>@8`.
    pub frames: BTreeMap<String, Schema>,
}

/// Generate a schema for a serialized output value.
#[must_use]
pub fn schema_for_serialize<T: ?Sized + JsonSchema>() -> Schema {
    SchemaSettings::default()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>()
}

/// Every command-provider request frame's schema — `respond`, `user`, `supervisor`,
/// `judge`, `assess` — with the id it is registered under,
/// `agent.onejudge-frame.<op>@<protocol version>`.
///
/// These are the declaration of the frames `CommandProvider` writes: each is
/// generated from the frame type that writes it.
#[must_use]
pub fn frame_schemas() -> Vec<(SchemaId, Schema)> {
    crate::command::frame_schemas()
}

/// Build the schema-link bundle that publishes the judge-seat frame grammar.
#[must_use]
pub fn judge_seat_frame_bundle() -> SchemaBundle {
    let schemas = frame_schemas()
        .into_iter()
        .map(|(id, schema)| RegistryDocument {
            id,
            schema: schema.to_value(),
        })
        .collect();
    SchemaBundle::new(
        crate::command::PROTOCOL_VERSION
            .to_string()
            .parse::<BundleVersion>()
            .expect("the numeric protocol version is a bundle version"),
        Some("onejudge command-provider request frames".to_string()),
        schemas,
    )
    .expect("the generated frame ids and schemas form a bundle")
}

/// Whether schema drift between two bundles is accompanied by a version increase.
#[must_use]
pub fn frame_bundle_version_allows(base: &SchemaBundle, current: &SchemaBundle) -> bool {
    base.schemas() == current.schemas() || current.version() > base.version()
}

/// Check that `path` contains the generated judge-seat bundle byte for byte.
pub fn check_judge_seat_frame_bundle(path: &std::path::Path) -> Result<(), String> {
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&judge_seat_frame_bundle())
            .expect("the bus bundle is serializable")
    );
    let committed = std::fs::read_to_string(path)
        .map_err(|failure| format!("could not read {}: {failure}", path.display()))?;
    if committed == generated {
        Ok(())
    } else {
        Err(format!(
            "{} differs from generated frames; regenerate with `cargo run -q -p onejudge --features sdk-schema --example generate_judge_seat_frames > schemas/judge-seat-frames.json`",
            path.display()
        ))
    }
}

/// The version of the bundle publishing the note contract.
///
/// Its own number, not the command-provider protocol's: the two grammars change
/// for different reasons, and a frame added to `docs/protocol.md` must not tell a
/// client its note validator went stale.
pub const NOTE_BUNDLE_VERSION: u32 = 1;

/// The note message's schema, with the id it is registered under, `agent.note@1`.
///
/// Generated from [`Note`] itself — the one declaration — so a client in another
/// language validates an arriving note against what the engine reads it with, and
/// a shape that moved here is a bundle that no longer matches the committed file.
#[must_use]
pub fn note_schema() -> (SchemaId, Schema) {
    (Note::SCHEMA, schemars::schema_for!(Note))
}

/// Build the schema-link bundle that publishes the note contract onejudge owns.
///
/// Holds `agent.note@1` alone. The command-provider frames are a separate grammar
/// in a separate bundle ([`judge_seat_frame_bundle`]), versioned separately.
#[must_use]
pub fn note_bundle() -> SchemaBundle {
    let (id, schema) = note_schema();
    SchemaBundle::new(
        NOTE_BUNDLE_VERSION
            .to_string()
            .parse::<BundleVersion>()
            .expect("the numeric bundle version is a bundle version"),
        Some("onejudge note contract".to_string()),
        vec![RegistryDocument {
            id,
            schema: schema.to_value(),
        }],
    )
    .expect("the generated note id and schema form a bundle")
}

/// Check that `path` contains the generated note bundle byte for byte.
///
/// # Errors
/// A message naming the regeneration command when the committed file differs from
/// what [`note_bundle`] generates, or when `path` cannot be read.
pub fn check_note_bundle(path: &std::path::Path) -> Result<(), String> {
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&note_bundle()).expect("the bus bundle is serializable")
    );
    let committed = std::fs::read_to_string(path)
        .map_err(|failure| format!("could not read {}: {failure}", path.display()))?;
    if committed == generated {
        Ok(())
    } else {
        Err(format!(
            "{} differs from the generated note schema; regenerate with `cargo run -q -p onejudge --features sdk-schema --example generate_note_schema > schemas/note.json`",
            path.display()
        ))
    }
}

/// Register the note schema in `registry`, so a bus that checks records against it
/// checks an arriving note against onejudge's own declaration.
///
/// # Errors
/// [`RegistryError`] when `registry` already holds a different document under
/// `agent.note@1`.
pub fn register_note(registry: &mut Registry) -> Result<(), RegistryError> {
    let (id, schema) = note_schema();
    registry.register_schema(id, schema.to_value())
}

/// Register every command-provider frame schema in `registry`, so a bus that checks
/// records against it checks onejudge's frames against onejudge's own declaration.
///
/// # Errors
/// [`RegistryError`] when `registry` already holds a different document under one
/// of the ids.
pub fn register_frames(registry: &mut Registry) -> Result<(), RegistryError> {
    for (id, schema) in frame_schemas() {
        registry.register_schema(id, schema.to_value())?;
    }
    Ok(())
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
        frames: frame_schemas()
            .into_iter()
            .map(|(id, schema)| (id.to_string(), schema))
            .collect(),
    }
}
