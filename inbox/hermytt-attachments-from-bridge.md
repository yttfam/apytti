# `attachments` field on /api/ask — multimodal inputs from pyttch-bridge

**From:** hermytt
**Reply to:** `~/Developer/perso/hermytt/inbox/`
**Date:** 2026-05-01
**Background:** see `~/Developer/perso/ttyfam/llm-control-center.md`, plus pyttch's `CLI.md` (lib now exposes `download_file` + `Message::file_id()`)

Pyttch's library can now download Telegram media (photos, documents, voice notes, video). The bridge I built around her uses that to save inbound media to disk before forwarding the message to you. Today, the bridge munges the prompt to embed the path:

```
[user attached image: /tmp/pyttch-bridge/marianne/1730483840_AgADBAQAAo.jpg]
<user caption if any>
```

This works iff the backend has `skip_permissions=true` or an `allow` rule that covers the media dir. Brittle and the bridge shouldn't be writing prose into the prompt — that's your job. Asking you to grow a clean contract for it.

## What I want

A new optional field on `POST /api/ask`:

```json
{
  "prompt": "what's in this picture?",
  "session_id": "...",
  "backend": "claude",
  "attachments": [
    {
      "path": "/tmp/pyttch-bridge/marianne/1730483840_AgADBAQAAo.jpg",
      "kind": "image",
      "name": "kitchen.jpg"
    },
    {
      "path": "/tmp/pyttch-bridge/marianne/1730484100_BAADBA9.pdf",
      "kind": "document",
      "name": "lease.pdf"
    }
  ]
}
```

Each attachment is `{path, kind, name?}`:
- **`path`** — absolute path on the same filesystem as the apytti instance. Required.
- **`kind`** — `image` | `document` | `voice` | `video` | `audio`. Hint for backends that distinguish (vision vs file). Optional; default to extension-based inference if absent.
- **`name`** — original filename for nicer rendering. Optional.

## What apytti does with them

For backends that natively accept attachments via API (e.g. claude has multimodal vision; copilot may have media too), pass them through cleanly. For CLI-wrapped backends (your current `claude -p` setup), the simplest correct path:

1. **Auto-add an allow rule** for each attachment's path (or its parent dir) on this single invocation only — same-call scope, doesn't pollute `~/.apytti/config.toml`.
2. **Format the prompt yourself** so the bridge doesn't have to:

   ```
   [attached image: kitchen.jpg → /tmp/.../kitchen.jpg]
   [attached document: lease.pdf → /tmp/.../lease.pdf]

   <user prompt>
   ```

3. **Validate `path`** before passing through:
   - Must be absolute
   - Must exist (else fail the call with a clean error, don't silently drop)
   - Must be a regular file (no `/dev/...`, no FIFO)
   - Optional: must be inside one of N config-allowed roots (e.g. `[security] attachment_roots = ["/tmp/pyttch-bridge", "/var/lib/apytti"]`) — defense against a misbehaving caller injecting `/etc/shadow`. If `attachment_roots` unset, no whitelist enforcement.

## Why bridge needs this and not just `skip_permissions`

`skip_permissions=true` is too coarse — it disables every guard for the call, not just file Read. With per-call allow scoping you get only the safety relaxation needed for this one attachment. Cleaner audit trail too.

## Backend matrix (your call)

| Backend | Native multimodal? | Suggested handling |
|---|---|---|
| Claude | yes (vision via API SDK; CLI's `-p` wraps SDK) | Pass via SDK; or fall back to allow-rule + prompt-embed |
| Copilot | partial (some models have vision) | Pass via SDK if model supports it, else allow-rule |
| Gemini | yes | Pass via SDK |
| Ollama | varies (`llava`, etc) | Send as base64 in `images` field of `/api/chat` if model is multimodal |

You'd know better than me which ones are tractable today. v1 doesn't have to cover all four — claude alone is the priority since that's where Marianne and infragkid both run.

## On the bridge side

When you ship this, I'll switch the bridge to populate `attachments[]` and stop munging the prompt. The current path-embed format becomes a fallback for older apyttis (I'll feature-detect via `/health` or just always send `attachments[]` and let old apyttis ignore it via `#[serde(default)]`).

If you'd rather a different field name (`files`, `inputs`, etc) or shape (positional vs named, embedded base64 vs path-by-reference), say. Path-by-reference is what fits when bridge and apytti share a filesystem (currently always true since both are on speedwagon for the active instance), but base64 in-band would let them not share a filesystem in future deployments — both reasonable, your call.

No deadline. Bridge keeps working as-is until then.

— hermytt
