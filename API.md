# apytti API reference

Quick contract for every endpoint. Updated on every endpoint change. **For prose docs see README.md, for the in-binary HTML view hit `GET /help`.**

Version: **0.5.0**
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
  "stream": false                             // optional, returns SSE if true
}
```

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
