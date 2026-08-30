# Out-of-band turn control

A dispatched agent turn runs for many minutes. Until now the only lever anything
above onejudge had over one that went the wrong way was to kill it — losing the
turn, its session, and everything it had learned. A correction written at the
right moment had to wait for the next dispatch, by which point the worker had
already finished the wrong work.

oneharness 0.6.14 provides the lever: `oneharness run --control` opens a unix
socket for the run's lifetime, and a **separate** `oneharness interrupt` process
aborts the in-flight turn and delivers a replacement message in one operation.
onejudge's advertised CLI floor is **0.11.0+** — the release that embeds the
`oneharness-core` the workspace manifest pins. The two crates version
independently; never read one number off the other.

onejudge's part is deliberately narrow, and it is all of this page:

1. **Ask** for controllable turns.
2. **Report the address** of each one you got — one per party.

onejudge never interrupts anything itself. The lever belongs to whatever is
supervising the run.

## Asking

```yaml
provider:
  kind: oneharness
  control: true          # default: false
```

Or, from the library: `OneharnessProvider::new().with_control(true)`.

The default is `false`, and `false` changes nothing at all — no flag on the argv,
no socket, no extra process. `true` adds `--control` to **both** parties'
`oneharness run`: the agent's turn and the supervisor's decision.

They are two addresses, not one, because the engine mints two session handles —
`<base>-skill` for the agent and `<base>-user` for the supervisor — and oneharness
derives the socket from the session name, so `<dir>/control/<name>.sock` differs
per party. That is the answer to the reason the judge side used to be excluded
("giving it a socket would put two runs on one address"): it does not, for a caller
that never gives both sides one name, which this engine never does. The other
stated reason — "a judge turn is short and has nothing to redirect" — is refuted by
measurement: a judge turn on this host wedged for nearly two hours.

The short stateless calls (`judge`, `assess`) stay uncontrolled: they carry no
`--session` for a socket to be addressed by. So does the legacy `user` turn, which
shares the supervisor's session name and would put a second socket on one address —
the hazard, in the one place it is real.

`control` belongs to the `oneharness` provider kind. Under `split`, set it on the
`skill:` child — the side that runs the controllable turn — not on the wrapper,
which has two backends and so no single turn to address.

## The address

The report gains one nullable block **per party**:

```json
"control": {
  "session": "run-42-skill",
  "session_dir": "/home/you/.local/state/oneharness/sessions",
  "cwd": "/work/repo"
},
"supervisor_control": {
  "session": "run-42-user",
  "session_dir": "/home/you/.local/state/oneharness/sessions",
  "cwd": "/work/repo"
}
```

`control` addresses the **agent's** turn; `supervisor_control` addresses the
**supervisor's** decision. Read the one for the party you mean to redirect: they
are separately opened and separately refused, so a caller that has one and assumes
the other holds a lever it does not have.

Those are exactly the three values `oneharness interrupt` addresses a turn with,
and nothing else:

```sh
oneharness interrupt \
  --session run-42-skill \
  --session-dir /home/you/.local/state/oneharness/sessions \
  --cwd /work/repo \
  --input "stop — fix the failing test first"
```

The socket path is deliberately **not** among them: `interrupt` derives it from
the store directory and the handle (`<session_dir>/control/<session>.sock`), so
reporting it too would be a second source for one fact.

All three are read back from oneharness's own run report rather than re-derived
from what onejudge asked for:

- `session` is the handle oneharness **stored**, which is the caller's name
  sanitized (`Run 42-skill` → `run-42-skill`). An address carrying the
  unsanitized name resolves to nothing.
- `session_dir` is the directory of the socket the run really opened, so it is
  right for a configured store as well as the platform default.
- `cwd` is the string onejudge passed as `oneharness run --cwd`, verbatim, because
  oneharness *slugs that string* to key the session store. Pass it back as it is.

**A fallback chain binds the handle to the candidate that ran**, not to the first
one tried, and the reported address follows it — which matters because `interrupt`
reads that record to decide, before dialling, whether the session's harness can be
interrupted at all. `a_fallback_chain_reports_the_session_of_the_candidate_that_ran`
in `tests/e2e.rs` drives a chain that routes around a control-incapable candidate
and asserts the reported address still passes that pre-flight.

## When there is no lever

`control` is `null` when control was **not asked for**. That is the common case,
and the field is serialized as an explicit `null` (rather than omitted like the
report's other optional fields) so a supervisor can key on its presence instead of
guessing whether it is reading an older onejudge.

A control ask that **could not be honored** is a different fact, and it is
reported as a different thing: `control` is `null` *and* `control_unavailable`
carries the reason. `supervisor_control` / `supervisor_control_unavailable` is the
same pair for the supervisor turn, answered independently — a judge harness with no
control mechanism leaves the agent's address open and standing. The reason is quoted from oneharness's own refusal wherever
there is one, because it names the harness, the run shape, or the control-capable
alternatives a supervisor would use to route around it:

```json
"control": null,
"control_unavailable": "oneharness exited with exit status: 2: oneharness: error: harness `qwen` has no out-of-band turn control, so --control cannot be honored (control-capable: claude-code, codex, opencode, goose, qwen, crush, copilot)"
```

The same reason is warned on stderr as the run happens.

**A refused ask never fails the run.** oneharness validates `--control` before it
spawns anything, so a refusal costs no model tokens — which makes retrying the
same turn without the flag strictly better than losing a judged run to a lever
the caller merely *wanted*. The retry is the same ladder the `--session`
degradation already uses: drop exactly the one thing the previous attempt was
refused for.

Refusals onejudge degrades from:

| Cause | Reported as |
| --- | --- |
| The harness declares no control mechanism | oneharness's `ControlUnsupported` |
| The run shape has no single turn (batch, fan-out, `--schema`, >1 harness) | oneharness's own refusal |
| A server-backed mechanism that cannot also `--stream` | oneharness's `ControlStreamUnsupported` |
| The socket could not be opened | oneharness's `ControlSocket` |
| The harness cannot bind a `--session` at all | onejudge's own: control needs a name to be addressed by |
| The named handle cannot be **continued** over this mechanism | oneharness's `SessionControlNoResume` — see below |
| The socket address would overrun this platform's `sun_path` budget | oneharness's `ControlSocketAddress` |
| **Windows** | onejudge's own, before the call: the socket is a unix domain socket |

Windows is answered by onejudge rather than by oneharness on purpose. There is
nothing for a retry to discover there, so the ask degrades to a stated reason
instead of spending a process on a refusal.

The refusal set is matched on oneharness's stderr text — these are usage errors
raised before a report exists, so there is nothing typed on the wire to match.
`control_refusal_markers_track_oneharness` pins the whole set against
`OneharnessError`'s own rendering, so an upstream rewording fails the gate here
rather than silently turning a degraded run into a failed one.

## A named session, under control

The lever and the handle have to agree about what a turn is, and since oneharness
0.11.0 they are made to. A control mechanism that
**drives the turn over its own protocol** negotiates prompt, model, cwd and
approvals on the wire and builds no argv at all — so the harness's `--resume`
mapping is never reached, and the only way to continue one conversation is the
protocol's own resume request. Codex's app-server has one (`thread/resume`);
OpenCode's, Crush's and ACP's do not. Claude Code's mechanism does not drive the
turn at all — its control frame rides the ordinary headless run — so its handle
travels on the same `--resume` argv it always did.

Where the mechanism cannot carry the handle, a *continuation* is refused before
anything spawns. That refusal is the fix: without it the turn opened a **new**
conversation while the store, the flag and the report all looked healthy, and the
whole transcript was re-sent every turn (measured: a 699 MB single event line).

onejudge degrades from it like any other refused ask — it drops `--control` and
the handle continues on the harness's ordinary headless run. That is oneharness's
own remedy, and it is the right one: the run loses the lever, never the
conversation. Degrading the other way — keeping the flag and taking a fresh
conversation — is exactly what the refusal exists to prevent.

Two journeys in `tests/e2e.rs` drive both halves through the **real** engine, one
per outcome, and each asserts on the native session token the harness was resumed
on — a run that silently started over cannot produce it:

- `a_controlled_turn_resumes_the_named_session_on_a_mechanism_that_carries_it`
  (claude-code): the second, controlled turn replies with the stored token, and
  the run still reports an address.
- `a_controlled_turn_on_a_mechanism_that_cannot_resume_degrades_instead_of_starting_over`
  (opencode): the ask is refused, `control` is `null` with oneharness's own words
  beside it, and the turn still continues the stored conversation.

## What is proven, and how

`the_reported_control_address_is_one_oneharness_interrupt_can_redirect_the_turn_through`
(`tests/e2e.rs`) is the test that makes this contract mean something. It runs a
controlled turn against the `onejudge-fake-oneharness` double — which opens a
**real** unix socket through oneharness's own `io::control` listener over a live
turn's stdin, and writes the real session record — then does exactly what
`oneharness interrupt` does with the reported address, through oneharness's own
code: resolve the store, read the record, run the pre-flight capability check,
send one request frame. It asserts the run answered `ok` **and** that the abort
frame and the operator's replacement message both arrived on the live turn's
stdin. The address is proven *correct*, not merely present.

`the_reported_supervisor_address_redirects_the_judges_live_turn` does the same for
the supervisor's socket, and additionally asserts the two parties' addresses are
different sockets under different stores.
`a_judge_side_control_refusal_degrades_and_leaves_the_agents_lever_alone` drives a
judge harness that declares no control mechanism: the supervisor turn is retried
without the flag, the run reaches its cap, and the agent's address survives a
refusal that was never about the agent.

## A redirected supervisor turn is asked once more

A redirect has never delivered *into* a running turn. It aborts the turn and
reopens the next one on the same session with the message as its prompt. On the
agent side that is invisible — the worker simply answers the correction. On the
supervisor side it is load-bearing, because that reopened turn's reply is the one
`parse_supervisor` reads against a two-shape JSON contract, and what comes back
answers the correction in prose.

So a supervisor turn **that was redirected** — read off oneharness's own
`ControlReport::interrupts`, asking `ControlEvent::is_redirected` so a plain stop
does not count — and whose answer does not parse is asked the question once more,
with the correction named. **Once, not a budget**: a second unparseable answer
fails the member exactly as it always did, because at that point the transport is
broken rather than the turn misaddressed. Both invocations are on the run's usage
and in `telemetry.attribution`, so a manager reading what a run spent sees the two
judge turns rather than one — a judge turn silently taken twice is a cost nothing
else would account for. `a_redirected_supervisor_answer_that_does_not_parse_is_asked_once_more`
and `a_second_unparseable_redirected_answer_fails_the_member_as_it_always_did`
drive the pair.

## Scope

This is the bottom of the fix, not the whole of it. onejudge does not interrupt,
does not decide when a turn has gone wrong, and does not publish the address
before the run ends — the report is where it appears. A supervisor that needs the
lever mid-run already knows the session name and working directory it configured;
what it could not know until now is the store the socket lives in, and whether it
got a controllable turn at all.
