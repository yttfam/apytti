use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::{AskRequest, Response};
use crate::persist::BackendConfig;
use crate::stream::StreamEvent;

#[derive(Debug, serde::Deserialize)]
struct RawResponse {
    session_id: Option<String>,
    response: Option<String>,
    error: Option<Value>,
}

pub fn build_command(cfg: &BackendConfig, req: &AskRequest) -> Command {
    let mut cmd = Command::new("gemini");
    cmd.arg("-p").arg(&req.prompt);
    cmd.arg("--output-format").arg("json");

    // Gemini's --resume takes id|"latest"|index
    if let Some(sid) = req.session_id.as_deref().or(cfg.session_id.as_deref()) {
        cmd.arg("--resume").arg(sid);
    }

    if let Some(m) = req.model.as_deref().or(cfg.model.as_deref()) {
        cmd.arg("--model").arg(m);
    }

    // Gemini has no --effort flag; ignore it silently.

    if cfg.skip_permissions {
        cmd.arg("--yolo");
    }

    if let Some(dir) = req.dir.as_deref().or(cfg.dir.as_deref()) {
        cmd.current_dir(dir);
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    cmd
}

/// Strip leading non-JSON banner output before the first `{`.
pub fn extract_json(stdout: &str) -> Option<&str> {
    let start = stdout.find('{')?;
    Some(&stdout[start..])
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
            error: Some(format!("gemini exited {}: {stderr}", output.status)),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = extract_json(&stdout)
        .ok_or_else(|| anyhow::anyhow!("no JSON found in gemini stdout: {stdout}"))?;

    let raw: RawResponse = serde_json::from_str(json)
        .map_err(|e| anyhow::anyhow!("failed to parse gemini JSON: {e}\nstdout: {stdout}"))?;

    let error = raw.error.map(|e| e.to_string());

    Ok(Response {
        response: raw.response.unwrap_or_default(),
        session_id: raw.session_id,
        cost_usd: None,
        backend: String::new(),
        error,
    })
}

/// Streaming variant — gemini's --output-format stream-json emits:
///   - `{"type":"init","session_id":"..."}` - session id source
///   - `{"type":"message","role":"assistant","content":"...","delta":true}` - text deltas
///   - `{"type":"result","status":"success",...}` - terminal
pub async fn ask_stream(
    cfg: &BackendConfig,
    req: &AskRequest,
    tx: &mpsc::Sender<StreamEvent>,
) -> anyhow::Result<Response> {
    let cmd = build_command(cfg, req);
    // Override to stream-json
    let args: Vec<_> = cmd.as_std().get_args().collect::<Vec<_>>();
    let new_args: Vec<String> = args
        .into_iter()
        .map(|a| {
            if a == std::ffi::OsStr::new("json") {
                "stream-json".to_string()
            } else {
                a.to_string_lossy().into_owned()
            }
        })
        .collect();
    let mut cmd = Command::new("gemini");
    for a in &new_args {
        cmd.arg(a);
    }
    if let Some(dir) = req.dir.as_deref().or(cfg.dir.as_deref()) {
        cmd.current_dir(dir);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("gemini: stdout was not piped"))?;
    let mut lines = BufReader::new(stdout).lines();

    let mut full_text = String::new();
    let mut session_id = None;

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("init") => {
                session_id = v.get("session_id").and_then(Value::as_str).map(String::from);
            }
            Some("message") => {
                if v.get("role").and_then(Value::as_str) == Some("assistant") {
                    if let Some(text) = v.get("content").and_then(Value::as_str) {
                        let is_delta = v.get("delta").and_then(Value::as_bool).unwrap_or(false);
                        if is_delta {
                            full_text.push_str(text);
                            if tx
                                .send(StreamEvent::Delta {
                                    text: text.to_string(),
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                        } else if text.len() > full_text.len() {
                            // Cumulative-style update — emit suffix
                            let suffix = text[full_text.len()..].to_string();
                            full_text = text.to_string();
                            let _ = tx.send(StreamEvent::Delta { text: suffix }).await;
                        }
                    }
                }
            }
            Some("result") => {
                // session_id was already captured from init
            }
            _ => {}
        }
    }

    let status = child.wait().await?;
    let error = if !status.success() {
        Some(format!("gemini exited {status}"))
    } else {
        None
    };

    Ok(Response {
        response: full_text,
        session_id,
        cost_usd: None,
        backend: String::new(),
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BackendConfig {
        BackendConfig {
            enabled: true,
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
        let cmd = build_command(&cfg(), &req("hi"));
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("-p")));
        assert!(args.contains(&std::ffi::OsStr::new("hi")));
    }

    #[test]
    fn build_command_yolo_for_skip_perms() {
        let mut c = cfg();
        c.skip_permissions = true;
        let cmd = build_command(&c, &req("hi"));
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--yolo")));
    }

    #[test]
    fn build_command_resume() {
        let mut r = req("hi");
        r.session_id = Some("latest".into());
        let cmd = build_command(&cfg(), &r);
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--resume")));
        assert!(args.contains(&std::ffi::OsStr::new("latest")));
    }

    #[test]
    fn build_command_ignores_effort() {
        // Gemini has no --effort flag, but request shouldn't break
        let mut r = req("hi");
        r.effort = Some("max".into());
        let cmd = build_command(&cfg(), &r);
        let args = args_of(&cmd);
        assert!(!args.contains(&std::ffi::OsStr::new("--effort")));
        assert!(!args.contains(&std::ffi::OsStr::new("max")));
    }

    #[test]
    fn extract_json_strips_banner() {
        let stdout = "YOLO mode is enabled.\nYOLO mode is enabled.\n{\"session_id\":\"abc\",\"response\":\"pong\"}";
        let json = extract_json(stdout).unwrap();
        assert!(json.starts_with("{"));
    }

    #[test]
    fn extract_json_returns_none_if_no_brace() {
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn parse_response_basic() {
        let json = r#"{"session_id":"abc-123","response":"pong","stats":{}}"#;
        let raw: RawResponse = serde_json::from_str(json).unwrap();
        assert_eq!(raw.response.as_deref(), Some("pong"));
        assert_eq!(raw.session_id.as_deref(), Some("abc-123"));
    }
}
