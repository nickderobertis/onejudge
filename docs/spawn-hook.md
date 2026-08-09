# The spawn hook: grouping what onejudge spawns, in-process

`SpawnHook` lets an **in-process embedder** place every process onejudge creates
into a process group the embedder owns, so cancelling a run still reaps the whole
harness tree.

## The problem it solves

Driving onejudge as a *subprocess* supplied OS-level grouping for free: an
embedder spawned one `onejudge`, put that one process in a group, and everything
onejudge went on to create inherited it. Driving onejudge as a *library* silently
removes that. The `oneharness` processes are now created by the embedder's own
process, inside whatever group it happens to be in, and a cancel can no longer
name a tree to terminate.

The consequence is not cosmetic. An agent harness that survives a cancellation
keeps calling the model and keeps billing tokens for a run nobody is watching. On
Windows in particular, `cancel --kill` cannot reap a two-party (worker + judge)
harness tree at all, because the processes are in no job object the canceller
holds a handle to.

This is the same shape one layer down: converting onejudge to drive `oneharness`
as a library removed the descendant teardown the subprocess boundary had been
providing, and oneharness had to grow signal-aware teardown (`Finish::Terminate`,
v0.6.9) to restore it — see [oneharness-library.md](oneharness-library.md). Each
hop that moves in-process gives up something the boundary was quietly supplying.

## The design, in one line

**The embedder owns the group; onejudge only offers the process.** onejudge takes
no grouping policy of its own — the embedder is the party that must later
terminate the group, and it may already have one that spans more than this run.

## The two halves

Grouping is spelled differently per platform, and the two spellings need different
moments. Both trait methods default to doing nothing, so a hook implements only
the half its platform needs and stays portable.

| | when | what it hands you | used for |
|---|---|---|---|
| `spawning` | before the fork/`CreateProcess` | `&mut std::process::Command` | POSIX: `CommandExt::process_group`. Windows: creation flags. |
| `spawned` | after the process exists, **before onejudge writes its request** | `&std::process::Child` | Windows: `AssignProcessToJobObject` on `AsRawHandle`. Recording pids either way. |

`spawned` returns `Ok(Option<String>)` — the label of the group it placed the
process in, or `None` for "deliberately not grouped".

### "Before spawned processes begin work"

`spawned` runs after the child exists but before onejudge writes the request to
its stdin, and **every** process onejudge spawns blocks reading stdin until that
write: `oneharness run --prompt-file -` on the oneharness backend, and the single
request object of the JSON-lines protocol on a `CommandProvider`
([protocol.md](protocol.md)). So a child observed in `spawned` has not run a
harness and has not created a descendant that could escape the assignment.

## Installing one

```rust
use std::sync::Arc;
use onejudge::{Engine, OneharnessProvider, Settings, SharedSpawnHook, SplitProvider};

let hook: SharedSpawnHook = Arc::new(MyGroup::open()?);
// Install the SAME hook on both backends so one group spans the whole
// two-party tree.
let provider = SplitProvider::new(
    OneharnessProvider::new().with_spawn_hook(hook.clone()),
    OneharnessProvider::new().with_spawn_hook(hook.clone()),
);
let engine = Engine::new(&provider, Settings::new());
```

`CommandProvider::with_spawn_hook` is the same, for a custom backend.

### …when you drive a `Plan`

An embedder that drives onejudge through the CLI's run driver (`Config` →
`Plan` → `run_plan` / `run_plan_streaming*`) never builds a provider itself, so
it installs the hook on the **plan** instead:

```rust
use onejudge::cli::{run_plan, Config, Format};

let plan = Config::from_yaml(&yaml)?
    .into_plan()?
    .with_spawn_hook(hook.clone());   // reaches every process the plan spawns
let summary = run_plan(plan, Format::Json, &mut |_| {})?;
```

This is the same seam, not a second one: the plan installs the hook on whichever
backend its `provider:` names, and on **both** children of a `split` — so one
group spans the whole two-party worker + judge tree, which is exactly what a
`cancel --kill` otherwise leaks. A plan with no hook is unchanged behaviour, and
`onejudge run` itself installs none (a command line cannot name an in-process
hook; a shell embedder groups the `onejudge` process instead).

## What it does *not* do silently

- **No hook installed → today's behaviour, unchanged.** onejudge spawns into its
  own group and claims nothing.
- **A hook that fails is loud.** An error from either method fails the operation
  with `ProviderErrorKind::Spawn`, and a `spawned` failure tears the child down
  first. A process the embedder could not group is a harness it could not cancel,
  so running it anyway would be the very defect this exists to prevent.
- **onejudge never reports a group that does not exist.** The `group` field on a
  `processes` record is populated only from what a hook returned.

## What it makes observable, including from the CLI

Every run reports the processes it spawned on `Report::processes` — and therefore
in `onejudge run --format json`, and on the `FailureReport` a failed run writes
instead ([contract.md](contract.md)):

```jsonc
"processes": [
  { "role": "agent", "op": "respond", "program": "oneharness", "pid": 41231,
    "group": "job:run-1" },
  { "role": "judge", "op": "judge", "program": "oneharness", "pid": 41244 }
]
```

`Engine::spawned_processes()` returns the same records mid-run and after a
*failed* run, so a caller that has to clean up knows what was created.

## How it is proven

One journey per entry point an embedder has, both driving onejudge as a library
over the real subprocess boundary: a hook that makes each spawned process its own
POSIX process-group leader, a two-party run where each party's harness stand-in
**outlives** the `oneharness` process that spawned it, a `killpg(SIGKILL)` of the
groups the hook handed back, and an assertion — from outside the tree, over each
stand-in's liveness socket — that every one of them is gone.

- `tests/e2e.rs`'s
  `an_embedder_group_reaps_the_whole_two_party_harness_tree_on_a_kill_cancel`
  builds the providers itself (`SplitProvider` + `with_spawn_hook`).
- `tests/cli.rs`'s
  `a_plan_driven_embedders_group_reaps_the_whole_two_party_harness_tree_on_a_kill_cancel`
  drives a **plan** (`Config` → `into_plan` → `with_spawn_hook` → `run_plan`),
  the entry point a plan-driven embedder actually uses.

The shared helpers live in `tests/support/mod.rs`.

Without the seam neither test can be written: the spawned processes sit in
onejudge's own group, which is the caller's, so the only available `killpg` would
take the caller with it. Removing just the `process_group` call from the hook — or,
for the plan journey, the hook's install in the run driver — makes the test fail on
exactly the orphaned-harness assertion.
