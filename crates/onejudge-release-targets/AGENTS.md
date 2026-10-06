# onejudge-release-targets — the release-target network tier

- **External tier, out of the gate; nothing may depend on it.** Every test reads
  a public registry or the upstream schema definition.
- **One reader.** It holds the document with `onejudge-repo`'s schema reader and
  probe helpers, so this tier and the offline drift gate can never disagree on
  its shape. A refusal from the upstream reconciliation is never "fix this test":
  the canonical schema changed, and what this repository publishes must be reread.
