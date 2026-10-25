//! The worker side's oneharness configuration over both real oneharness entry
//! points: the linked engine in process, and the released CLI shell spawned.
//! Only the paid harness is doubled, and it reports the variant environment the
//! configuration it ran under gave it.
//!
//! Every journey sandboxes the user file (`XDG_CONFIG_HOME`, `HOME`, `APPDATA`)
//! so the host's own is never read, and puts a project `oneharness.toml` at the
//! worktree root. With a worker config, both carry the incident's shape — a
//! variant whose `env_from` names a variable nothing sets, which oneharness
//! refuses — so a turn that layered either one could not complete at all.
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use onejudge::{Conversation, Engine, Observation, OneharnessProvider, Settings, Skill};
use onejudge_test_doubles::{self as doubles, support};

/// Set by every file, to a value naming the file: which one won.
const SHARED: &str = "ONEJUDGE_WC_SHARED";
/// Each set by one file only: whether that file was read at all.
const USER: &str = "ONEJUDGE_WC_USER";
const PROJECT: &str = "ONEJUDGE_WC_PROJECT";
const WORKER: &str = "ONEJUDGE_WC_WORKER";
/// What a poisoned file's `env_from` maps from [`UNSET`].
const INDIRECT: &str = "ONEJUDGE_WC_INDIRECT";
/// The parent variable no process in a journey ever has.
const UNSET: &str = "ONEJUDGE_WC_NEVER_SET";

/// The fake harness's reply naming every variable a journey distinguishes.
fn report_env() -> String {
    format!("[[report-env:{SHARED},{USER},{PROJECT},{WORKER},{INDIRECT}]]")
}

/// The reply a turn run under exactly the worker config gives.
fn worker_only() -> String {
    format!("{SHARED}=worker {USER}=unset {PROJECT}=unset {WORKER}=worker {INDIRECT}=unset")
}

/// One oneharness config file selecting `harness:pinned`, whose variant sets
/// [`SHARED`] to `name`, `own` to `name`, and — when `poisoned` — maps
/// [`INDIRECT`] from the never-set [`UNSET`]. `top` lands above every table.
fn harness_file(
    path: &Path,
    harness: &str,
    name: &str,
    own: &str,
    poisoned: bool,
    top: &str,
) -> PathBuf {
    let history = path.parent().unwrap().join("history");
    let mut text = format!(
        "harnesses = [\"{harness}:pinned\"]\nhistory_dir = {:?}\n{top}\n\
         [harness.{harness}]\nbin = {:?}\n\
         [harness.{harness}.variant.pinned.env]\n{SHARED} = \"{name}\"\n{own} = \"{name}\"\n",
        history.display().to_string(),
        doubles::fake_harness(),
    );
    if poisoned {
        text.push_str(&format!(
            "[harness.{harness}.variant.pinned.env_from]\n{INDIRECT} = \"{UNSET}\"\n"
        ));
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    path.to_path_buf()
}

/// A journey's directory: a worktree whose root holds a project config, a user
/// config this process — and every oneharness it starts — discovers instead of
/// the host's, and nothing else.
struct Sandbox {
    worktree: PathBuf,
    root: PathBuf,
}

impl Sandbox {
    /// `poisoned` gives both discovered files the unresolvable indirection;
    /// `user_top` and `project_top` land above each file's tables.
    fn new(name: &str, harness: &str, poisoned: bool, user_top: &str, project_top: &str) -> Self {
        // Neither the host's oneharness overrides nor its user file may reach a
        // journey: the dispatch host enables history through the environment.
        for (key, _) in std::env::vars() {
            if key.starts_with("ONEHARNESS_") {
                std::env::remove_var(key);
            }
        }
        std::env::remove_var(UNSET);
        let root = support::scratch_path(name);
        let _ = std::fs::remove_dir_all(&root);
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let config_home = root.join("config-home");
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var("APPDATA", &config_home);
        std::env::set_var("HOME", root.join("home"));
        harness_file(
            &config_home.join("oneharness").join("config.toml"),
            harness,
            "user",
            USER,
            poisoned,
            user_top,
        );
        harness_file(
            &worktree.join("oneharness.toml"),
            harness,
            "project",
            PROJECT,
            poisoned,
            project_top,
        );
        Self { worktree, root }
    }

    /// A worker config outside the worktree, so nothing discovers it.
    fn worker_config(&self, harness: &str, top: &str) -> PathBuf {
        harness_file(
            &self.root.join("pinned").join("worker.toml"),
            harness,
            "worker",
            WORKER,
            false,
            top,
        )
    }

    fn skill(&self) -> Skill {
        Skill::new("pinned", self.worktree.to_str().unwrap(), report_env())
    }
}

fn seam(provider: OneharnessProvider, spawned: bool) -> OneharnessProvider {
    if spawned {
        provider.with_bin(doubles::oneharness_cli())
    } else {
        provider
    }
}

/// Run one worker turn and hand back its reply and how many worker-side history
/// openings it announced.
fn worker_turn(provider: &OneharnessProvider, sandbox: &Sandbox) -> (String, usize) {
    let mut openings = 0;
    let outcome = Engine::new(provider, Settings::new())
        .run_observing(
            &Conversation::single_turn(sandbox.skill(), "report"),
            &mut |observation| {
                if let Observation::HistoryBegun(begun) = observation {
                    assert_eq!(begun.role, onejudge::Role::Assistant);
                    openings += 1;
                }
                ControlFlow::Continue(())
            },
        )
        .expect("the worker turn completes");
    (outcome.transcript.messages[1].content.clone(), openings)
}

#[test]
fn a_worker_config_is_all_the_worker_side_runs_under_on_both_seams() {
    for spawned in [false, true] {
        let sandbox = Sandbox::new(&format!("wc-pinned-{spawned}"), "claude-code", true, "", "");
        let worker = sandbox.worker_config("claude-code", "");
        let provider = seam(
            OneharnessProvider::new().with_worker_config(&worker),
            spawned,
        );
        let (reply, openings) = worker_turn(&provider, &sandbox);
        // Neither discovered file reached the turn: either would have refused it
        // over its indirection, and either would have named itself here.
        assert_eq!(reply, worker_only(), "spawned: {spawned}");
        // onejudge's history default still sits beneath the worker config.
        assert_eq!(openings, 1, "spawned: {spawned}");
    }
}

#[test]
fn a_worker_config_that_turns_history_off_is_honoured_on_both_seams() {
    for spawned in [false, true] {
        let sandbox = Sandbox::new(&format!("wc-quiet-{spawned}"), "claude-code", true, "", "");
        let worker = sandbox.worker_config("claude-code", "history = false");
        let provider = seam(
            OneharnessProvider::new().with_worker_config(&worker),
            spawned,
        );
        let (reply, openings) = worker_turn(&provider, &sandbox);
        assert_eq!(reply, worker_only(), "spawned: {spawned}");
        assert_eq!(openings, 0, "spawned: {spawned}");
    }
}

/// Without a worker config, the poisoned files are exactly what a worker turn
/// runs under — the incident itself, so the journeys above prove something.
#[test]
fn without_a_worker_config_the_discovered_files_reach_the_worker_turn() {
    for spawned in [false, true] {
        let sandbox = Sandbox::new(
            &format!("wc-incident-{spawned}"),
            "claude-code",
            true,
            "",
            "",
        );
        let provider = seam(OneharnessProvider::new(), spawned);
        let err = Engine::new(&provider, Settings::new())
            .run(&Conversation::single_turn(sandbox.skill(), "report"))
            .expect_err("a discovered indirection that cannot resolve refuses the turn");
        assert!(err.to_string().contains(UNSET), "spawned: {spawned}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn every_rung_of_the_retry_ladder_runs_under_the_worker_config() {
    // OpenCode's control mechanism implements no resume request, so a controlled
    // continuation of a named session is refused and retried without `--control`:
    // the second rung is the one that runs, and it must carry the same config.
    for spawned in [false, true] {
        let _store = support::use_private_session_store(&format!("wc-ladder-{spawned}"));
        let sandbox = Sandbox::new(&format!("wc-ladder-{spawned}"), "opencode", true, "", "");
        let worker = sandbox.worker_config("opencode", "");
        let turn = |control: bool| {
            let provider = seam(
                OneharnessProvider::new()
                    .with_worker_config(&worker)
                    .with_control(control),
                spawned,
            );
            let outcome = Engine::new(&provider, Settings::new().with_session_name("wc-ladder"))
                .run(&Conversation::single_turn(sandbox.skill(), "report"))
                .expect("every rung runs under the worker config");
            let reply = outcome.transcript.messages[1].content.clone();
            (reply, outcome.into_report(vec![]))
        };
        let (opened, _) = turn(false);
        assert_eq!(opened, worker_only(), "spawned: {spawned}");
        let (continued, report) = turn(true);
        assert_eq!(continued, worker_only(), "spawned: {spawned}");
        let reason = report
            .control_unavailable
            .expect("the control rung was refused, so the turn was retried");
        assert!(
            reason.contains("implements no resume request"),
            "spawned: {spawned}: {reason}"
        );
    }
}

#[test]
fn without_a_worker_config_the_project_file_layers_over_the_user_file() {
    for spawned in [false, true] {
        let sandbox = Sandbox::new(
            &format!("wc-discovery-{spawned}"),
            "claude-code",
            false,
            "",
            "",
        );
        let provider = seam(OneharnessProvider::new(), spawned);
        let (reply, openings) = worker_turn(&provider, &sandbox);
        // Each file's own key, and the project's value for the key both set.
        assert_eq!(
            reply,
            format!(
                "{SHARED}=project {USER}=user {PROJECT}=project {WORKER}=unset {INDIRECT}=unset"
            ),
            "spawned: {spawned}"
        );
        // Neither file sets `history`, so onejudge's default holds.
        assert_eq!(openings, 1, "spawned: {spawned}");
    }
}

#[test]
fn without_a_worker_config_either_discovered_file_can_turn_history_off() {
    for spawned in [false, true] {
        for (layer, user_top, project_top) in [
            ("user", "history = false", ""),
            ("project", "", "history = false"),
        ] {
            let sandbox = Sandbox::new(
                &format!("wc-discovered-quiet-{layer}-{spawned}"),
                "claude-code",
                false,
                user_top,
                project_top,
            );
            let provider = seam(OneharnessProvider::new(), spawned);
            let (reply, openings) = worker_turn(&provider, &sandbox);
            assert!(reply.contains(&format!("{USER}=user")), "{reply}");
            assert_eq!(
                openings, 0,
                "`history = false` in the {layer} file sits above onejudge's default \
                 (spawned: {spawned})"
            );
        }
    }
}
