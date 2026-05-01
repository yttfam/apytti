# apytti API reference

Quick contract for every endpoint. Updated on every endpoint change. **For prose docs see README.md, for the in-binary HTML view hit `GET /help`.**

Version: **0.6.0**
Default port: **7781**
Base URL: `http://<host>:<port>`

## Auth

`X-Hermytt-Key: <token>` header is required for write/destructive endpoints **iff** `hermytt.config_token` is set in the persisted config. Open otherwise.

| Endpoint | Auth required? |
|---|---|
| `PUT /config` | yes (when `config_token` set) |
| `POST /models/init` | yes |
| `DELETE /backends/{name}/sessions/{sid}` | yes |
| `GET /backends/{name}/sessions/{sid}/messages` | yes |
| `POST /backends/{name}/mcp` | yes |
| `DELETE /backends/{name}/mcp/{server}` | yes |
| `POST /backends/{name}/commands` | yes |
| `DELETE /backends/{name}/commands/{cmd}` | yes |
| Everything else | no |

---

## POST /api/ask

Send a prompt to a backend.

**Request body**:
```json
{
  "prompt": "string",                         // required
  "backend": "claude|copilot|gemini|ollama",  // optional, defaults to active
  "session_id": "uuid",                       // optional, resumes session
  "model": "string",                          // optional, overrides default
  "effort": "low|medium|high|max",            // optional (claude/copilot only)
  "dir": "/path",                             // optional, per-request CWD override
  "stream": false,                            // optional, returns SSE if true
  "agent": "infrakid",                        // optional (claude only) — passes --agent <name>
  "command": "review",                        // optional (claude only) — expand ~/.claude/commands/review.md, $ARGUMENTS = prompt
  "attachments": [                            // optional, file references on the apytti host's filesystem
    { "path": "/abs/path/kitchen.jpg", "kind": "image", "name": "kitchen.jpg" },
    { "path": "/abs/path/lease.pdf",   "kind": "document" }
  ]
}
```

**Attachments**: each entry is `{path, kind?, name?}`. `path` is required, must be absolute, exist, and be a regular file. `kind` is one of `image | document | voice | video | audio` (defaults to extension-based inference). `name` is the original filename for display (defaults to basename).

Apytti prepends a reference line per attachment to the prompt (`[attached <kind>: <name> -> <path>]`) and, for the claude CLI backend, mints a per-call `--allowedTools Read(<path>)` rule so the file is readable without `--dangerously-skip-permissions`. Per-call scope only — never persisted to `~/.apytti/config.toml`.

**Security gate** (optional): set `[security] attachment_roots = ["/tmp/pyttch-bridge", ...]` in the persisted config to require every `attachments[].path` live inside one of those roots. Unset = no whitelist enforcement (existence/regular-file checks still apply).

**Response (non-streaming)** — `application/json`:
```json
{
  "response": "string",
  "session_id": "uuid",
  "cost_usd": 0.05,            // null when not applicable
  "backend": "claude",
  "error": null                // string when something went wrong
}
```

**Response (streaming, `stream: true`)** — `text/event-stream`:
```
event: delta
data: {"type":"delta","text":"hello"}

event: delta
data: {"type":"delta","text":" world"}

event: done
data: {"type":"done","response":"hello world","session_id":"...","cost_usd":...,"backend":"...","error":null}
```

Plus `event: error` mid-stream on fatal failures.

---

## GET /health

```json
{
  "status": "ok",
  "version": "0.5.0",
  "active_backend": "claude",
  "enabled_backends": ["claude", "ollama"]
}
```

---

## GET /help

`text/html` — the in-binary documentation page.

---

## GET /config

Returns current `PersistedConfig`. All four backends always present; tokens are redacted to `"***"` on read.

```json
{
  "active": "claude",
  "backends": {
    "claude":  { "enabled": true, "model": "...", "effort": "...", "dir": "...",
                 "skip_permissions": true, "allow": [], "resume": true,
                 "session_id": null, "endpoint": null },
    "copilot": { ... },
    "gemini":  { ... },
    "ollama":  { "enabled": true, "model": "llama3.2", "endpoint": "http://...", ... }
  },
  "hermytt": { "url": "...", "token": "***", "config_token": "***", "endpoint": "...", "name": "..." }
}
```

---

## PUT /config

**Auth**: `X-Hermytt-Key` (when `config_token` set).

Accepts the same shape as GET. **Partial updates merged**: each `backends.<name>` you send replaces that side wholesale; omitted backends untouched. `active` overrides if set. `hermytt` replaced if sent.

**Response**:
```json
{ "ok": true }
```

---

## GET /backends/schema

Static description of each backend's configurable fields, types, hints, and capability flags.

```json
{
  "claude":  { "fields": [...], "supports_effort": true,  "supports_cost": true,  "supports_streaming": true },
  "copilot": { "fields": [...], "supports_effort": true,  "supports_cost": false, "supports_streaming": true },
  "gemini":  { "fields": [...], "supports_effort": false, "supports_cost": false, "supports_streaming": true },
  "ollama":  { "fields": [...], "supports_effort": false, "supports_cost": false, "supports_streaming": true }
}
```

---

## GET /models

Reads the cached model list from `~/.apytti/models.json`. Empty `{}` if `init` was never called.

```json
{
  "claude":  { "models": ["claude-opus-4-7", ...], "fetched_at": "...", "via": "probe" },
  "ollama":  { "models": ["mistral:7b", ...],     "fetched_at": "...", "via": "live"  }
}
```

`via` enum: `live` | `probe` | `probing` | `error` | `missing`.

---

## POST /models/init

**Auth**: `X-Hermytt-Key` (when `config_token` set).
**Query**: `?backend=claude` (optional) — refresh just one backend.

Probes every enabled backend in parallel (or one if filtered). Writes each result to `~/.apytti/models.json` as it completes — clients polling `GET /models` see incremental progress (`via=probing` → `via=probe|live|error`).

Returns the cache after all probes complete (same shape as `GET /models`).

---

## GET /backends/{name}/models

Single-backend slice of the cache.

```json
{ "models": [...], "fetched_at": "...", "via": "..." }
```

Returns `{"models":[],"via":"missing"}` if the backend has never been probed.

---

## GET /backends/{name}/projects

List projects (one per CWD `claude` was ever started in). Currently Claude only; other backends return empty arrays.

```json
{
  "projects": [
    { "dir": "/Users/cali/Developer/perso/apytti",
      "session_count": 3,
      "total_bytes": 5634432,
      "last_modified": "2026-05-01T10:05:31Z" }
  ]
}
```

Sorted by `last_modified` descending.

---

## GET /backends/{name}/sessions

List sessions for a backend.
**Query**: `?dir=/path` — filter to one project (optional; without it, returns sessions across all projects).

```json
{
  "sessions": [
    { "session_id": "uuid",
      "dir": "/path",
      "bytes": 5485248,
      "modified_at": "2026-05-01T10:05:31Z",
      "first_message": "Ready? your big sis gave you quite a lot of insight" }
  ]
}
```

Sorted by `modified_at` descending. Currently Claude only.

---

## GET /backends/{name}/sessions/{sid}/status

Detect whether a session is currently being processed by some other process (catches external interactive `claude` running on the same machine).

```json
{
  "session_id": "uuid",
  "active": true,
  "processes": [
    { "pid": 33219, "cwd": "/Users/cali/Developer/...", "match_type": "argv" }
  ]
}
```

`match_type`: `argv` (strong — SID found in `--resume`/`--session-id` flags) or `cwd` (weak — interactive claude in same project dir).

---

## GET /backends/{name}/sessions/{sid}/messages

**Auth**: `X-Hermytt-Key` (when `config_token` set). Logs may contain secrets.

Full conversation as a flat ordered array. Backend-agnostic shape.

```json
{
  "session_id": "uuid",
  "dir": "/path",
  "messages": [
    { "role": "user",
      "content": "explain this codebase",
      "timestamp": "2026-04-29T18:01:33Z" },
    { "role": "assistant",
      "content": "Sure, this looks like...\n[tool: Bash]",
      "timestamp": "2026-04-29T18:01:36Z",
      "model": "claude-opus-4-7",
      "tool_uses": [{ "name": "Bash", "input_summary": "ls -la" }] }
  ]
}
```

`role`: matches the source CLI's vocabulary (`user`, `assistant`, `system`, `tool`).
`content`: text-flattened. Tool calls in assistant turns are rendered as `[tool: <name>]` markers; full tool details are in `tool_uses`. Tool results in user turns appear as `[tool result]`. Extended-thinking blocks appear as `[thinking]`.
`tool_uses`: per-tool `{name, input_summary}` for chip rendering. Empty when no tools were used.

Currently Claude only. Other backends return `messages: []`.

Returns `400` if the session is not found.

---

## GET /backends/{name}/mcp

List registered MCP servers for the backend (currently claude only). Shells out to `claude mcp list` and parses the result.

```json
{
  "servers": [
    { "name": "palazzo",  "transport": "http", "target": "http://10.10.0.3:6335/mcp", "scope": null },
    { "name": "prompto",  "transport": "http", "target": "http://10.10.0.3:6337/mcp", "scope": null }
  ]
}
```

## POST /backends/{name}/mcp

**Auth**: `X-Hermytt-Key` (when `config_token` set).

Add an MCP server. Wraps `claude mcp add`.

```json
{
  "name": "my-mcp",
  "transport": "http",         // "http" | "sse" | "stdio"
  "target": "https://example.com/mcp",   // URL for http/sse, command for stdio
  "args": [],                  // stdio subprocess args
  "headers": ["Authorization: Bearer xxx"],   // optional, http/sse only
  "scope": "user"              // optional: "user" | "project" | "local"
}
```

## DELETE /backends/{name}/mcp/{server}

**Auth**: `X-Hermytt-Key` (when `config_token` set).

Remove an MCP server. Wraps `claude mcp remove`.

---

## GET /backends/{name}/commands

List custom slash-command templates (user-level + plugin-provided).

```json
{
  "commands": [
    { "name": "help", "scope": "plugin:ralph-wiggum", "path": "...", "body": null },
    { "name": "review", "scope": "user", "path": "/Users/cali/.claude/commands/review.md", "body": null }
  ]
}
```

`body` is null in the listing (read each via `GET /backends/.../commands/{name}` to fetch the markdown).

## GET /backends/{name}/commands/{cmd}

Read a single command including its markdown body. Returns 400 if not found.

## POST /backends/{name}/commands

**Auth**: `X-Hermytt-Key` (when `config_token` set).

Create a user-level command at `~/.claude/commands/{name}.md`.

```json
{ "name": "review", "body": "Review this code:\n\n$ARGUMENTS" }
```

`$ARGUMENTS` is the placeholder replaced with the request's `prompt` when invoked via `/api/ask` with `command: "review"`.

## DELETE /backends/{name}/commands/{cmd}

**Auth**: `X-Hermytt-Key` (when `config_token` set).

Remove a user-level command. Plugin commands cannot be deleted (read-only).

---

## GET /backends/{name}/agents

List user-defined and plugin-provided agents.

```json
{
  "agents": [
    { "name": "infrakid", "scope": "user", "path": "/Users/cali/.claude/agents/infrakid.md" }
  ]
}
```

Read-only — agent CRUD goes through the .md files directly. Pass `agent: "name"` in `/api/ask` to use one for that call.

---

## DELETE /backends/{name}/sessions/{sid}

**Auth**: `X-Hermytt-Key` (when `config_token` set).

Deletes the session file from disk. Returns 400 if not found.

```json
{ "ok": true }
```

---

## Errors

All endpoints return `application/json` errors in this shape:

```json
{
  "response": "",
  "session_id": null,
  "cost_usd": null,
  "error": "human-readable message"
}
```

| Status | Meaning |
|---|---|
| 400 | Bad request, unknown backend, disabled backend, missing prompt, unauthorized header |
| 415 | Wrong content-type on POST/PUT |
| 422 | JSON body failed to deserialize (missing required field) |
| 500 | Internal failure (subprocess crash, file write error) |
