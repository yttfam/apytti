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
  "prompt": "string",                         // required unless `attachments` is non-empty
  "backend": "claude|copilot|gemini|ollama",  // optional, defaults to active
  "session_id": "uuid",                       // optional, resumes session
  "model": "string",                          // optional, overrides default
  "effort": "low|medium|high|max",            // optional (claude/copilot only)
  "dir": "/path",                             // optional, per-request CWD override
  "stream": false,                            // optional, returns SSE if true
  "agent": "infrakid",                        // optional (claude only) — passes --agent <name>
  "command": "review",                        // optional (claude only) — expand ~/.claude/commands/review.md, $ARGUMENTS = prompt
  "attachments": [                            // optional; one of `path` OR `data` per entry
    { "path": "/abs/path/kitchen.jpg", "kind": "image", "name": "kitchen.jpg" },
    { "data": "<base64>",              "kind": "image", "name": "selfie.jpg"  },
    { "path": "/abs/path/lease.pdf",   "kind": "document" }
  ]
}
```

**Attachments**: each entry is `{path?, data?, kind?, name?}`. Exactly one of `path` or `data` per entry — both → 400, neither → 400. `kind` is one of `image | document | voice | video | audio` (defaults to extension-based inference, using `name` if present, else the path's basename). `name` is the original filename for display.

- **`path`** form — absolute path on apytti's filesystem. Must exist and be a regular file. Use when the caller and apytti share a filesystem.
- **`data`** form — base64-encoded raw bytes. Use when the caller is on a different host. Apytti decodes the bytes, writes them to a per-request temp dir under `~/.apytti/inbox/<uuid>/`, uses that path for the rest of the call, and deletes the dir when the request finishes (RAII cleanup; survives errors). Filenames derived from `name` (sanitized: only `[A-Za-z0-9._-]`, with index prefix to disambiguate).

Apytti prepends a reference line per attachment to the prompt (`[attached <kind>: <name> -> <path>]`) and, for the claude CLI backend, mints a per-call `--allowedTools Read(<path>)` rule so the file is readable without `--dangerously-skip-permissions`. Per-call scope only — never persisted to `~/.apytti/config.toml`.

**Security gate** (optional, `path` form only): set `[security] attachment_roots = ["/tmp/pyttch-bridge", ...]` in the persisted config to require every `attachments[].path` live inside one of those roots. The `data` form is unaffected — apytti owns the write location. Unset = no whitelist enforcement (existence/regular-file checks still apply to `path`).

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

event: tool_use
data: {"type":"tool_use","name":"Bash","input_summary":"git status"}

event: tool_result
data: {"type":"tool_result","name":"Bash"}

event: delta
data: {"type":"delta","text":" world"}

event: done
data: {"type":"done","response":"hello world","session_id":"...","cost_usd":...,"backend":"...","error":null}
```

Event types:
- `delta` — incremental text chunk; concatenate to build the assistant turn.
- `tool_use` — model started a tool call. `input_summary` is a one-line preview (same shape as the `tool_uses[]` field in `GET /sessions/{sid}/messages`); omitted when nothing useful to show.
- `tool_result` — the tool returned. No body — bridge UIs can use this to clear a "🔧 running…" placeholder.
- `done` — terminal event with full response payload.
- `error` — terminal event for fatal stream failures.

Ordering: `tool_use` before any deltas/results that follow from it; `done` is always last (or `error`). Tool events are claude-only today; other backends emit `delta` + `done` only.

---

## DELETE /api/ask

**Kill switch.** Aborts every in-flight `/api/ask` call. Each backend Command runs with `kill_on_drop`, so the underlying subprocess is SIGKILL'd as the future drops.

```json
{ "killed": 3 }
```

Cancelled non-streaming callers receive a `400` with `error: "cancelled"`. Streaming callers see the SSE stream close mid-flight (no `done`/`error` event guaranteed).

---

## POST /backends/{name}/sessions/{sid}/cancel

Cancel any in-flight `/api/ask` call(s) for this `(backend, session_id)`. Sessionless calls aren't matched here — use `DELETE /api/ask` for those.

```json
{ "killed": 1 }
```

Returns `{"killed": 0}` (with 200) if nothing was in flight.

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

## GET /config-ui

`text/html` — self-contained settings page. Reads `/backends/schema`, `/health`, and `/config`, lets you edit each backend's fields, the active default, and hermytt registry settings, then PUTs back to `/config`. The macOS menu-bar app exposes it via the "Settings…" item, but it works in any browser.

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
**Query**: `?since=<int>` — return only messages from index `<since>` onward (cheap incremental fetch for long sessions).

- `since` ≤ 0 or omitted: full set returned.
- `since` valid (`0 <= since <= total`): `messages[since..]` returned.
- `since` > `total` (file truncated/edited externally): full set returned, so the client sees index 0 and knows to reset.

Response always includes `total` (length of the underlying log) so the client can tell whether it's caught up.

Full conversation as a flat ordered array. Backend-agnostic shape.

```json
{
  "session_id": "uuid",
  "dir": "/path",
  "total": 7742,
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
