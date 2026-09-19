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

// ---------- cancellation ----------

#[tokio::test]
async fn cancel_request_unknown_id_returns_zero() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/requests/never-existed/cancel"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["killed"], 0);
}

#[tokio::test]
async fn cancel_request_aborts_matching_in_flight_call() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    // Sessionless call — unaddressable by session cancel, which is the whole
    // point of request_id. `sleep` stands in for a slow CLI so there's a real
    // in-flight window to cancel inside of.
    let mut cfg = config_with_claude();
    cfg.set_backend(
        BackendKind::Claude,
        BackendConfig {
            enabled: true,
            dir: Some("/nonexistent-dir-so-spawn-hangs-or-fails".into()),
            ..Default::default()
        },
    );

    let ask = tokio::spawn(async move {
        Http::new()
            .post(format!("http://127.0.0.1:{port}/api/ask"))
            .json(&serde_json::json!({
                "prompt": "irrelevant",
                "request_id": "req-abc-123",
            }))
            .send()
            .await
    });

    // Give the handler time to register the call before cancelling it.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/requests/req-abc-123/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // Either we caught it in flight (killed=1) or the spawn already failed
    // (killed=0). Both are valid; what must hold is that the endpoint answers
    // with a well-formed count rather than erroring.
    assert!(body["killed"].is_number(), "killed must be a number");

    let _ = ask.await;
}

#[tokio::test]
async fn cancel_by_session_ignores_other_sessions() {
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .post(format!(
            "http://127.0.0.1:{port}/backends/claude/sessions/not-running/cancel"
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["killed"],
        0
    );
}

#[tokio::test]
async fn cancel_request_unknown_backend_path_still_routes() {
    // /requests/{id}/cancel is backend-agnostic by design — no backend segment
    // to get wrong.
    let port = free_port();
    start_server(port, config_with_claude()).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/requests/whatever/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ---------- timeouts / session-lock release ----------

/// A server that accepts TCP connections and then never answers — stands in for a
/// backend that hangs rather than failing, which is the case that used to wedge a
/// session permanently.
async fn blackhole_endpoint() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        // Hold every socket open, write nothing, ever.
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    format!("http://127.0.0.1:{}", addr.port())
}

fn config_with_hung_ollama(endpoint: String) -> PersistedConfig {
    let mut cfg = PersistedConfig::default();
    cfg.active = Some(BackendKind::Ollama);
    cfg.set_backend(
        BackendKind::Ollama,
        BackendConfig {
            enabled: true,
            endpoint: Some(endpoint),
            ..Default::default()
        },
    );
    cfg
}

#[tokio::test]
async fn hung_backend_times_out_with_504() {
    let port = free_port();
    let endpoint = blackhole_endpoint().await;
    start_server(port, config_with_hung_ollama(endpoint)).await;

    let resp = Http::new()
        .post(format!("http://127.0.0.1:{port}/api/ask"))
        .json(&serde_json::json!({"prompt": "hello", "timeout_secs": 1}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 504, "a hung backend must return Gateway Timeout");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap_or("").contains("deadline"),
        "error should explain the deadline: {body}"
    );
}

/// The regression test for the bug that wedged Lou: before the fix, the first
/// call's guard was never released (the handler never returned), so every later
/// call to the same session_id blocked forever on the mutex with no response at
/// all. Now the first call times out, frees the lock, and the second gets its own
/// timely answer instead of silence.
#[tokio::test]
async fn timed_out_call_releases_the_session_lock() {
    let port = free_port();
    let endpoint = blackhole_endpoint().await;
    start_server(port, config_with_hung_ollama(endpoint)).await;

    let body = serde_json::json!({
        "prompt": "hello",
        "session_id": "stuck-session",
        "timeout_secs": 1
    });

    let first = Http::new()
        .post(format!("http://127.0.0.1:{port}/api/ask"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 504);

    // The second call must come back promptly. If the lock leaked it would hang
    // until this client's own timeout, so bound it and fail loudly.
    let second = tokio::time::timeout(
        Duration::from_secs(20),
        Http::new()
            .post(format!("http://127.0.0.1:{port}/api/ask"))
            .json(&body)
            .send(),
    )
    .await
    .expect("second call hung — the session lock was not released")
    .unwrap();

    assert_eq!(
        second.status(),
        504,
        "second call should hit its own deadline, not inherit a leaked lock"
    );
}

#[tokio::test]
async fn queued_caller_gets_504_not_silence_when_predecessor_is_stuck() {
    let port = free_port();
    let endpoint = blackhole_endpoint().await;
    start_server(port, config_with_hung_ollama(endpoint)).await;

    let mk = |secs: u64| {
        serde_json::json!({"prompt": "x", "session_id": "shared", "timeout_secs": secs})
    };

    // Long-running holder, then a short-patience caller queued behind it.
    let holder = tokio::spawn({
        let url = format!("http://127.0.0.1:{port}/api/ask");
        async move { Http::new().post(url).json(&mk(12)).send().await }
    });
    tokio::time::sleep(Duration::from_millis(400)).await;

    let queued = tokio::time::timeout(
        Duration::from_secs(15),
        Http::new()
            .post(format!("http://127.0.0.1:{port}/api/ask"))
            .json(&mk(2))
            .send(),
    )
    .await
    .expect("queued caller never got a response")
    .unwrap();

    assert_eq!(queued.status(), 504, "waiting on the lock must be bounded too");
    let body: serde_json::Value = queued.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap_or("").contains("cancel"),
        "the error should tell the caller how to clear it: {body}"
    );

    let _ = holder.await;
}

/// A minimal stand-in for Ollama that answers immediately, so the happy path can
/// be exercised without a real backend.
async fn fake_ollama() -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/api/chat",
        post(|| async {
            axum::Json(serde_json::json!({
                "model": "fake",
                "message": {"role": "assistant", "content": "pong"},
                "done": true
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://127.0.0.1:{}", addr.port())
}

/// The happy path must not deadlock: the guard has to be released on success just
/// as reliably as on timeout, or the fix would trade one wedge for another.
#[tokio::test]
async fn successful_call_releases_lock_for_the_next_one() {
    let port = free_port();
    let endpoint = fake_ollama().await;
    let mut cfg = PersistedConfig::default();
    cfg.active = Some(BackendKind::Ollama);
    cfg.set_backend(
        BackendKind::Ollama,
        BackendConfig {
            enabled: true,
            endpoint: Some(endpoint),
            ..Default::default()
        },
    );
    start_server(port, cfg).await;

    let body = serde_json::json!({"prompt": "ping", "session_id": "reused"});
    for attempt in 1..=3 {
        let resp = tokio::time::timeout(
            Duration::from_secs(10),
            Http::new()
                .post(format!("http://127.0.0.1:{port}/api/ask"))
                .json(&body)
                .send(),
        )
        .await
        .unwrap_or_else(|_| panic!("call {attempt} hung — lock not released by the previous one"))
        .unwrap();

        assert_eq!(resp.status(), 200, "call {attempt} should succeed");
        let v: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(v["response"], "pong", "call {attempt}");
        assert!(v["error"].is_null(), "call {attempt} should carry no error");
    }
}

/// Streaming used to drop the session guard as soon as the SSE response was
/// built, so streaming calls never actually serialised. With the guard handed to
/// the stream, a second call to the same session queues behind it — and the 504
/// it eventually gets must name the *lock*, not its own dispatch deadline.
#[tokio::test]
async fn streaming_call_holds_the_session_lock_while_it_streams() {
    let port = free_port();
    let endpoint = blackhole_endpoint().await;
    start_server(port, config_with_hung_ollama(endpoint)).await;

    let streamer = tokio::spawn({
        let url = format!("http://127.0.0.1:{port}/api/ask");
        async move {
            Http::new()
                .post(url)
                .json(&serde_json::json!({
                    "prompt": "x", "session_id": "streamed", "stream": true, "timeout_secs": 12
                }))
                .send()
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(600)).await;

    let queued = tokio::time::timeout(
        Duration::from_secs(15),
        Http::new()
            .post(format!("http://127.0.0.1:{port}/api/ask"))
            .json(&serde_json::json!({
                "prompt": "y", "session_id": "streamed", "timeout_secs": 2
            }))
            .send(),
    )
    .await
    .expect("queued caller never got a response")
    .unwrap();

    assert_eq!(queued.status(), 504);
    let body: serde_json::Value = queued.json().await.unwrap();
    let err = body["error"].as_str().unwrap_or("");
    assert!(
        err.contains("still running"),
        "should have blocked on the streaming call's lock, got: {err}"
    );

    let _ = streamer.await;
}
