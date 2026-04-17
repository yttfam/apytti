use std::process::Stdio;

use tokio::process::Command;

use super::{AskRequest, Response};
use crate::persist::BackendConfig;

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

    for rule in &cfg.allow {
        cmd.arg("--allowedTools").arg(rule);
    }

    if let Some(ref dir) = cfg.dir {
        cmd.current_dir(dir);
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

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
