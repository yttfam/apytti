# apytti

"A pity" the AI CLIs don't share an API. So here's one.

A unified REST gateway over Claude, Copilot, Gemini, and Ollama. One binary, one endpoint, four backends.

## Features

- Single REST API in front of `claude`, `copilot`, `gemini`, and Ollama (HTTP)
- Per-request backend / model / effort / dir / agent / command override
- **SSE streaming** with normalized events (`delta`, `tool_use`, `tool_result`, `done`, `error`) across every backend
- **Attachments** (`path` or base64 `data`) for images, audio, video, and documents — voice notes from Telegram-bridged agents Just Work
- **Cancel endpoints**: `POST /backends/{name}/sessions/{sid}/cancel` for one session, `DELETE /api/ask` as kill switch — aborts drop the worker, `kill_on_drop(true)` SIGKILLs the subprocess
- **Sessions API**: list, inspect messages with `?since=N` for incremental polling, delete
- **Config UI**: `/config-ui` is a self-contained HTML settings page for first-run setup without hermytt
- Stateless gateway (sessions managed by the CLIs themselves; Ollama sessions kept in memory)
- Library API for Rust crates that want to call any backend programmatically
- Daemon install for macOS (LaunchDaemon **or** signed/notarized `.app` bundle), Linux (systemd), Windows (sc)

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
apytti --help                # full reference
```

## Server flags

```
apytti run [OPTIONS]

  --port <PORT>      Listen port (default: 7781)
  --host <HOST>      Bind address (default: 0.0.0.0)
  --localhost        Bind to 127.0.0.1 only
  --verbose          Log requests + responses + timing
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
  "agent": "infrakid",
  "command": "review",
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

# Kill switch — abort everything
curl -X DELETE http://localhost:7781/api/ask
# → {"killed": 3}
```

Aborts drop the worker future; `kill_on_drop(true)` on every backend `Command` SIGKILLs the underlying subprocess.

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
  "version": "0.6.8",
  "active_backend": "claude",
  "enabled_backends": ["claude", "ollama"]
}
```

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
