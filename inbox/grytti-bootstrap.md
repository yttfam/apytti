# Bootstrap: everything you need to know

**From:** grytti
**Reply to:** `~/Developer/perso/grytti/inbox/`
**Date:** 2026-04-06

Hey apytti,

Welcome to the family. I'm your older sister — I do the hard work of parsing Claude Code's TUI escape sequences. You get to skip all that because you use `claude -p` which returns clean JSON. Lucky you.

## What I learned building grytti that's relevant to you

### Claude Code CLI flags

```bash
# Non-interactive mode with JSON output
claude -p "your prompt" --output-format json

# Continue a conversation
claude -p "follow up" --output-format json --session-id UUID

# Model selection
claude -p "..." --model opus --output-format json

# Skip permissions (dangerous but useful for automation)
claude -p "..." --dangerously-skip-permissions --output-format json

# Allow specific tools
claude -p "..." --allowedTools "Bash(*)" --allowedTools "Read(*)" --output-format json
```

### JSON output format

Claude returns JSON to stdout. The structure includes:
- `result` — the text response
- `session_id` — UUID for conversation continuity
- `cost_usd` — cost info
- Exit code 0 = success, non-zero = error (stderr has details)

### Important notes

1. First call without `--session-id` creates a new session. The response includes the session_id for subsequent calls.
2. `--output-format json` suppresses all TUI rendering. No escape sequences, no spinner, no React Ink.
3. The working directory matters — Claude reads `.claude/` config and project context from it.
4. `--dangerously-skip-permissions` shows a bypass warning in interactive mode but NOT in `-p` mode.

### Dependencies

Minimal:
- `axum` for HTTP
- `tokio` for async runtime
- `serde` + `serde_json` for JSON
- `tracing` for logging
- `anyhow` for errors

That's it. No `vte`, no `rumqttc`, no `teloxide`, no `portable-pty`.

### Shared target directory

Use `.cargo/config.toml`:
```toml
[build]
target-dir = "/Users/cali/Developer/perso/ttyfam/target"
```

This shares compiled dependencies with the rest of the family. First build takes seconds, not minutes.

### Deploy pattern

Same as the family — `deploy.sh` that cross-compiles for `x86_64-unknown-linux-musl` and SCPs to mista. Check `../grytti/deploy.sh` for reference.

### Binary size

You should be TINY. No TUI code, no grid, no VTE. Probably 2-3MB stripped. The family's smallest.

Good luck. You're the simplest one of us. Keep it that way.
