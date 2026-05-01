//! Session enumeration + deletion across backends.
//!
//! Claude/Copilot/Gemini all persist sessions to disk; Ollama keeps them in
//! apytti's in-memory store. This module gives a uniform `list` / `delete`
//! interface so hermytt's UI can build a "session browser" without learning
//! each harness's storage layout.

use std::path::PathBuf;

use serde::Serialize;

use crate::backend::BackendKind;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    /// Decoded path (the actual cwd Claude was started in).
    pub dir: String,
    pub session_count: usize,
    pub total_bytes: u64,
    /// Most recent session mtime as ISO 8601.
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub dir: Option<String>,
    pub bytes: u64,
    pub modified_at: Option<String>,
    /// First user message (truncated to ~200 chars), useful for UI previews.
    pub first_message: Option<String>,
}

/// Per-backend root directory for sessions.
fn claude_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude/projects"))
}

/// Encode/decode the cwd ↔ project-dir-name (`/` ↔ `-`).
pub fn encode_dir(dir: &str) -> String {
    dir.replace('/', "-")
}

pub fn decode_dir(encoded: &str) -> String {
    encoded.replace('-', "/")
}

/// List all Claude projects (one per cwd Claude was ever started in).
pub fn list_claude_projects() -> Vec<ProjectInfo> {
    let Some(root) = claude_root() else {
        return vec![];
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return vec![];
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let mut session_count = 0usize;
        let mut total_bytes = 0u64;
        let mut latest_mtime: Option<std::time::SystemTime> = None;
        // Read the authoritative cwd from the first jsonl we encounter, since the
        // dir name encoding (replace `/` with `-`) is ambiguous when path
        // components legitimately contain `-` (e.g. act-runner-rs vs act/runner/rs).
        let mut authoritative_cwd: Option<String> = None;

        if let Ok(files) = std::fs::read_dir(entry.path()) {
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                session_count += 1;
                if let Ok(meta) = f.metadata() {
                    total_bytes += meta.len();
                    if let Ok(m) = meta.modified() {
                        latest_mtime = Some(latest_mtime.map_or(m, |cur| cur.max(m)));
                    }
                }
                if authoritative_cwd.is_none() {
                    authoritative_cwd = peek_session_cwd(&path);
                }
            }
        }

        if session_count == 0 {
            continue;
        }

        out.push(ProjectInfo {
            dir: authoritative_cwd.unwrap_or_else(|| decode_dir(&name)),
            session_count,
            total_bytes,
            last_modified: latest_mtime.map(systime_to_iso),
        });
    }

    out.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    out
}

/// List sessions for a given Claude project (decoded `dir`). If `dir` is None,
/// walks every project.
pub fn list_claude_sessions(dir: Option<&str>) -> Vec<SessionInfo> {
    let Some(root) = claude_root() else {
        return vec![];
    };

    let project_dirs: Vec<PathBuf> = match dir {
        Some(d) => vec![root.join(encode_dir(d))],
        None => std::fs::read_dir(&root)
            .map(|it| {
                it.flatten()
                    .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default(),
    };

    let mut out = Vec::new();
    for project_path in project_dirs {
        let fallback_dir = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(decode_dir);

        let Ok(files) = std::fs::read_dir(&project_path) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if session_id.is_empty() {
                continue;
            }
            let meta = f.metadata().ok();
            let bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified_at = meta.and_then(|m| m.modified().ok()).map(systime_to_iso);
            let first_message = peek_first_user_message(&path);
            // Prefer the cwd recorded inside the jsonl (authoritative) over the
            // ambiguous dir-name decode.
            let session_dir = peek_session_cwd(&path).or_else(|| fallback_dir.clone());

            out.push(SessionInfo {
                session_id,
                dir: session_dir,
                bytes,
                modified_at,
                first_message,
            });
        }
    }

    out.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    out
}

/// Delete a single Claude session. Searches across all projects (since session_id
/// is unique). Returns true if a file was removed.
pub fn delete_claude_session(session_id: &str) -> anyhow::Result<bool> {
    let Some(root) = claude_root() else {
        return Ok(false);
    };
    let Ok(projects) = std::fs::read_dir(&root) else {
        return Ok(false);
    };
    for entry in projects.flatten() {
        let candidate = entry.path().join(format!("{session_id}.jsonl"));
        if candidate.exists() {
            std::fs::remove_file(&candidate)?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Dispatch list across backends. For now: Claude is the only one with a
/// well-defined on-disk layout we've inventoried; others return empty.
pub fn list_projects(kind: BackendKind) -> Vec<ProjectInfo> {
    match kind {
        BackendKind::Claude => list_claude_projects(),
        // TODO: copilot, gemini once we map their stores
        _ => vec![],
    }
}

pub fn list_sessions(kind: BackendKind, dir: Option<&str>) -> Vec<SessionInfo> {
    match kind {
        BackendKind::Claude => list_claude_sessions(dir),
        _ => vec![],
    }
}

pub fn delete_session(kind: BackendKind, session_id: &str) -> anyhow::Result<bool> {
    match kind {
        BackendKind::Claude => delete_claude_session(session_id),
        _ => Ok(false),
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SessionStatus {
    pub session_id: String,
    pub active: bool,
    pub processes: Vec<ProcessMatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessMatch {
    pub pid: u32,
    pub cwd: Option<String>,
    /// "argv" = SID was found in --resume / --session-id flags (strong match)
    /// "cwd"  = process cwd matches a project containing this SID (weak match)
    pub match_type: String,
}

/// Detect whether a session is currently being processed by some other process.
/// Catches:
///   - Strong: any process with `--resume <sid>` or `--session-id <sid>` in argv
///   - Weak (Claude only): interactive `claude` with cwd matching a project that owns this sid
pub fn session_status(kind: BackendKind, sid: &str) -> SessionStatus {
    let processes = match kind {
        BackendKind::Claude => claude_session_processes(sid),
        _ => vec![],
    };
    SessionStatus {
        session_id: sid.to_string(),
        active: !processes.is_empty(),
        processes,
    }
}

#[cfg(unix)]
fn claude_session_processes(sid: &str) -> Vec<ProcessMatch> {
    let mut out = Vec::new();

    // Find which project dir owns this sid (for the weak cwd-based match later).
    let owning_project_dir = find_owning_project(sid);

    let Ok(output) = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
    else {
        return out;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        let line = line.trim_start();
        let Some((pid_str, rest)) = line.split_once(' ') else {
            continue;
        };
        let Ok(pid): Result<u32, _> = pid_str.parse() else {
            continue;
        };
        // Quick prefilter — skip anything that doesn't look like a claude invocation
        if !rest.contains("claude") {
            continue;
        }
        // Skip ourselves and any apytti subprocess that isn't actually claude
        if !looks_like_claude_process(rest) {
            continue;
        }

        // Strong match: SID appears in argv (--resume <sid> or --session-id <sid>)
        if rest.contains(sid) {
            out.push(ProcessMatch {
                pid,
                cwd: process_cwd(pid),
                match_type: "argv".into(),
            });
            continue;
        }

        // Weak match: interactive claude in the same project dir
        if let Some(ref proj_dir) = owning_project_dir {
            if let Some(cwd) = process_cwd(pid) {
                if cwd == *proj_dir {
                    out.push(ProcessMatch {
                        pid,
                        cwd: Some(cwd),
                        match_type: "cwd".into(),
                    });
                }
            }
        }
    }
    out
}

#[cfg(not(unix))]
fn claude_session_processes(_sid: &str) -> Vec<ProcessMatch> {
    // ps-based detection isn't trivially portable to Windows; future work.
    vec![]
}

/// Try to identify the cmd as a real `claude` binary invocation rather than e.g. `grep claude`.
fn looks_like_claude_process(cmd: &str) -> bool {
    // Take the first whitespace-separated token (the executable path)
    let exe = cmd.split_whitespace().next().unwrap_or("");
    let basename = exe.rsplit('/').next().unwrap_or(exe);
    basename == "claude" || basename.starts_with("claude")
}

#[cfg(unix)]
fn process_cwd(pid: u32) -> Option<String> {
    // macOS: `lsof -p <pid> -d cwd -Fn` → "n<path>"
    // Linux: /proc/<pid>/cwd is a symlink
    if cfg!(target_os = "linux") {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .and_then(|p| p.into_os_string().into_string().ok())
    } else {
        let output = std::process::Command::new("lsof")
            .args(["-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(stripped) = line.strip_prefix('n') {
                return Some(stripped.to_string());
            }
        }
        None
    }
}

/// One message in a flattened conversation log. Backend-agnostic shape so
/// hermytt's UI doesn't need to know about claude's jsonl format.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Summary of tool calls in this turn, e.g. ["Bash", "Read"]. Empty if none.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_uses: Vec<ToolUse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolUse {
    pub name: String,
    /// Brief one-line summary of the tool's input, truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageLog {
    pub session_id: String,
    pub dir: Option<String>,
    pub messages: Vec<Message>,
}

/// Read the full conversation log for a session.
pub fn read_messages(kind: BackendKind, sid: &str) -> anyhow::Result<MessageLog> {
    match kind {
        BackendKind::Claude => read_claude_messages(sid),
        _ => Ok(MessageLog {
            session_id: sid.to_string(),
            dir: None,
            messages: vec![],
        }),
    }
}

fn read_claude_messages(sid: &str) -> anyhow::Result<MessageLog> {
    let root = claude_root().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let owning_dir = find_owning_project(sid);
    let project_path = match &owning_dir {
        Some(d) => root.join(encode_dir(d)),
        None => anyhow::bail!("session not found: {sid}"),
    };
    let path = project_path.join(format!("{sid}.jsonl"));
    if !path.exists() {
        anyhow::bail!("session not found: {sid}");
    }

    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(&path)?;
    let reader = BufReader::new(file);

    let mut messages = Vec::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let entry_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if entry_type != "user" && entry_type != "assistant" {
            continue;
        }
        let msg = match v.get("message") {
            Some(m) => m,
            None => continue,
        };
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or(entry_type)
            .to_string();
        let timestamp = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(String::from);
        let model = msg
            .get("model")
            .and_then(|m| m.as_str())
            .map(String::from);

        let (content, tool_uses) = flatten_claude_content(msg.get("content"));

        // Skip empty messages (rare but happens with pure tool_result wrappers)
        if content.is_empty() && tool_uses.is_empty() {
            continue;
        }

        messages.push(Message {
            role,
            content,
            timestamp,
            model,
            tool_uses,
        });
    }

    Ok(MessageLog {
        session_id: sid.to_string(),
        dir: owning_dir,
        messages,
    })
}

/// Flatten claude's structured content (string OR array of {type:text|tool_use|tool_result})
/// into a single human-readable string + a list of tool calls.
fn flatten_claude_content(content: Option<&serde_json::Value>) -> (String, Vec<ToolUse>) {
    let Some(content) = content else {
        return (String::new(), vec![]);
    };

    if let Some(s) = content.as_str() {
        return (s.to_string(), vec![]);
    }

    let Some(arr) = content.as_array() else {
        return (String::new(), vec![]);
    };

    let mut text_parts = Vec::new();
    let mut tools = Vec::new();
    for block in arr {
        let bt = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match bt {
            "text" => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(t.to_string());
                }
            }
            "tool_use" => {
                let name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("?")
                    .to_string();
                let input_summary = block
                    .get("input")
                    .map(|i| summarize_tool_input(i, &name));
                tools.push(ToolUse {
                    name: name.clone(),
                    input_summary,
                });
                text_parts.push(format!("[tool: {name}]"));
            }
            "tool_result" => {
                // Tool results live in user-role messages following an assistant tool_use.
                // Skip the body in the flat string; hermytt UI surfaces the tool_use chip.
                text_parts.push("[tool result]".to_string());
            }
            "thinking" => {
                // Extended-thinking blocks — surface a marker, not the content
                text_parts.push("[thinking]".to_string());
            }
            _ => {}
        }
    }
    (text_parts.join("\n").trim().to_string(), tools)
}

/// One-line summary of a tool's input. Picks the most useful field per tool name,
/// falls back to the first string-ish value.
fn summarize_tool_input(input: &serde_json::Value, tool_name: &str) -> String {
    let pick = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(s) = input.get(k).and_then(|v| v.as_str()) {
                return Some(truncate(s, 100));
            }
        }
        None
    };

    let s = match tool_name {
        "Bash" => pick(&["command"]),
        "Read" | "Edit" | "Write" => pick(&["file_path", "path"]),
        "Glob" => pick(&["pattern"]),
        "Grep" => pick(&["pattern", "query"]),
        "WebFetch" | "WebSearch" => pick(&["url", "query"]),
        _ => pick(&["query", "command", "path", "name"]),
    };

    s.unwrap_or_else(|| {
        // Fallback: serialize the whole input compactly, truncated.
        let raw = serde_json::to_string(input).unwrap_or_default();
        truncate(&raw, 100)
    })
}

/// Find the project directory (decoded cwd) that contains a given session id.
fn find_owning_project(sid: &str) -> Option<String> {
    let root = claude_root()?;
    let entries = std::fs::read_dir(&root).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(format!("{sid}.jsonl"));
        if candidate.exists() {
            let name = entry.file_name().to_string_lossy().into_owned();
            return Some(decode_dir(&name));
        }
    }
    None
}

/// Read the first few lines of a session file and pull the recorded cwd.
/// Claude stores `"cwd":"/abs/path"` on most non-trivial events; the dirname
/// encoding (replace `/` with `-`) is ambiguous when components contain `-`
/// (act-runner-rs vs act/runner/rs), so this is the authoritative source.
fn peek_session_cwd(path: &PathBuf) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for (i, line) in reader.lines().enumerate() {
        if i > 30 {
            break;
        }
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
            return Some(cwd.to_string());
        }
    }
    None
}

/// Read the first ~20 lines of a session file and pull the first user message.
fn peek_first_user_message(path: &PathBuf) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for (i, line) in reader.lines().enumerate() {
        if i > 30 {
            break;
        }
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        if v.pointer("/message/role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let content = v.pointer("/message/content");
        if let Some(text) = content.and_then(|c| c.as_str()) {
            return Some(truncate(text, 200));
        }
        if let Some(arr) = content.and_then(|c| c.as_array()) {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        return Some(truncate(text, 200));
                    }
                }
            }
        }
    }
    None
}

fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

fn systime_to_iso(t: std::time::SystemTime) -> String {
    use crate::models;
    // Reuse models.rs's epoch-to-ISO formatter (no chrono dep).
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    models::epoch_to_iso(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let dir = "/Users/cali/Developer/perso/apytti";
        assert_eq!(decode_dir(&encode_dir(dir)), dir);
    }

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_long() {
        let s = "a".repeat(300);
        let t = truncate(&s, 50);
        assert_eq!(t.chars().count(), 51); // 50 + ellipsis
        assert!(t.ends_with('…'));
    }
}
