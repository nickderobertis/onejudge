//! Protocol-v8 schema-bundle generation and link-resolution contract tests.
#![cfg(feature = "sdk-schema")]

use onejudge::sdk_schema::{frame_bundle_version_allows, judge_seat_frame_bundle};
use onemessagebus::sdk_schema::RegistryDocument;
use onemessagebus::{BundleVersion, Freshness, LinkResolver, SchemaBundle, SchemaLink};

fn bundle(version: &str, schema: serde_json::Value) -> SchemaBundle {
    SchemaBundle::new(
        version.parse::<BundleVersion>().unwrap(),
        None,
        vec![RegistryDocument {
            id: "agent.test.frame@1".parse().unwrap(),
            schema,
        }],
    )
    .unwrap()
}

#[test]
fn committed_bundle_is_generated_and_resolves_from_a_pinned_file_link() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/judge-seat-frames.json");
    let committed = std::fs::read_to_string(&path).unwrap();
    let generated = serde_json::to_string_pretty(&judge_seat_frame_bundle()).unwrap() + "\n";
    assert_eq!(committed, generated, "regenerate with `cargo run -q -p onejudge --features sdk-schema --example generate_judge_seat_frames > schemas/judge-seat-frames.json`");

    let link: SchemaLink = format!("file://{}@8", path.display()).parse().unwrap();
    let resolved = LinkResolver::new(None)
        .resolve(&link, Freshness::Window)
        .unwrap();
    assert_eq!(resolved.bundle(), &judge_seat_frame_bundle());
    assert_eq!(resolved.bundle().schemas().len(), 5);
    assert!(resolved
        .bundle()
        .schemas()
        .iter()
        .all(|document| document.id.to_string().ends_with("@8")));
}

#[test]
fn schema_drift_requires_a_version_increase() {
    let original = bundle("8", serde_json::json!({"type": "object"}));
    let changed_same = bundle("8", serde_json::json!({"type": "string"}));
    let changed_new = bundle("9", serde_json::json!({"type": "string"}));
    let unchanged = bundle("8", serde_json::json!({"type": "object"}));

    assert!(!frame_bundle_version_allows(&original, &changed_same));
    assert!(frame_bundle_version_allows(&original, &changed_new));
    assert!(frame_bundle_version_allows(&original, &unchanged));
}

#[test]
fn generated_bundle_check_refuses_a_hand_edit_with_the_regenerate_command() {
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("hand-edited-frames.json");
    std::fs::write(&path, "{}\n").unwrap();
    let failure = onejudge::sdk_schema::check_judge_seat_frame_bundle(&path).unwrap_err();
    assert!(failure.contains("differs from generated frames"));
    assert!(failure.contains("generate_judge_seat_frames"));
}

#[test]
fn documentation_frame_versions_and_turn_contract_match_the_generated_schema() {
    let protocol = include_str!("../../../docs/protocol.md");
    let contract = include_str!("../../../docs/contract.md");
    for document in [protocol, contract] {
        for occurrence in document.match_indices("agent.onejudge-frame.") {
            let suffix = &document[occurrence.0..];
            let version = suffix
                .split_once('@')
                .and_then(|(_, rest)| {
                    rest.chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                        .parse::<u32>()
                        .ok()
                })
                .expect("a documented frame id carries a numeric version");
            assert_eq!(version, 8, "a documented frame id drifted from protocol v8");
        }
    }
    assert!(protocol.contains(r#""turn":{"outcome":"taken"}"#));
    let supervisor = judge_seat_frame_bundle()
        .schemas()
        .iter()
        .find(|document| document.id.to_string() == "agent.onejudge-frame.supervisor@8")
        .unwrap()
        .schema
        .clone();
    let cause = &supervisor["$defs"]["TurnOutcome"]["oneOf"][1]["properties"]["cause"];
    assert_eq!(cause["minLength"], 1);
    assert_eq!(cause["maxLength"], 80);
    assert_eq!(cause["pattern"], "^[^\\r\\n]+$");
}
