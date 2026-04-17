# apytti

"A pity" the AI CLIs don't share an API. So here's one.

A unified REST gateway over Claude, Copilot, Gemini, and Ollama. One binary, one endpoint, four backends.

## Features

- Single REST API in front of `claude`, `copilot`, `gemini`, and Ollama (HTTP)
- Per-request backend override — talk to all four from one server
- Stateless gateway (sessions managed by the CLIs themselves; Ollama sessions kept in memory)
- Library API for Rust crates that want to call any backend programmatically
- Daemon install for macOS (LaunchDaemon), Linux (systemd), Windows (sc)

## Install

```bash
cargo build --release
# Binary at target/release/apytti
```

Then configure backends:

```bash
apytti setup
```

Interactive menu — pick which backends to enable, set defaults (model, effort, skip-perms, etc.), choose the active default. Config saved to `~/.apytti/config.toml`.

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

## REST API

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
  "effort": "low"
}
```

Only `prompt` is required. `backend` defaults to the configured active. Everything else is optional.

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

### GET /health

```json
{
  "status": "ok",
  "version": "0.2.0",
  "active_backend": "claude",
  "enabled_backends": ["claude", "ollama"]
}
```

### GET /help

Full HTML API documentation served from the binary.

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
```

Use `apytti setup` to edit interactively.

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
# macOS — generates LaunchDaemon plist (sudo required to install)
apytti install --port 7781

# Linux — generates systemd user unit
apytti install --port 7781

# Windows — prints sc.exe commands
apytti install --port 7781
```

## Backend mapping

| | Claude | Copilot | Gemini | Ollama |
|---|---|---|---|---|
| Subprocess | `claude` | `copilot` | `gemini` | (HTTP) |
| Endpoint | — | — | — | `localhost:11434` |
| Output | single JSON | JSONL stream | single JSON | HTTP `/api/chat` |
| Sessions | `--resume` | `--resume=` | `--resume` | in-memory store |
| Skip perms | `--dangerously-skip-permissions` | `--allow-all` | `--yolo` | n/a |
| Effort | yes | yes | n/a | n/a |
| Cost reporting | yes (API key) | n/a | n/a | n/a |

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

Apytti is the simplest member. She uses each CLI's `-p` non-interactive mode and skips all the TUI parsing her sister [grytti](../grytti) does.

## License

MIT
