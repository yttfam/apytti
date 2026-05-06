#![allow(clippy::field_reassign_with_default)]
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use apytti::handler::ServerState;
use apytti::persist::{BackendConfig, HermyttConfig, PersistedConfig};
use apytti::BackendKind;
use reqwest::Client as Http;

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn temp_config_path() -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // Leak the dir so it lives for the duration of the test
    std::mem::forget(dir);
    path
}

async fn start_server(port: u16, config: PersistedConfig) -> PathBuf {
    let path = temp_config_path();
    let state = Arc::new(ServerState::new(config, path.clone()));

    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let app = apytti::build_router(state);
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    path
}

fn config_with_claude() -> PersistedConfig {
    let mut cfg = PersistedConfig::default();
    cfg.active = Some(BackendKind::Claude);
    cfg.set_backend(
        BackendKind::Claude,
        BackendConfig {
            enabled: true,
            ..Default::default()
        },
    );
    cfg
}

#[tokio::test]
async fn health_endpoint() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["active_backend"], "claude");
    assert!(body["enabled_backends"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "claude"));
}

#[tokio::test]
async fn ask_empty_prompt_returns_400() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/api/ask"))
        .json(&serde_json::json!({"prompt": ""}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("prompt or attachments required"));
}

#[tokio::test]
async fn ask_missing_prompt_returns_422() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/api/ask"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 422);
}

#[tokio::test]
async fn ask_unknown_backend_returns_400() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/api/ask"))
        .json(&serde_json::json!({"prompt": "hi", "backend": "bogus"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("unknown backend"));
}

#[tokio::test]
async fn ask_disabled_backend_returns_400() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/api/ask"))
        .json(&serde_json::json!({"prompt": "hi", "backend": "copilot"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("not enabled"));
}

#[tokio::test]
async fn ask_no_active_backend_returns_400() {
    let port = free_port();
    start_server(port, PersistedConfig::default()).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/api/ask"))
        .json(&serde_json::json!({"prompt": "hi"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn help_endpoint_returns_html() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/help"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("apytti"));
}

#[tokio::test]
async fn get_config_returns_all_four_backends() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/config"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["active"], "claude");
    let backends = body["backends"].as_object().unwrap();
    for k in ["claude", "copilot", "gemini", "ollama"] {
        assert!(backends.contains_key(k), "missing backend: {k}");
    }
    assert_eq!(backends["claude"]["enabled"], true);
    assert_eq!(backends["copilot"]["enabled"], false);
}

#[tokio::test]
async fn put_config_persists_and_merges() {
    let port = free_port();
    let path = start_server(port, config_with_claude()).await;

    // Enable copilot via PUT (partial update)
    let resp = Http::new()
        .put(format!("http://127.0.0.1:{port}/config"))
        .json(&serde_json::json!({
            "backends": {
                "copilot": {"enabled": true, "model": "claude-sonnet-4.6"}
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Verify file was written
    assert!(path.exists());
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("copilot"));
    assert!(written.contains("claude-sonnet-4.6"));

    // Verify GET reflects merged state (claude still enabled, copilot now too)
    let body: serde_json::Value = Http::new()
        .get(format!("http://127.0.0.1:{port}/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["backends"]["claude"]["enabled"], true);
    assert_eq!(body["backends"]["copilot"]["enabled"], true);
    assert_eq!(body["backends"]["copilot"]["model"], "claude-sonnet-4.6");
}

#[tokio::test]
async fn put_config_requires_token_when_set() {
    let port = free_port();
    let mut cfg = config_with_claude();
    cfg.hermytt = Some(HermyttConfig {
        url: "http://h:7777".into(),
        config_token: Some("secret".into()),
        ..Default::default()
    });
    start_server(port, cfg).await;

    // Without header — denied
    let resp = Http::new()
        .put(format!("http://127.0.0.1:{port}/config"))
        .json(&serde_json::json!({"active": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Wrong token — denied
    let resp = Http::new()
        .put(format!("http://127.0.0.1:{port}/config"))
        .header("X-Hermytt-Key", "wrong")
        .json(&serde_json::json!({"active": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Correct token — allowed
    let resp = Http::new()
        .put(format!("http://127.0.0.1:{port}/config"))
        .header("X-Hermytt-Key", "secret")
        .json(&serde_json::json!({"active": "claude"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn get_config_redacts_tokens() {
    let port = free_port();
    let mut cfg = config_with_claude();
    cfg.hermytt = Some(HermyttConfig {
        url: "http://h:7777".into(),
        token: Some("secret-registry-token".into()),
        config_token: Some("secret-config-token".into()),
        ..Default::default()
    });
    start_server(port, cfg).await;

    let body: serde_json::Value = Http::new()
        .get(format!("http://127.0.0.1:{port}/config"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["hermytt"]["token"], "***");
    assert_eq!(body["hermytt"]["config_token"], "***");
    assert_eq!(body["hermytt"]["url"], "http://h:7777");
}

#[tokio::test]
async fn get_claude_projects_returns_array() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/backends/claude/projects"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["projects"].is_array());
}

#[tokio::test]
async fn get_sessions_unknown_backend_returns_400() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/backends/bogus/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn delete_session_unknown_returns_400() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .delete(format!(
            "http://127.0.0.1:{port}/backends/claude/sessions/nonexistent-uuid-12345"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("session not found"));
}

#[tokio::test]
async fn delete_session_requires_token_when_set() {
    let port = free_port();
    let mut cfg = config_with_claude();
    cfg.hermytt = Some(HermyttConfig {
        url: "http://h:7777".into(),
        config_token: Some("secret".into()),
        ..Default::default()
    });
    start_server(port, cfg).await;

    // Without header — denied
    let resp = Http::new()
        .delete(format!(
            "http://127.0.0.1:{port}/backends/claude/sessions/anything"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("unauthorized"));
}

#[tokio::test]
async fn ask_request_accepts_dir_field() {
    // Just verifies the field is accepted by the API contract.
    // Full per-call dir behavior is covered in backend unit tests.
    let port = free_port();
    start_server(port, config_with_claude()).await;

    // Empty prompt still returns 400, but the request body parses successfully
    // including the new `dir` field — that's what we're checking here.
    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/api/ask"))
        .json(&serde_json::json!({
            "prompt": "",
            "backend": "claude",
            "dir": "/some/project",
            "session_id": "abc-123"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    // Failed because of empty prompt, not because dir was rejected
    assert!(body["error"].as_str().unwrap().contains("prompt or attachments required"));
}

#[tokio::test]
async fn get_models_returns_empty_when_no_cache() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/models"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // Empty cache serializes as {} (the flatten of an empty hashmap)
    assert!(body.is_object());
}

#[tokio::test]
async fn get_backend_models_returns_missing_when_uncached() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/backends/claude/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["via"], "missing");
    assert!(body["models"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn get_backend_models_unknown_backend_400() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/backends/bogus/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn init_models_requires_token_when_set() {
    let port = free_port();
    let mut cfg = config_with_claude();
    cfg.hermytt = Some(HermyttConfig {
        url: "http://h:7777".into(),
        config_token: Some("secret".into()),
        ..Default::default()
    });
    start_server(port, cfg).await;

    // Without header — denied (returns 400 from our error handler)
    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/models/init"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn backends_schema_endpoint() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .get(format!("http://127.0.0.1:{port}/backends/schema"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    for k in ["claude", "copilot", "gemini", "ollama"] {
        assert!(body[k]["fields"].is_array());
    }
    assert_eq!(body["claude"]["supports_effort"], true);
    assert_eq!(body["gemini"]["supports_effort"], false);
}
