# The CommandProvider JSON-lines protocol

`CommandProvider` speaks a small, stable protocol so any command — a custom
backend, or the deterministic test doubles — can act as a `Provider`. It is a
one-shot exchange per operation: onejudge spawns the command, writes **one JSON
request object** and a newline to the child's stdin, closes stdin, and reads
**one JSON response object** from stdout. A non-zero exit, empty stdout, or
unparseable/wrong-shaped output is a loud error (a classified
`ProviderErrorKind::Protocol` / `Spawn`), never a silent empty turn.

All four operations are distinguished by the request's `op` field.

## Protocol version

**v3** (current) — added the `assess` free-text judgement operation. **v2**
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

Request:

```json
{ "op": "user", "persona": "A hurried shopper.", "messages": [ ... ], "session": "run-42-user" }
```

Response:

```json
{ "message": "And can I get it by Friday?", "stop": false, "usage": { ... } }
```

- `stop` (default `false`) ends the conversation.

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
