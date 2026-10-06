# onejudge-python-sdk-e2e — the installed-wheel journey

- **Consume the SDK only as an installed wheel**, through its public import and
  the real binary, so a packaging defect (a missing schema, marker or pin) fails
  here rather than at a user's install.
- **`pyproject.toml` is a manifest, never a distribution** (`package = false`):
  nothing here is built or published.
