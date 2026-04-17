use std::process::Stdio;

use serde_json::Value;
use tokio::process::Command;

use super::{AskRequest, Response};
use crate::persist::BackendConfig;

pub fn build_command(cfg: &BackendConfig, req: &AskRequest) -> Command {
    let mut cmd = Command::new("copilot");
    cmd.arg("-p").arg(&req.prompt);
    cmd.arg("--output-format").arg("json");

    // Copilot only has --resume, no separate --session-id flag.
    if let Some(sid) = req.session_id.as_deref().or(cfg.session_id.as_deref()) {
        cmd.arg(format!("--resume={sid}"));
    }

    if let Some(m) = req.model.as_deref().or(cfg.model.as_deref()) {
        cmd.arg("--model").arg(m);
    }

    if let Some(e) = req.effort.as_deref().or(cfg.effort.as_deref()) {
        cmd.arg("--effort").arg(e);
    }

    if cfg.skip_permissions {
        cmd.arg("--allow-all");
    }

    for rule in &cfg.allow {
        cmd.arg("--allow-tool").arg(rule);
    }

    if let Some(ref dir) = cfg.dir {
        cmd.arg("--add-dir").arg(dir);
        cmd.current_dir(dir);
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    cmd
}

/// Parse Copilot's JSONL stream. Returns (response_text, session_id).
pub fn parse_jsonl(stdout: &str) -> (String, Option<String>) {
    let mut response = String::new();
    let mut session_id = None;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("assistant.message") => {
                if let Some(c) = v.get("data").and_then(|d| d.get("content")).and_then(Value::as_str) {
                    response.push_str(c);
                }
            }
            Some("result") => {
                if let Some(s) = v.get("sessionId").and_then(Value::as_str) {
                    session_id = Some(s.to_string());
                }
            }
            _ => {}
        }
    }

    (response, session_id)
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
            error: Some(format!("copilot exited {}: {stderr}", output.status)),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (response, session_id) = parse_jsonl(&stdout);

    Ok(Response {
        response,
        session_id,
        cost_usd: None,
        backend: String::new(),
        error: None,
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
        assert!(args.contains(&std::ffi::OsStr::new("--output-format")));
        assert!(args.contains(&std::ffi::OsStr::new("json")));
    }

    #[test]
    fn build_command_resume_uses_equals_form() {
        let mut r = req("hi");
        r.session_id = Some("abc".into());
        let cmd = build_command(&cfg(), &r);
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--resume=abc")));
    }

    #[test]
    fn build_command_skip_perms_uses_allow_all() {
        let mut c = cfg();
        c.skip_permissions = true;
        let cmd = build_command(&c, &req("hi"));
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--allow-all")));
    }

    #[test]
    fn build_command_allow_uses_singular_flag() {
        let mut c = cfg();
        c.allow = vec!["shell(git)".into()];
        let cmd = build_command(&c, &req("hi"));
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--allow-tool")));
        assert!(args.contains(&std::ffi::OsStr::new("shell(git)")));
    }

    #[test]
    fn build_command_dir_uses_add_dir() {
        let mut c = cfg();
        c.dir = Some("/tmp/test".into());
        let cmd = build_command(&c, &req("hi"));
        let args = args_of(&cmd);
        assert!(args.contains(&std::ffi::OsStr::new("--add-dir")));
        assert!(args.contains(&std::ffi::OsStr::new("/tmp/test")));
    }

    #[test]
    fn parse_jsonl_extracts_message_and_session() {
        let stdout = r#"{"type":"session.warning","data":{}}
{"type":"assistant.message_delta","data":{"deltaContent":"hel"},"ephemeral":true}
{"type":"assistant.message_delta","data":{"deltaContent":"lo"},"ephemeral":true}
{"type":"assistant.message","data":{"content":"hello","toolRequests":[]}}
{"type":"result","sessionId":"sess-123","exitCode":0}"#;
        let (resp, sid) = parse_jsonl(stdout);
        assert_eq!(resp, "hello");
        assert_eq!(sid.as_deref(), Some("sess-123"));
    }

    #[test]
    fn parse_jsonl_handles_empty() {
        let (resp, sid) = parse_jsonl("");
        assert!(resp.is_empty());
        assert!(sid.is_none());
    }

    #[test]
    fn parse_jsonl_skips_unparseable_lines() {
        let stdout = "not json\n{\"type\":\"assistant.message\",\"data\":{\"content\":\"ok\"}}\nbroken{";
        let (resp, _) = parse_jsonl(stdout);
        assert_eq!(resp, "ok");
    }
}
