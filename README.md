<p align="center"><img src="assets/logo.png" alt="apytti" width="180"/></p>

# apytti

"A pity" the AI CLIs don't share an API. So here's one.

A unified REST gateway over Claude, Copilot, Gemini, and Ollama. One binary, one endpoint, four backends.

> ⚠️ **Heads up — Claude billing change (2026-06-15)**
>
> Starting **June 15th, 2026**, Anthropic is billing `claude -p` (non-interactive / headless mode) **separately from Claude Pro/Max subscriptions** — every call goes against API credits equivalent to your sub price instead of being covered by your seat.
>
> Because apytti drives Claude exclusively through `claude -p`, **using this harness with the `claude` backend after that date will bill you per token on top of your subscription**. If you're on Pro/Max and expected flat-rate coverage, **don't point production traffic at apytti's claude backend from June 15th onward**.
>
> The Copilot, Gemini, and Ollama backends are unaffected.


## Features

- Single REST API in front of `claude`, `copilot`, `gemini`, and Ollama (HTTP)
- Per-request backend / model / effort / dir / agent / command override
- **SSE streaming** with normalized events (`delta`, `tool_use`, `tool_result`, `done`, `error`) across every backend
- **Attachments** (`path` or base64 `data`) for images, audio, video, and documents — voice notes from Telegram-bridged agents Just Work
- **Cancel endpoints**: `POST /backends/{name}/sessions/{sid}/cancel` for one session, `POST /requests/{request_id}/cancel` for one call (the only way to cancel a sessionless one), `DELETE /api/ask` as kill switch — aborts drop the worker, `kill_on_drop(true)` SIGKILLs the subprocess
- **Sessions API**: list, inspect messages with `?since=N` for incremental polling, delete
- **Config UI**: `/config-ui` is a self-contained HTML settings page for first-run setup without hermytt
- Stateless gateway (sessions managed by the CLIs themselves; Ollama sessions kept in memory)
- Library API for Rust crates that want to call any backend programmatically
- Daemon install for macOS (LaunchDaemon **or** signed/notarized `.app` bundle), Linux (systemd), Windows (sc)
- **Self-update** for the macOS `.app`: hourly background check, `POST /update/apply` swaps the bundle with no admin prompt, verified against SHA-256 + Developer ID + pinned Team ID, with automatic rollback if the new build doesn't come up healthy

## Install

```bash
cargo build --release
# Binary at target/release/apytti
```

For the production macOS bundle (signed + notarized + stapled `.pkg` for `/Applications/Apytti.app`):

```bash
VAULT_TOKEN=... ./build-pkg.sh
# Output: target/apytti-<version>.pkg
```

Then configure backends:

```bash
apytti setup
```

Interactive menu — pick which backends to enable, set defaults (model, effort, skip-perms, etc.), choose the active default. Config saved to `~/.apytti/config.toml`.

Or just open `http://localhost:7781/config-ui` in a browser after first launch.

## Run

```bash
apytti                       # default: starts the server
apytti run --port 7781       # explicit
apytti setup                 # interactive backend config
apytti install               # generate OS daemon (launchd/systemd/sc)
apytti uninstall             # remove daemon
apytti status                # daemon install + running state, as JSON
apytti init-models           # probe enabled backends, cache to ~/.apytti/models.json
apytti --help                # full reference
```

## Server flags

```
apytti run [OPTIONS]

  --port <PORT>      Listen port (default: 7781)
  --host <HOST>      Bind address (default: 0.0.0.0)
  --localhost        Bind to 127.0.0.1 only
  --verbose          Log requests + responses + timing
  --no-menu          macOS: skip the menu-bar wrapper, run headless (dev/test)
```

Override config path with `--config <PATH>` at any subcommand.

On macOS the `.app` bundle tees logs to `~/Library/Logs/Apytti/apytti.log` (the menu-bar "Open Log" item points there).

## REST API

Full contract is in [API.md](API.md). Highlights below.

### POST /api/ask

```bash
curl -X POST http://localhost:7781/api/ask \
  -H 'Content-Type: application/json' \
  -d '{"prompt": "translate hello to french", "backend": "ollama"}'
```

Request:
```json
{
  "prompt": "your question",
  "backend": "claude",
  "session_id": "uuid-from-previous-call",
  "model": "sonnet",
  "effort": "low",
  "stream": false,
  "dir": "/srv/project-foo",
  "agent": "infrakid",
  "command": "review",
  "request_id": "flow-42-step-3",
  "timeout_secs": 900,
  "attachments": [
    { "path": "/abs/path/kitchen.jpg", "kind": "image" },
    { "data": "<base64>",              "kind": "audio", "name": "voice.ogg" }
  ]
}
```

Either `prompt` or non-empty `attachments` is required. Everything else is optional. `backend` defaults to the configured active.

Response:
```json
{
  "response": "Bonjour",
  "session_id": "uuid-for-next-call",
  "cost_usd": 0.05,
  "backend": "claude",
  "error": null
}
```

With `"stream": true` you get SSE instead — events are `delta`, `tool_use`, `tool_result`, `done`, `error`.

### Cancellation

```bash
# Cancel one in-flight call by (backend, session_id)
curl -X POST http://localhost:7781/backends/claude/sessions/<sid>/cancel
# → {"killed": 1}

# Cancel one call by caller-chosen request_id — works for sessionless calls
curl -X POST http://localhost:7781/requests/flow-42-step-3/cancel
# → {"killed": 1}

# Kill switch — abort everything
curl -X DELETE http://localhost:7781/api/ask
# → {"killed": 3}
```

Aborts drop the worker future; `kill_on_drop(true)` on every backend `Command` SIGKILLs the underlying subprocess.

Pass `request_id` in the `/api/ask` body to make a call cancellable. Sessionless calls need it — without a `session_id` they're registered under an internal key the caller never sees, leaving `DELETE /api/ask` as the only alternative, which takes out unrelated work too.

Note that dropping the HTTP connection cancels nothing: no part of the request path watches for client disconnect, so a client-side timeout leaves the subprocess running to completion. Call a cancel endpoint explicitly.

### Timeouts

Every call has a deadline — `timeout_secs` on the request, else the backend's `timeout_secs`, else **900s**. It covers the whole call (queueing for the session lock *and* the backend run); on expiry the worker is aborted, the CLI is SIGKILLed, and the caller gets `504`.

It exists because the per-session mutex is only released when the handler returns. A CLI that *fails* releases it fine; one that *hangs* never did — so a single stuck call used to wedge every later request to that `session_id` silently and indefinitely. The default is generous on purpose: this is a deadlock guard, not a latency policy.

If you are queued behind a stuck call, the `504` names the cancel endpoint that clears it.

### Self-update (macOS `.app` only)

```bash
curl http://localhost:7781/update                 # cached status
curl 'http://localhost:7781/update?check=true'    # force a fresh check
curl -X POST http://localhost:7781/update/apply   # verify, swap, restart
```

Checks run in the background hourly and never install anything on their own — applying is always an explicit call. No admin prompt: `/Applications` is group-`admin` writable and macOS doesn't stop an app replacing *itself*, so the pkg payload is unpacked and swapped in with atomic renames instead of running `installer`.

Because `installer` isn't in the loop, nothing else validates the download, so apytti does it all itself first: SHA-256 against the release's `SHA256SUMS`, `codesign --verify --deep --strict`, Gatekeeper assessment, **Team ID pinned to `XJQQCN392F`**, and a bundle-identifier match against the running app. Any failure aborts and keeps the current version.

The old bundle is kept as `Apytti.app.previous` until the new one answers `/health` with the expected version; if it doesn't within 60s, the previous build is restored automatically. The `.pkg` is still the first-install path — it lays the `/usr/local/bin/apytti` symlink and needs admin once.

### Sessions

```bash
GET    /backends/{name}/sessions                         # list
GET    /backends/{name}/sessions/{sid}/messages?since=N  # incremental — returns total
GET    /backends/{name}/sessions/{sid}/status            # is anyone interactive on this session right now?
DELETE /backends/{name}/sessions/{sid}                   # delete
```

### GET /health

```json
{
  "status": "ok",
  "version": "0.6.13",
  "active_backend": "claude",
  "enabled_backends": ["claude", "ollama"],
  "update": {
    "current": "0.6.12",
    "latest": "0.6.12",
    "available": false,
    "supported": true,
    "checked_at": "2026-08-14T09:12:03Z"
  }
}
```

The `update` block appears once a self-update check has run, so callers can tell "no update" from "haven't looked yet".

### GET /help

Full HTML API documentation served from the binary.

### GET /config-ui

Self-contained HTML settings page. Reads `/backends/schema` + `/config` + `/health` and PUTs back to `/config`. Lets a standalone apytti install (no hermytt) do first-run setup from a browser instead of editing `~/.apytti/config.toml` by hand.

### GET /config / PUT /config

Returns the current `PersistedConfig` as JSON. All four backends always present even when disabled. Tokens redacted to `"***"` on read.

`PUT` accepts the same shape, merges into current config, persists to `~/.apytti/config.toml`. Partial updates supported. Returns `{"ok": true}`.

Auth: if `hermytt.config_token` is set, requires `X-Hermytt-Key: <token>` header. Otherwise open.

### GET /backends/schema

Static description of each backend's configurable fields with type hints. Lets web UIs render forms without hardcoding apytti-specific knowledge.

## Configuration

`~/.apytti/config.toml`:

```toml
active = "claude"

[backends.claude]
enabled = true
model = "sonnet"
effort = "low"
skip_permissions = true
allow = ["Bash(git:*)"]

[backends.copilot]
enabled = true
model = "claude-sonnet-4.6"

[backends.gemini]
enabled = false

[backends.ollama]
enabled = true
endpoint = "http://localhost:11434"
model = "llama3.2"

# Optional: where attachment paths must live (defense in depth — apytti only
# reads attachments under one of these roots; data-form attachments materialize
# under ~/.apytti/inbox/ with a 5-minute TTL).
attachment_roots = ["/Users/cali", "/tmp"]

# Optional: announce to hermytt registry for the family command center
[hermytt]
url = "http://mista:7777"
token = "..."          # X-Hermytt-Key header for /registry/announce
config_token = "..."   # optional; required header for PUT /config writes
endpoint = "..."       # optional; defaults to http://<hostname>:<port>
```

Use `apytti setup`, `/config-ui` in a browser, or `PUT /config` from a remote tool (like hermytt's UI).

## Library

```rust
use apytti::{dispatch, AskRequest, BackendKind, BackendConfig};

let cfg = BackendConfig {
    enabled: true,
    model: Some("sonnet".into()),
    skip_permissions: true,
    resume: true,
    ..Default::default()
};

let req = AskRequest {
    prompt: "hello".into(),
    ..Default::default()
};

let resp = dispatch(BackendKind::Claude, &cfg, &req).await;
println!("{}", resp.response);
```

## Daemon install

```bash
# Basic
apytti install --port 7781

# Full options (used by hermytt for remote spawn)
apytti install \
  --port 7781 \
  --host 127.0.0.1 \
  --dir /srv/project-foo \
  --hermytt-url http://mista:7777 \
  --hermytt-token <token>

# Inspect installed daemon
apytti status   # prints JSON: installed, running, version, platform, paths

# Remove
apytti uninstall
```

On macOS, `build-pkg.sh` produces a signed/notarized `Apytti.app` that lives in `/Applications`, runs as a menu-bar agent, and registers itself with launchd as a per-user GUI app (label `application.net.calii.apytti.app.<...>`). The menu bar exposes Settings…, Open Log, Open Config Folder, Open Help.

## Backend mapping

| | Claude | Copilot | Gemini | Ollama |
|---|---|---|---|---|
| Subprocess | `claude` | `copilot` | `gemini` | (HTTP) |
| Endpoint | — | — | — | `localhost:11434` |
| Output (apytti uses) | single JSON | JSONL stream | single JSON | HTTP `/api/chat` (non-stream) |
| Streaming option | yes (`stream-json`) | yes (default) | yes (`stream-json`) | yes (HTTP `stream:true`) |
| Tool-use stream events | yes | — | — | — |
| Sessions | `--resume` | `--resume=` | `--resume` | in-memory store |
| Skip perms | `--dangerously-skip-permissions` | `--allow-all` | `--yolo` | n/a |
| Effort | yes | yes | n/a | n/a |
| Cost reporting | yes (API key) | n/a | n/a | n/a |
| `kill_on_drop` | yes | yes | yes | n/a (HTTP) |

## Cross-compile

```bash
# Linux x86_64 (static, musl)
cargo build --release --target x86_64-unknown-linux-musl

# Windows x86_64
cargo build --release --target x86_64-pc-windows-gnu

# macOS Intel
cargo build --release --target x86_64-apple-darwin
```

## Part of the YTT family

Apytti is the simplest member. She uses each CLI's `-p` non-interactive mode and skips all the TUI parsing her sister [grytti](../grytti) does. Pyttch-bridge routes Telegram voice notes through `POST /api/ask` with `attachments[]`; downstream agents (Lou, infrakid, etc.) read the materialized files within the 5-minute inbox TTL.

## License

MIT
