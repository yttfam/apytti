use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use apytti::handler::ServerState;
use apytti::persist::{BackendConfig, PersistedConfig};
use apytti::BackendKind;
use reqwest::Client as Http;

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn start_server(port: u16, config: PersistedConfig) {
    let state = Arc::new(ServerState { config });

    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let app = apytti::build_router(state);
        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
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
    assert!(body["error"].as_str().unwrap().contains("prompt is required"));
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
    // claude is enabled but copilot isn't
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
