# onejudge-llmlint-real — the real-llmlint tier

- **External tier, out of the gate**, against the *released* llmlint at the
  provider's floor. Its config's only rule matches no file, so it never calls a
  model. An absent llmlint fails it — never make it skip.
- **Nothing may depend on it** (`type:external`).
