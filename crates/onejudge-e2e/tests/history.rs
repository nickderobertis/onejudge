//! Live history addresses over both real oneharness entry points. Only the
//! paid harness is doubled; the spawning path runs the released CLI shell.
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use onejudge::{
    CommandProvider, Conversation, Engine, JudgeEntry, JudgePanel, LlmlintProvider, Observation,
    OneharnessProvider, Provider, Settings, SimulatedUser, Skill, SplitProvider,
};
use onejudge_test_doubles::{self as doubles, support};
use serde_json::Value;

fn project(name: &str, history: Option<bool>, fallback: bool) -> PathBuf {
    // The dispatch host enables history through the environment. These
    // journeys exercise the side's file config, without writing its pointer.
    std::env::remove_var("ONEHARNESS_HISTORY");
    std::env::remove_var("ONEHARNESS_HISTORY_POINTER_FILE");
    let dir = support::scratch_path(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = |fallback: bool| {
        let harnesses = if fallback {
            r#"["claude-code:limited", "claude-code:ready"]"#
        } else {
            r#"["claude-code:ready"]"#
        };
        format!(
            "harnesses = {harnesses}\nrun_mode = \"fallback\"\nhistory_dir = {:?}\n{}\n\
             [harness.claude-code]\nbin = {:?}\n\
             [harness.claude-code.variant.limited.env]\nONEJUDGE_FAKE_HARNESS_QUOTA = \"1\"\n\
             [harness.claude-code.variant.ready]\n",
            dir.join("history").to_str().unwrap(),
            history.map_or(String::new(), |enabled| format!("history = {enabled}")),
            doubles::fake_harness(),
        )
    };
    std::fs::write(dir.join("oneharness.toml"), config(fallback)).unwrap();
    std::fs::write(dir.join("judge.toml"), config(fallback)).unwrap();
    dir
}

fn provider(dir: &Path, spawned: bool) -> OneharnessProvider {
    let provider = OneharnessProvider::new()
        .with_judge_config(dir.join("judge.toml"))
        .with_judge_events(true);
    if spawned {
        provider.with_bin(doubles::oneharness_cli())
    } else {
        provider
    }
}

fn conversation(dir: &Path) -> Conversation {
    Conversation::multi_turn(
        Skill::new(
            "history",
            dir.to_str().unwrap(),
            format!(
                "[[reply:worker reply]][[say:working]][[event:worker tool]][[stream-wait:{}]]",
                dir.join("release").display()
            ),
        ),
        format!(
            "do it [[artifact-evaluator:{}]][[artifact-supervisor-turns:2]][[judge-event:judge tool]]",
            dir.join("judge-prompts").display()
        ),
        SimulatedUser::new("review").max_turns(3),
    )
}

fn history_file(value: &Value) -> PathBuf {
    Path::new(value["history_dir"].as_str().unwrap())
        .join(value["history_project"].as_str().unwrap())
        .join(format!(
            "{}.jsonl",
            value["history_session"].as_str().unwrap()
        ))
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn journey(spawned: bool, fallback: bool) {
    let dir = project(&format!("history-{spawned}-{fallback}"), None, fallback);
    let provider = provider(&dir, spawned);
    let mut seen = Vec::new();
    let mut active: BTreeMap<(usize, String), Value> = BTreeMap::new();
    let outcome = Engine::new(&provider, Settings::new())
        .run_observing(&conversation(&dir), &mut |observation| {
            let value = serde_json::to_value(observation).unwrap();
            if let Observation::HistoryBegun(begun) = observation {
                let key = (begun.turn, value["role"].as_str().unwrap().to_string());
                assert!(seen.iter().any(|previous: &Value| {
                    previous["type"] == "turn_opened"
                        && previous["turn"] == value["turn"]
                        && previous["role"] == value["role"]
                }));
                // The opening is already durable while the harness is still
                // blocked. Only this callback can release the worker's turn.
                let opening = lines(&history_file(&value));
                assert!(
                    opening.iter().any(|line| {
                        line["type"] == "run-started" && line["history_id"] == value["history_id"]
                    }),
                    "history address must already locate its opening: {opening:?}"
                );
                active.insert(key, value.clone());
                std::fs::write(dir.join("release"), b"go").unwrap();
            }
            if let Observation::Action(action) = observation {
                let current = &active[&(action.turn, "assistant".into())];
                assert!(
                    lines(&history_file(current)).iter().any(|line| {
                        line["type"] == "event"
                            && line["run_id"] == current["history_id"]
                            && line["event"]["index"] == action.event.index
                    }),
                    "an action must follow its own candidate's live history opening"
                );
            }
            if matches!(
                observation,
                Observation::Tool(_) | Observation::JudgeTool(_) | Observation::Message(_)
            ) {
                let role = if value["type"] == "judge_tool" {
                    "user"
                } else if value["type"] == "tool" {
                    "assistant"
                } else {
                    value["role"].as_str().unwrap()
                };
                assert!(
                    active.contains_key(&(value["turn"].as_u64().unwrap() as usize, role.into()))
                );
            }
            seen.push(value);
            ControlFlow::Continue(())
        })
        .unwrap();
    assert!(outcome.completion_reason.is_some());
    assert_eq!(outcome.transcript.assistant_turns(), 2);
    let histories: Vec<_> = seen
        .iter()
        .filter(|v| v["type"] == "history_begun")
        .collect();
    assert_eq!(histories.len(), if fallback { 8 } else { 4 });
    let ids: std::collections::BTreeSet<_> = histories
        .iter()
        .map(|v| v["history_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids.len(),
        histories.len(),
        "every candidate has a distinct id"
    );
    for history in &histories {
        let records = lines(&history_file(history));
        assert!(records
            .iter()
            .any(|line| line["type"] == "run" && line["history_id"] == history["history_id"]));
    }
    // Every candidate actually produced events, including the failed candidate.
    let action_count = seen.iter().filter(|v| v["type"] == "action").count();
    assert_eq!(action_count, if fallback { 6 } else { 4 });
    assert!(seen.iter().any(|v| v["type"] == "judge_tool"));
    // Removing the new variant leaves the old event order and payloads intact.
    let old: Vec<_> = seen
        .iter()
        .filter(|v| v["type"] != "history_begun")
        .collect();
    for (index, observation) in old.iter().enumerate() {
        if observation["type"] == "tool" {
            assert_eq!(old[index + 1]["type"], "action");
            assert_eq!(
                old[index + 1]["event"]["index"],
                observation["event"]["index"]
            );
        }
    }
}

#[test]
fn in_process_history_begins_live_on_both_sides_over_multiple_turns() {
    journey(false, false);
}
#[test]
fn released_cli_history_begins_live_on_both_sides_over_multiple_turns() {
    journey(true, false);
}
#[test]
fn in_process_history_begins_for_each_fallback_candidate() {
    journey(false, true);
}
#[test]
fn released_cli_history_begins_for_each_fallback_candidate() {
    journey(true, true);
}

#[test]
fn existing_provider_callbacks_keep_their_payloads_and_can_stop_a_turn() {
    for spawned in [false, true] {
        let dir = project(&format!("history-legacy-callbacks-{spawned}"), None, false);
        let provider = provider(&dir, spawned).with_streaming(true);
        let skill = onejudge::SkillRef {
            name: "legacy",
            dir: dir.to_str().unwrap(),
            instructions: "[[reply:finished]][[say:working]][[event:legacy tool]]",
        };
        let mut tools = Vec::new();
        let reply = provider
            .respond_streaming(&skill, &[], None, &mut |event| {
                tools.push(event.clone());
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(reply.message, "finished");
        assert_eq!(tools, reply.events);
        assert_eq!(tools.len(), 1);

        let mut actions = Vec::new();
        let reply = provider
            .respond_observing(&skill, &[], None, &mut |event| {
                actions.push(event.clone());
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(reply.message, "finished");
        assert!(actions.iter().any(|event| event.kind == "tool_call"));

        let mut calls = 0;
        let stopped = provider
            .respond_streaming(&skill, &[], None, &mut |_| {
                calls += 1;
                ControlFlow::Break(())
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert!(stopped.message.is_empty());
    }
}

#[test]
fn history_disabled_sides_emit_no_openings_on_either_path() {
    for spawned in [false, true] {
        let dir = project(&format!("history-off-{spawned}"), Some(false), false);
        std::fs::write(dir.join("release"), b"go").unwrap();
        let provider = provider(&dir, spawned);
        let mut histories = 0;
        let mut actions = 0;
        let outcome = Engine::new(&provider, Settings::new())
            .run_observing(&conversation(&dir), &mut |observation| {
                histories += usize::from(matches!(observation, Observation::HistoryBegun(_)));
                actions += usize::from(matches!(observation, Observation::Action(_)));
                ControlFlow::Continue(())
            })
            .unwrap();
        assert!(outcome.completion_reason.is_some());
        assert_eq!(histories, 0);
        assert!(actions > 0);
        assert!(!dir.join("history").exists());
    }
}

fn no_history_judge(judge: impl Provider, name: &str) {
    let dir = project(name, None, false);
    let worker = provider(&dir, false);
    let provider = SplitProvider::new(worker, judge);
    let mut worker_histories = 0;
    let mut judge_openings = 0;
    let outcome = Engine::new(&provider, Settings::new())
        .run_observing(&conversation(&dir), &mut |observation| {
            if let Observation::TurnOpened(opened) = observation {
                judge_openings += usize::from(opened.role == onejudge::Role::User);
            }
            if let Observation::HistoryBegun(begun) = observation {
                assert_eq!(begun.role, onejudge::Role::Assistant);
                worker_histories += 1;
                std::fs::write(dir.join("release"), b"go").unwrap();
            }
            ControlFlow::Continue(())
        })
        .unwrap();
    assert!(outcome.completion_reason.is_some());
    assert!(worker_histories > 0);
    assert!(judge_openings > 0);
}

#[test]
fn command_judge_emits_no_history_opening() {
    let judge = CommandProvider::new(vec![
        doubles::echo_provider().into(),
        "[[supervisor-complete:accepted]]".into(),
    ])
    .unwrap();
    no_history_judge(judge, "history-command-judge");
}

#[test]
fn llmlint_judge_emits_no_history_opening() {
    let judge = LlmlintProvider::new(doubles::fake_llmlint()).unwrap();
    no_history_judge(judge, "history-llmlint-judge");
}

#[test]
fn panel_relays_live_history_on_the_observers_thread() {
    let dir = project("history-panel", None, false);
    let judge = JudgePanel::new(vec![
        JudgeEntry::new("first", "oneharness", provider(&dir, false)),
        JudgeEntry::new("second", "oneharness", provider(&dir, false)),
    ])
    .unwrap();
    let provider = SplitProvider::new(provider(&dir, false), judge);
    let mut roles = Vec::new();
    let observer_thread = std::thread::current().id();
    let mut task = conversation(&dir);
    task.input = "do it [[evaluate]][[judge-event:review]]".into();
    let result = Engine::new(&provider, Settings::new())
        .run_observing(&task, &mut |observation| {
            assert_eq!(std::thread::current().id(), observer_thread);
            if let Observation::HistoryBegun(begun) = observation {
                roles.push(begun.role);
                std::fs::write(dir.join("release"), b"go").unwrap();
            }
            ControlFlow::Continue(())
        })
        .unwrap();
    assert!(result.completion_reason.is_some());
    assert_eq!(
        roles,
        [
            onejudge::Role::Assistant,
            onejudge::Role::User,
            onejudge::Role::User
        ]
    );
}

#[test]
fn breaking_a_history_opening_stops_either_side_without_further_observations() {
    for spawned in [false, true] {
        for role in [onejudge::Role::Assistant, onejudge::Role::User] {
            let dir = project(&format!("history-stop-{spawned}-{role:?}"), None, false);
            let provider = provider(&dir, spawned);
            let mut last = Value::Null;
            let outcome = Engine::new(&provider, Settings::new())
                .run_observing(&conversation(&dir), &mut |observation| {
                    last = serde_json::to_value(observation).unwrap();
                    if let Observation::HistoryBegun(begun) = observation {
                        if begun.role == role {
                            return ControlFlow::Break(());
                        }
                        std::fs::write(dir.join("release"), b"go").unwrap();
                    }
                    ControlFlow::Continue(())
                })
                .unwrap();
            assert!(outcome.stopped_early);
            assert_eq!(last["type"], "history_begun");
            assert_eq!(last["role"], serde_json::to_value(role).unwrap());
        }
    }
}

#[test]
fn breaking_a_panel_history_opening_stops_all_history_delivery() {
    let dir = project("history-panel-stop", None, false);
    let judges = JudgePanel::new(vec![
        JudgeEntry::new("first", "oneharness", provider(&dir, false)),
        JudgeEntry::new("second", "oneharness", provider(&dir, false)),
    ])
    .unwrap();
    let provider = SplitProvider::new(provider(&dir, false), judges);
    let mut stopped = false;
    let mut worker_openings = 0;
    let result = Engine::new(&provider, Settings::new())
        .run_observing(&conversation(&dir), &mut |observation| {
            assert!(
                !stopped,
                "no observation may follow the one that stopped the run"
            );
            if let Observation::HistoryBegun(begun) = observation {
                if begun.role == onejudge::Role::User {
                    stopped = true;
                    return ControlFlow::Break(());
                }
                worker_openings += 1;
                std::fs::write(dir.join("release"), b"go").unwrap();
            }
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(worker_openings, 1);
    assert!(stopped);
    assert!(result.stopped_early);
}
