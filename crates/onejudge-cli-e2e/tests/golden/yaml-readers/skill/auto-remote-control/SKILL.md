---
# Redacted copy of a real host file (the user's own SKILL.md): identifying values replaced with placeholders, every other byte kept.
name: auto-remote-control
description: Enable, disable, or list Claude Code remote control for a directory on a ExampleCorp cloud dev workspace (AWS WorkSpaces box). Use when the user asks to "enable/turn on remote control for <project>", "make <dir> reachable from my phone", "stop remote-controlling <dir>", "what's remote-controlled here", or to flip the bare-terminal remote-control default.
---

# Auto Remote Control

Manages `claude remote-control` on a ExampleCorp cloud dev workspace: which directories
the always-on server exposes to [claude.ai/code](https://claude.ai/code) and the Claude
mobile app, and whether a bare terminal `claude` joins remote control.

Only meaningful **on a cloud workspace box** — see `infra/docs/cloud-workspaces.md`. The
tool it drives is installed there:

```
/opt/examplecorp/cloud-workspace/remote-control-dirs.sh
```

Fall back to `infra/scripts/cloud-workspace/remote-control-dirs.sh` in a checkout if the
installed copy is missing.

## Mental model

- A remote-control server is **scoped to one directory**. Exposing several means running
  several; the launcher runs one server per configured directory, named after it.
- The set lives in the developer's own file on the user volume (survives a rebuild):
  `~/.config/examplecorp/remote-control.conf`. If none of its directories exist the
  launcher falls back to `$HOME`.
- A directory must be **trusted** before it can be served with nobody at a prompt. The
  tool trusts it for you on `add`.
- Changes apply by restarting the `claude-remote-control` user service — the tool does
  this automatically.

## Do this

Resolve the directory the user means to an absolute path — default to the current repo
root or cwd when they say "this project" / "here". Then run the tool:

| Intent | Command |
| --- | --- |
| Enable remote control for a project | `remote-control-dirs.sh add <dir>` |
| Disable it | `remote-control-dirs.sh remove <dir>` |
| List what's enabled (and whether each exists) | `remote-control-dirs.sh list` |
| Bare `claude` → remote control, on/off | `remote-control-dirs.sh terminal on\|off` |

`add` is idempotent: it adds the directory to the config, trusts it, and restarts the
server. After enabling, tell the user it will appear in the claude.ai/code session list
named after the directory and is reachable from their phone.

## Notes

- `claude-remote-control` is a **system** service (so it survives a WorkSpace disconnect —
  a `systemd --user` service would be killed when the streaming session logs out). Managing
  it needs root: the tool uses `sudo`, which may prompt for a password when you're the
  developer. If the tool prints that it could not restart the service, relay that: the
  change is saved and applies with `sudo systemctl restart claude-remote-control`.
- To confirm it's live: `systemctl status claude-remote-control` should be active, with one
  `claude remote-control … --name <dir>` child per served directory.
