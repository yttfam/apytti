use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::{AskRequest, Response};
use crate::persist::BackendConfig;
use crate::stream::StreamEvent;

#[derive(Debug, serde::Deserialize)]
struct RawResponse {
    result: Option<String>,
    session_id: Option<String>,
    total_cost_usd: Option<f64>,
    is_error: Option<bool>,
}

pub fn build_command(cfg: &BackendConfig, req: &AskRequest) -> Command {
    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(&req.prompt);
    cmd.arg("--output-format").arg("json");

    if let Some(sid) = req.session_id.as_deref().or(cfg.session_id.as_deref()) {
        if cfg.resume {
            cmd.arg("--resume").arg(sid);
        } else {
            cmd.arg("--session-id").arg(sid);
        }
    }

    if let Some(m) = req.model.as_deref().or(cfg.model.as_deref()) {
        cmd.arg("--model").arg(m);
    }

    if let Some(e) = req.effort.as_deref().or(cfg.effort.as_deref()) {
        cmd.arg("--effort").arg(e);
    }

    if cfg.skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }

    if let Some(agent) = req.agent.as_deref() {
        cmd.arg("--agent").arg(agent);
    }

    for rule in &cfg.allow {
        cmd.arg("--allowedTools").arg(rule);
    }

    for rule in &req.extra_allow {
        cmd.arg("--allowedTools").arg(rule);
    }

    // Per-request `dir` overrides the per-backend default. Sessions are scoped
    // to the cwd, so this is required for multi-project resume.
    if let Some(dir) = req.dir.as_deref().or(cfg.dir.as_deref()) {
        cmd.current_dir(dir);
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // SIGKILL the subprocess if the owning future drops (cancellation /
    // request abort). Without this, /api/ask cancellations would leave
    // claude running until natural completion.
    cmd.kill_on_drop(true);

    cmd
}

pub async fn ask(cfg: &BackendConfig, req: &AskRequest) -> anyhow::Result<Response> {
    let mut cmd = build_command(cfg, req);
    let output = cmd.output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(Response {
            response: String::new(),
            session_id: None,
            cost_usd: None,
            backend: String::new(),
            error: Some(format!("claude exited {}: {stderr}", output.status)),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw: RawResponse = serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("failed to parse claude JSON: {e}\nstdout: {stdout}"))?;

    let error = if raw.is_error == Some(true) {
        Some("claude reported an error".into())
    } else {
        None
    };

    Ok(Response {
        response: raw.result.unwrap_or_default(),
        session_id: raw.session_id,
        cost_usd: raw.total_cost_usd,
        backend: String::new(),
        error,
    })
}

/// Streaming variant — emits Delta events as the assistant generates text.
/// Returns the final Response (used by dispatch_stream to emit the Done event).
///
/// Claude's `--output-format stream-json` (with --verbose) emits JSONL of:
///   - `{"type":"system","subtype":"init",...}` (skipped)
///   - `{"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}` (text chunks)
///   - `{"type":"result","session_id":"...","total_cost_usd":...}` (terminal)
///
/// Each `assistant` event's text field is cumulative for that turn, so we track
/// last-emitted length and emit only the suffix as a delta.
pub async fn ask_stream(
    cfg: &BackendConfig,
    req: &AskRequest,
    tx: &mpsc::Sender<StreamEvent>,
) -> anyhow::Result<Response> {
    let mut cmd = build_command(cfg, req);
    // Override output-format to stream-json. claude requires --verbose with stream-json.
    cmd.arg("--output-format").arg("stream-json").arg("--verbose");

    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("claude: stdout was not piped"))?;
    let mut lines = BufReader::new(stdout).lines();

    let mut session_id = None;
    let mut cost_usd = None;
    let mut full_text = String::new();
    let mut emitted_len = 0usize;
    let mut error_msg: Option<String> = None;
    // Map tool_use_id -> tool name so subsequent tool_result blocks
    // (which only carry the id) can be paired with their tool name.
    let mut tool_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                if let Some(content) = v
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        let bt = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match bt {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    full_text = text.to_string();
                                    if full_text.len() > emitted_len {
                                        let delta = full_text[emitted_len..].to_string();
                                        emitted_len = full_text.len();
                                        if tx.send(StreamEvent::Delta { text: delta }).await.is_err() {
                                            return Ok(final_response(full_text, session_id, cost_usd, error_msg));
                                        }
                                    }
                                }
                            }
                            "tool_use" => {
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("?")
                                    .to_string();
                                if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                                    tool_names.insert(id.to_string(), name.clone());
                                }
                                let input_summary = block
                                    .get("input")
                                    .map(|i| crate::sessions::summarize_tool_input(i, &name));
                                if tx
                                    .send(StreamEvent::ToolUse { name, input_summary })
                                    .await
                                    .is_err()
                                {
                                    return Ok(final_response(full_text, session_id, cost_usd, error_msg));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("user") => {
                // Tool results come back in user-role messages following an
                // assistant tool_use. Only the tool_use_id is present, so we
                // look up the name from the map populated above.
                if let Some(content) = v
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            let name = block
                                .get("tool_use_id")
                                .and_then(|i| i.as_str())
                                .and_then(|id| tool_names.get(id).cloned())
                                .unwrap_or_else(|| "?".to_string());
                            if tx
                                .send(StreamEvent::ToolResult { name })
                                .await
                                .is_err()
                            {
                                return Ok(final_response(full_text, session_id, cost_usd, error_msg));
                            }
                        }
                    }
                }
            }
            Some("result") => {
                session_id = v.get("session_id").and_then(|s| s.as_str()).map(String::from);
                cost_usd = v.get("total_cost_usd").and_then(|c| c.as_f64());
                if v.get("is_error").and_then(|e| e.as_bool()) == Some(true) {
                    error_msg = Some(
                        v.get("result")
                            .and_then(|r| r.as_str())
                            .unwrap_or("claude reported an error")
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }

    let status = child.wait().await?;
    if !status.success() && error_msg.is_none() {
        error_msg = Some(format!("claude exited {status}"));
    }

    Ok(final_response(full_text, session_id, cost_usd, error_msg))
}

fn final_response(
    response: String,
    session_id: Option<String>,
    cost_usd: Option<f64>,
    error: Option<String>,
) -> Response {
    Response {
        response,
        session_id,
        cost_usd,
        backend: String::new(),
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BackendConfig {
        BackendConfig {
            enabled: true,
            model: Some("sonnet".into()),
            effort: Some("low".into()),
            resume: true,
            ..Default::default()
        }
    }

    fn req(prompt: &str) -> AskRequest {
        AskRequest {
            prompt: prompt.into(),
            ..Default::default()
        }
    }

    fn args_of(cmd: &Command) -> Vec<&std::ffi::OsStr> {
        cmd.as_std().get_args().collect()
    }

    #[test]
    fn build_command_basic() {
        let cmd = build_command(&cfg(), &req("hello"));
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("-p")));
        assert!(args.contains(&std::ffi::OsStr::new("hello")));
        assert!(args.contains(&std::ffi::OsStr::new("--model")));
        assert!(args.contains(&std::ffi::OsStr::new("sonnet")));
        assert!(args.contains(&std::ffi::OsStr::new("--effort")));
        assert!(args.contains(&std::ffi::OsStr::new("low")));
    }

    #[test]
    fn build_command_resume() {
        let mut r = req("hi");
        r.session_id = Some("abc-123".into());
        let cmd = build_command(&cfg(), &r);
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--resume")));
        assert!(args.contains(&std::ffi::OsStr::new("abc-123")));
    }

    #[test]
    fn request_dir_overrides_config_dir() {
        let mut c = cfg();
        c.dir = Some("/config-dir".into());
        let mut r = req("hi");
        r.dir = Some("/request-dir".into());
        let cmd = build_command(&c, &r);
        assert_eq!(
            cmd.as_std().get_current_dir(),
            Some(std::path::Path::new("/request-dir"))
        );
    }

    #[test]
    fn config_dir_used_when_request_dir_absent() {
        let mut c = cfg();
        c.dir = Some("/config-dir".into());
        let cmd = build_command(&c, &req("hi"));
        assert_eq!(
            cmd.as_std().get_current_dir(),
            Some(std::path::Path::new("/config-dir"))
        );
    }

    #[test]
    fn build_command_no_resume() {
        let mut c = cfg();
        c.resume = false;
        let mut r = req("hi");
        r.session_id = Some("abc-123".into());
        let cmd = build_command(&c, &r);
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--session-id")));
        assert!(!args.contains(&std::ffi::OsStr::new("--resume")));
    }

    #[test]
    fn request_overrides_config() {
        let mut r = req("hi");
        r.model = Some("opus".into());
        r.effort = Some("max".into());
        let cmd = build_command(&cfg(), &r);
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("opus")));
        assert!(args.contains(&std::ffi::OsStr::new("max")));
        assert!(!args.contains(&std::ffi::OsStr::new("sonnet")));
    }

    #[test]
    fn skip_permissions() {
        let mut c = cfg();
        c.skip_permissions = true;
        let cmd = build_command(&c, &req("hi"));
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--dangerously-skip-permissions")));
    }

    #[test]
    fn allowed_tools() {
        let mut c = cfg();
        c.allow = vec!["Bash(git:*)".into(), "Read(*)".into()];
        let cmd = build_command(&c, &req("hi"));
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--allowedTools")));
        assert!(args.contains(&std::ffi::OsStr::new("Bash(git:*)")));
        assert!(args.contains(&std::ffi::OsStr::new("Read(*)")));
    }

    #[test]
    fn working_dir() {
        let mut c = cfg();
        c.dir = Some("/tmp/test".into());
        let cmd = build_command(&c, &req("hi"));
        assert_eq!(
            cmd.as_std().get_current_dir(),
            Some(std::path::Path::new("/tmp/test"))
        );
    }

    #[test]
    fn parse_response() {
        let json = r#"{"result": "Hello!", "session_id": "abc-123", "total_cost_usd": 0.05, "is_error": false}"#;
        let raw: RawResponse = serde_json::from_str(json).unwrap();
        assert_eq!(raw.result.as_deref(), Some("Hello!"));
        assert_eq!(raw.session_id.as_deref(), Some("abc-123"));
    }
}
