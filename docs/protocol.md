# The CommandProvider JSON-lines protocol

`CommandProvider` speaks a small, stable protocol so any command — a custom
backend, or the deterministic test doubles — can act as a `Provider`. It is a
one-shot exchange per operation: onejudge spawns the command, writes **one JSON
request object** and a newline to the child's stdin, closes stdin, and reads
**one JSON response object** from stdout. A non-zero exit, empty stdout, or
unparseable/wrong-shaped output is a loud error (a classified
`ProviderErrorKind::Protocol` / `Spawn`), never a silent empty turn.

All five operations are distinguished by the request's `op` field.

This is not the only provider protocol: an `oneharness`-kind provider can instead
publish its agent turn incrementally, as NDJSON tool events followed by a terminal
report line. See [streaming.md](streaming.md). The two are independent — a
`CommandProvider` exchange is unchanged by it.

## Protocol version

**v5** (current) — added `notes` to the `supervisor` request: the role-addressed
corrections delivered into the run so far (see [notes.md](notes.md)). The field is
omitted when nothing has been delivered, so a v4 command sees a byte-identical
request and needs no change. **v4** added the unified `supervisor` operation. **v3** added the
`assess` free-text judgement operation. **v2**
dropped `platform` and `model` from every request: harness and
model **selection** is the command's own concern now (onejudge no longer passes
it). v1 carried `platform`/`model` on `respond` and `model` on `user`/`judge`.

## `respond` — run one skill turn

Request:

```json
{
  "op": "respond",
  "skill": { "name": "greeter", "path": "/skills/greeter", "instructions": "..." },
  "messages": [ { "role": "user", "content": "hi" } ],
  "session": "run-42-skill"
}
```

- `session` is the caller-owned name the engine threads across turns; the engine
  always sends it, so omit it from the request only when it is `None` (a stateless
  provider ignores it and reads the inlined `messages`).
- `messages` is the transcript so far; each message is `{role, content, events?}`
  where `role` is `user` / `assistant` / `system`.

Response:

```json
{
  "message": "Hello! How can I help?",
  "done": false,
  "usage": { "input_tokens": 12, "output_tokens": 8, "cache_read_tokens": 4, "cache_write_tokens": 0, "cost_usd": 0.0 },
  "events": [
    { "kind": "tool_call", "name": "bash", "input": { "command": "ls" }, "index": 0 }
  ]
}
```

- `message` (required) is the assistant reply.
- `done` (default `false`) signals the skill considers the task complete.
- `usage` (optional) — any subset of `input_tokens`, `output_tokens`,
  `cache_read_tokens`, `cache_write_tokens`, `cost_usd`; omit what you can't report
  (`null`/absent means "no signal", never zero).
- `events` (optional) — the normalized tool events the skill took this turn;
  each is `{kind, name?, input?, output?, index}`. They are attached to the
  assistant turn so the judge and `ToolQuery` can reason over them.

## `user` — produce one simulated-user turn

This operation remains for API compatibility and explicit role-play calls. The
engine's per-turn loop uses `supervisor` below.

Request:

```json
{ "op": "user", "persona": "A hurried shopper.", "messages": [ ... ], "session": "run-42-user" }
```

Response:

```json
{ "message": "And can I get it by Friday?", "stop": false, "usage": { ... } }
```

- `stop` (default `false`) ends the conversation.

## `supervisor` — decide completion or produce the next user turn

The engine sends exactly one request after each ordinary nonterminal agent turn:

```json
{"op":"supervisor","task":"fix it","persona":"A strict reviewer.","done_when":"tests pass","worktree":"/repo","history_name":"run-42-skill","messages":[...],"session":"run-42-user"}
```

`done_when` is the criterion **actually in force**: the configured one plus every
criterion a delivered binding note added, composed by onejudge. `notes` (v5,
omitted when empty) carries what each party was told, so a command that renders its
own judge prompt can frame them by the role they address rather than reading them
as instructions to itself:

```json
{"op":"supervisor","...":"...","notes":[{"note":{"addressee":"worker","text":"the reviewer asked for a smaller diff","criterion":"the diff touches only the migration"},"delivered_to":"worker"}]}
```

`addressee` is who the note is *for* (`worker` / `supervisor` / `both`);
`delivered_to` is which party was handed it, and the two differ whenever a note
reached whichever party happened to be live. See [notes.md](notes.md).

Return exactly one discriminated shape. Completed requires a non-empty reason and
forbids `message`; continue requires the exact non-empty next user message:

```json
{"completion":true,"reason":"all required tests passed","usage":{...}}
{"completion":false,"message":"Run the integration suite too.","reason":"unit tests alone are insufficient","usage":{...}}
```

`message` reaches the agent **verbatim** as its next user turn and is the only
thing it receives, so `completion:false` obliges you to name a concrete, actionable
next instruction. A continue with an absent or blank `message` is not a protocol
error — onejudge repeats the identical request up to `SUPERVISOR_REASK_LIMIT`
times, and if the answer is still unusable the run *settles* on the work it has
(`settled_reason` in the report, see [contract.md](contract.md)) rather than
failing. A `message` that is *present* but keeps asking for nothing settles the run
too: `NOOP_SETTLE_LIMIT` consecutive exchanges that record no tool activity and
return the same tiny answer to the same tiny instruction end the loop on the work
it has, rather than re-prompting a finished task to the turn cap. What onejudge
never does is hand the agent an empty user turn. A malformed
response — not one JSON object, a missing `completion`, or a `completed` one
without a `reason` or carrying a `message` — is still a hard protocol error.

The transcript carries compact normalized event summaries, not raw tool dumps.
`worktree` and `history_name` let a backend inspect the full oneharness recording
when needed with `oneharness history show <history_name> --project <worktree>
--format text`.

## `assess` — write a free-text judgement

Request:

```json
{"op":"assess","prompt":"Identify useful follow-up work.","messages":[...]}
```

Response text must be non-empty; usage is optional:

```json
{"text":"Add a regression test for the discovered edge case.","usage":{"input_tokens":120,"output_tokens":12}}
```

## `judge` — score a criterion against the transcript

Request:

```json
{ "op": "judge", "kind": "boolean", "criterion": "the reply was polite", "messages": [ ... ] }
```

- `kind` is `boolean` or `numeric`; a numeric query also carries `min` and `max`.

Response:

```json
{ "value": true, "reason": "the assistant used courteous phrasing", "usage": { ... } }
```

- `value` is a boolean for a `boolean` query and a number for a `numeric` one;
  onejudge type-checks it against the requested `kind`.
- `reason` (optional) is the one-sentence justification.
