# Notes: delivering a role-addressed correction into a running conversation

A note is a correction sent into a conversation that is already running. The seam
exists because the two ways of doing it before reached one party each: a note
delivered to the live *agent* turn never reached the judge, while an amendment
bound the judge and could not interrupt. Neither did both, so a worker and its
judge ended up holding two instructions of equal authority — one measured ruling
was contradicted by the node's own judge seven minutes later, reviewing against a
task that never mentioned it.

Everything here is on the **library surface**. No command line is required at any
layer.

```rust
use onejudge::{Addressee, Conversation, Engine, Note, Notes, Settings};

let (notes, inbox) = Notes::channel();
let engine = Engine::new(&provider, Settings::new()).with_notes(inbox);
// …or, driving the run through a plan: `plan.with_notes(inbox)`.

// From whatever supervises the run, on another thread:
notes.send(Note::to(Addressee::Worker, "the reviewer asked for a smaller diff"))?;
```

## The rules

**The note reaches whoever is live.** Not whoever it is addressed to — the
addressee is what the *recipient is told*, so a judge handed an update to the
worker's task knows it is one and does not take the worker's job on. One measured
run saw a simulated user compose a four-point "manager ruling" in-conversation and
instruct the worker to post it over the run channel, and the worker complied.

**The other party receives it with the response.** A note that reached the worker
is in the next `SupervisorQuery` the judge answers, alongside the worker's response
to it. A note that reached the judge is prepended to the user turn the judge's own
message becomes, so the worker cannot act on one without the other.

**If the judge's response is completion, nothing is delivered to the worker.** The
judge passed the work with the note in hand; there is nothing left to correct. The
sender is told so — `Accepted::JudgedWith { completion_reason }` — and that is not
an error.

**Between turns, the next turn gets it** (`Accepted::Queued`).

**A note that arrives once the conversation has completed raises.** `Notes::send`
answers `Err(Undelivered::…)`, naming that it was not delivered and why, so the
caller chooses relaunch, tweak or follow-up. The silent half is worse than the loud
half: a note that is accepted and never delivered leaves the caller believing the
correction landed. One measured node accepted a note after its worker had reported
completion, did another forty minutes of correct work, and was then failed for a
completion report that preceded its own subsequent commits.

## The four cases, and what the sender is told

| the note arrives | the worker | the judge | `Notes::send` answers |
| --- | --- | --- | --- |
| **during the worker's turn** | the turn is reopened carrying the note, before the supervisor is consulted | receives it **with** the worker's response: the next `supervise` carries it in `SupervisorQuery::notes`, and a bound criterion in the criterion in force | `Accepted::Interrupted { party: Worker }` |
| **during the judge's turn**, answering `Continue` | receives it **with** the response: one user turn carrying the note's framing and then the supervisor's own words | its decision is discarded and re-taken with the note in hand; that decision is the one that counts | `Accepted::Interrupted { party: Supervisor }` |
| **during the judge's turn**, answering `Completed` | **nothing is delivered** | as above, and the completion was decided with the note in hand | `Accepted::JudgedWith { completion_reason }` |
| **between turns** | the next turn's prompt carries it | the next `supervise` carries it | `Accepted::Queued` |
| **after the conversation ended** | — | — | `Err(Undelivered::…)` |

Two costs, stated where an implementer meets them. A redirected judge turn is a
**second judge invocation**, so a caller sending notes into a tight loop pays per
note. And `Notes::send` blocks until the disposition is known — immediately when
nothing is live, and for a live turn until that party has been handed the note
(for the judge, until its re-taken decision comes back, because that decision is
what distinguishes the last two rows). Send from a thread other than the one
driving the engine.

## What "live delivery" means

An interrupt has never delivered *into* a running turn. oneharness's own control
channel aborts the turn and reopens the next one on the same session with the
message as its prompt — "committed with the abort, delivered at the turn boundary",
because the agent CLIs discard or refuse a mid-turn frame. onejudge's in-process
engine keeps that semantics and the ordering guarantee it is for, at the seam it
owns:

* the **worker's** finished reply is kept rather than discarded — a redirect throws
  away a turn's words, never the work it already committed — and the note is the
  very next thing the worker is handed, before any other party is consulted;
* the **judge's** decision *is* discarded and re-taken, because a decision taken
  without the note is exactly the decision the note exists to change.

## A note that moves the bar

`Note::binding(criterion)` makes the note part of what the finished work is judged
against. Omit it — which is what a caller that says nothing gets — and the note
reaches its addressee, is shown to the judge as context, and touches no criterion.

A bound criterion enters the criterion in force at **both** judging sites: the
per-turn supervisor decision, and the authoritative re-judge against the finished
transcript that decides whether the task completed (`Engine::criteria`, read by
`cli::run_plan`). A note that reached only the first would be invisible to the
verdict that decides `done` versus failed.

Two fields rather than one, deliberately. `Note::text` is what the worker reads and
`Note::criterion` is what the judge is held to, because a single field forces one
string to be both an explanation a worker acts on and a property a judge evaluates
— and under time pressure it is written as the first. Two measured nodes were
failed on exactly that: a criterion that named the mechanism its author had in mind
("assert that the script *refuses* an invalid label") failed an implementation that
reached the same property another way.

`Criterion` is validated where the note is built, so a note carrying an unusable
criterion is unrepresentable rather than refused somewhere later. The rules are the
ones an orchestrator already enforces on authored plan criteria — the upstream is
`orchestrator/criteria_guard.py` in `nickderobertis/ai-orchestrator`, and this is a
port of it:

| refused | why |
| --- | --- |
| blank after trimming | a bar nobody can clear |
| a version literal | it perishes between writing and judging; mid-run it perishes faster |
| a `just`/shell invocation, or a chained command | a judge fails the spelling |
| a demand for a particular string | the property can be met and the wording failed |
| deferral to prose elsewhere | a criterion the judge reconstructs is reconstructed as a wording demand |
| work the dispatch cannot do | "the PR is merged" fails finished work |

**What these rules do not catch, stated plainly rather than implied:** "this is a
mechanism the author preferred rather than a property the node owes" needs the
author's intent, and no pattern has it. Neither of the two incidents above would be
refused by any rule in the table. What stands in for it is the rendered framing —
the criteria the judge is given carry the sentence *"Where an item names a
mechanism rather than a property, judge the property that mechanism was serving —
work that reached the same property another way has met it."*

Because the rules are a port, they exist in two places. Keeping them reconciled is
a gate on the **upstream** side, where both rule sets are visible; this crate pins
its own half with `tests/notes.rs`.

## Also true, and load-bearing

The judge is shown the worker's **full task, including any amendment in force**.
`SupervisorQuery::task` is the conversation's input verbatim — an orchestrator that
folds an amendment into the task it dispatches has already put it there — and a
mid-run amendment is a note addressed to `Both` that binds a criterion, so it
reaches the judge on the bar rather than only in the prose.
