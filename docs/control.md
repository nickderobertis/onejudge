# Out-of-band turn control

A dispatched agent turn runs for many minutes. Until now the only lever anything
above onejudge had over one that went the wrong way was to kill it — losing the
turn, its session, and everything it had learned. A correction written at the
right moment had to wait for the next dispatch, by which point the worker had
already finished the wrong work.

oneharness 0.6.14 provides the lever: `oneharness run --control` opens a unix
socket for the run's lifetime, and a **separate** `oneharness interrupt` process
aborts the in-flight turn and delivers a replacement message in one operation.
onejudge's advertised floor is the pinned `oneharness-core`, currently **0.8.0**.

onejudge's part is deliberately narrow, and it is all of this page:

1. **Ask** for a controllable agent turn.
2. **Report the address** of the one you got.

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
no socket, no extra process. `true` adds `--control` to the **agent-side**
`oneharness run` only. The judge and simulated-user calls are short scoring turns
with nothing to redirect, and giving them a socket would put two runs on one
address.

`control` belongs to the `oneharness` provider kind. Under `split`, set it on the
`skill:` child — the side that runs the controllable turn — not on the wrapper,
which has two backends and so no single turn to address.

## The address

The report gains one nullable block:

```json
"control": {
  "session": "run-42-skill",
  "session_dir": "/home/you/.local/state/oneharness/sessions",
  "cwd": "/work/repo"
}
```

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
carries the reason. The reason is quoted from oneharness's own refusal wherever
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
| **Windows** | onejudge's own, before the call: the socket is a unix domain socket |

Windows is answered by onejudge rather than by oneharness on purpose. There is
nothing for a retry to discover there, so the ask degrades to a stated reason
instead of spending a process on a refusal.

The refusal set is matched on oneharness's stderr text — these are usage errors
raised before a report exists, so there is nothing typed on the wire to match.
`control_refusal_markers_track_oneharness` pins the whole set against
`OneharnessError`'s own rendering, so an upstream rewording fails the gate here
rather than silently turning a degraded run into a failed one.

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

## Scope

This is the bottom of the fix, not the whole of it. onejudge does not interrupt,
does not decide when a turn has gone wrong, and does not publish the address
before the run ends — the report is where it appears. A supervisor that needs the
lever mid-run already knows the session name and working directory it configured;
what it could not know until now is the store the socket lives in, and whether it
got a controllable turn at all.
