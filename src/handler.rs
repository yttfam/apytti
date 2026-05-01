use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::http::header::HeaderMap;
use axum::Json;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use axum::extract::Path;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;

use crate::backend::{dispatch, dispatch_stream, AskRequest, BackendKind};
use crate::error::AppError;
use crate::models::{self, ModelsCache};
use crate::persist::PersistedConfig;
use crate::schema;
use crate::sessions;
use crate::stream::StreamEvent;

/// Mutable shared server state. Config is wrapped in RwLock so PUT /config can persist updates
/// without dropping the server.
pub struct ServerState {
    pub config: RwLock<PersistedConfig>,
    pub config_path: PathBuf,
    /// Per-(backend, dir, session_id) lock — serializes concurrent /api/ask calls to the same
    /// session so apytti doesn't fork itself. External processes (interactive claude) aren't
    /// covered here; use GET /backends/{name}/sessions/{sid}/status for that.
    pub session_locks: Mutex<HashMap<(String, String, String), Arc<Mutex<()>>>>,
}

impl ServerState {
    pub fn new(config: PersistedConfig, config_path: PathBuf) -> Self {
        Self {
            config: RwLock::new(config),
            config_path,
            session_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Get-or-create the mutex for a given session triple.
    pub async fn session_lock(
        &self,
        backend: &str,
        dir: Option<&str>,
        sid: &str,
    ) -> Arc<Mutex<()>> {
        let key = (
            backend.to_string(),
            dir.unwrap_or("").to_string(),
            sid.to_string(),
        );
        let mut locks = self.session_locks.lock().await;
        locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct AskRequestBody {
    pub prompt: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub backend: Option<String>,
    /// When true, returns text/event-stream instead of a single JSON response.
    #[serde(default)]
    pub stream: bool,
    /// Per-request working directory override. Combines with `session_id` to
    /// resume sessions in different project directories from a single apytti.
    pub dir: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub active_backend: Option<String>,
    pub enabled_backends: Vec<String>,
}

pub async fn ask(
    state: Arc<ServerState>,
    Json(body): Json<AskRequestBody>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;

    if body.prompt.is_empty() {
        return Err(AppError::BadRequest("prompt is required".into()));
    }

    // Snapshot config once per call so the rest of the handler doesn't hold the lock across await.
    let (kind, cfg) = {
        let snapshot = state.config.read().await;
        let kind = resolve_backend(&snapshot, body.backend.as_deref())?;
        let cfg = snapshot.backend(kind);
        if !cfg.enabled {
            return Err(AppError::BadRequest(format!(
                "backend {kind} is not enabled. Run `apytti setup` to configure it.",
            )));
        }
        (kind, cfg)
    };

    let prompt_preview: String = body.prompt.chars().take(100).collect();
    info!(
        backend = kind.as_str(),
        session_id = body.session_id.as_deref().unwrap_or("-"),
        model = body.model.as_deref().unwrap_or("-"),
        effort = body.effort.as_deref().unwrap_or("-"),
        stream = body.stream,
        "ask: {prompt_preview}{}",
        if body.prompt.len() > 100 { "..." } else { "" }
    );

    let req = AskRequest {
        prompt: body.prompt,
        session_id: body.session_id,
        model: body.model,
        effort: body.effort,
        dir: body.dir,
    };

    // Acquire per-(backend, dir, sid) mutex if a session_id is set — serializes
    // concurrent calls from apytti to the same session. External processes are NOT
    // covered (use GET /backends/{name}/sessions/{sid}/status to detect those).
    let _lock_guard = if let Some(sid) = req.session_id.as_deref() {
        let lock = state
            .session_lock(kind.as_str(), req.dir.as_deref(), sid)
            .await;
        Some(lock.lock_owned().await)
    } else {
        None
    };

    if body.stream {
        let rx = dispatch_stream(kind, cfg, req);
        let stream = sse_stream_from_rx(rx);
        let sse = Sse::new(stream).keep_alive(KeepAlive::default());
        return Ok(sse.into_response());
    }

    let start = std::time::Instant::now();
    let resp = dispatch(kind, &cfg, &req).await;
    let elapsed = start.elapsed();

    info!(
        backend = resp.backend.as_str(),
        session_id = resp.session_id.as_deref().unwrap_or("-"),
        cost_usd = resp.cost_usd.unwrap_or(0.0),
        elapsed_ms = elapsed.as_millis() as u64,
        error = resp.error.as_deref().unwrap_or("-"),
        "done"
    );

    debug!(
        response_len = resp.response.len(),
        "response: {}{}",
        &resp.response.chars().take(200).collect::<String>(),
        if resp.response.len() > 200 { "..." } else { "" }
    );

    Ok(Json(resp).into_response())
}

/// Convert an mpsc receiver of StreamEvents into an SSE-compatible Stream.
fn sse_stream_from_rx(
    mut rx: tokio::sync::mpsc::Receiver<StreamEvent>,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    async_stream::stream! {
        while let Some(event) = rx.recv().await {
            let name = event.sse_event();
            let data = serde_json::to_string(&event)
                .unwrap_or_else(|_| String::from("{\"type\":\"error\",\"error\":\"serialize failed\"}"));
            yield Ok(Event::default().event(name).data(data));
        }
    }
}

fn resolve_backend(cfg: &PersistedConfig, requested: Option<&str>) -> Result<BackendKind, AppError> {
    if let Some(name) = requested {
        return BackendKind::parse(name)
            .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")));
    }
    cfg.active.ok_or_else(|| {
        AppError::BadRequest(
            "no backend specified and no active backend configured. Run `apytti setup`.".into(),
        )
    })
}

pub async fn help() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("help.html"))
}

pub async fn health(state: Arc<ServerState>) -> Json<HealthResponse> {
    let snapshot = state.config.read().await;
    let active_backend = snapshot.active.map(|k| k.to_string());
    let enabled_backends: Vec<String> = BackendKind::ALL
        .iter()
        .filter(|k| snapshot.backend(**k).enabled)
        .map(|k| k.to_string())
        .collect();

    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        active_backend,
        enabled_backends,
    })
}

/// GET /config — returns the current PersistedConfig as JSON. Includes ALL four backends
/// (even if not configured) so hermytt can render empty forms.
pub async fn get_config(state: Arc<ServerState>) -> Json<serde_json::Value> {
    let snapshot = state.config.read().await;

    let mut backends = serde_json::Map::new();
    for kind in BackendKind::ALL {
        let cfg = snapshot.backend(kind);
        backends.insert(
            kind.as_str().to_string(),
            serde_json::to_value(cfg).unwrap_or(serde_json::Value::Null),
        );
    }

    let mut out = serde_json::Map::new();
    if let Some(a) = snapshot.active {
        out.insert("active".into(), serde_json::Value::String(a.to_string()));
    } else {
        out.insert("active".into(), serde_json::Value::Null);
    }
    out.insert("backends".into(), serde_json::Value::Object(backends));
    if let Some(h) = &snapshot.hermytt {
        let mut redacted = h.clone();
        redacted.token = redacted.token.map(|_| "***".into());
        redacted.config_token = redacted.config_token.map(|_| "***".into());
        out.insert(
            "hermytt".into(),
            serde_json::to_value(redacted).unwrap_or(serde_json::Value::Null),
        );
    }

    Json(serde_json::Value::Object(out))
}

/// PUT /config — merge incoming config and persist to ~/.apytti/config.toml.
/// Auth: if `hermytt.config_token` is set in the current config, requests must send
/// `X-Hermytt-Key: <token>`. If not set, the endpoint is open.
pub async fn put_config(
    state: Arc<ServerState>,
    headers: HeaderMap,
    Json(incoming): Json<PersistedConfig>,
) -> Result<Json<serde_json::Value>, AppError> {
    {
        let snapshot = state.config.read().await;
        if let Some(expected) = snapshot
            .hermytt
            .as_ref()
            .and_then(|h| h.config_token.as_deref())
        {
            let provided = headers.get("x-hermytt-key").and_then(|v| v.to_str().ok());
            if provided != Some(expected) {
                return Err(AppError::BadRequest("unauthorized: invalid X-Hermytt-Key".into()));
            }
        }
    }

    let mut snapshot = state.config.write().await;
    snapshot.merge(incoming);
    if let Err(e) = snapshot.save(&state.config_path) {
        warn!("failed to persist config: {e}");
        return Err(AppError::Internal(format!("failed to persist config: {e}")));
    }
    info!(path = ?state.config_path, "config updated via PUT /config");

    Ok(Json(serde_json::json!({"ok": true})))
}

/// GET /backends/schema — static description of each backend's fields.
pub async fn get_backends_schema() -> Json<serde_json::Value> {
    Json(schema::backends_schema())
}

/// GET /models — return the current models cache (may be empty if never inited).
pub async fn get_models(state: Arc<ServerState>) -> Json<serde_json::Value> {
    let cache_path = ModelsCache::path_for(&state.config_path);
    let cache = ModelsCache::load(&cache_path).unwrap_or_default();
    Json(serde_json::to_value(&cache).unwrap_or(serde_json::Value::Null))
}

/// GET /backends/{name}/sessions — list sessions for a backend.
/// Optional ?dir=/path filters to one project; without it, returns sessions
/// across all projects.
pub async fn get_backend_sessions(
    Path(name): Path<String>,
    axum::extract::Query(q): axum::extract::Query<DirQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let kind = BackendKind::parse(&name)
        .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")))?;
    let sessions = sessions::list_sessions(kind, q.dir.as_deref());
    Ok(Json(serde_json::json!({"sessions": sessions})))
}

/// GET /backends/{name}/sessions/{sid}/messages — full conversation log.
/// Auth: same X-Hermytt-Key rule as PUT /config — message logs can contain
/// secrets the user typed, so they're never open even when listings are.
pub async fn get_backend_session_messages(
    state: Arc<ServerState>,
    headers: HeaderMap,
    Path((name, sid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    {
        let snapshot = state.config.read().await;
        if let Some(expected) = snapshot
            .hermytt
            .as_ref()
            .and_then(|h| h.config_token.as_deref())
        {
            let provided = headers.get("x-hermytt-key").and_then(|v| v.to_str().ok());
            if provided != Some(expected) {
                return Err(AppError::BadRequest("unauthorized: invalid X-Hermytt-Key".into()));
            }
        }
    }

    let kind = BackendKind::parse(&name)
        .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")))?;
    let log = sessions::read_messages(kind, &sid)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(serde_json::to_value(log).unwrap_or(serde_json::Value::Null)))
}

/// GET /backends/{name}/sessions/{sid}/status — detect whether the session
/// is currently being processed by some other process (external interactive
/// claude, etc).
pub async fn get_backend_session_status(
    Path((name, sid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let kind = BackendKind::parse(&name)
        .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")))?;
    let status = sessions::session_status(kind, &sid);
    Ok(Json(serde_json::to_value(status).unwrap_or(serde_json::Value::Null)))
}

/// GET /backends/{name}/projects — list projects for a backend, with session
/// counts and last-modified.
pub async fn get_backend_projects(
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let kind = BackendKind::parse(&name)
        .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")))?;
    let projects = sessions::list_projects(kind);
    Ok(Json(serde_json::json!({"projects": projects})))
}

/// DELETE /backends/{name}/sessions/{sid} — delete a single session.
/// Auth: same X-Hermytt-Key rule as PUT /config.
pub async fn delete_backend_session(
    state: Arc<ServerState>,
    headers: HeaderMap,
    Path((name, sid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    {
        let snapshot = state.config.read().await;
        if let Some(expected) = snapshot
            .hermytt
            .as_ref()
            .and_then(|h| h.config_token.as_deref())
        {
            let provided = headers.get("x-hermytt-key").and_then(|v| v.to_str().ok());
            if provided != Some(expected) {
                return Err(AppError::BadRequest("unauthorized: invalid X-Hermytt-Key".into()));
            }
        }
    }

    let kind = BackendKind::parse(&name)
        .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")))?;
    let removed = sessions::delete_session(kind, &sid)
        .map_err(|e| AppError::Internal(format!("delete failed: {e}")))?;
    if !removed {
        return Err(AppError::BadRequest(format!(
            "session not found: {sid} (backend: {name})"
        )));
    }
    info!(backend = name, session_id = sid, "session deleted");
    Ok(Json(serde_json::json!({"ok": true})))
}

#[derive(Debug, serde::Deserialize)]
pub struct DirQuery {
    pub dir: Option<String>,
}

/// GET /backends/{name}/models — single-backend list from cache.
pub async fn get_backend_models(
    state: Arc<ServerState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let kind = BackendKind::parse(&name)
        .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")))?;
    let cache_path = ModelsCache::path_for(&state.config_path);
    let cache = ModelsCache::load(&cache_path).unwrap_or_default();
    let entry = cache
        .get(kind)
        .cloned()
        .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::json!({"models": [], "via": "missing"}));
    Ok(Json(entry))
}

/// POST /models/init — probe every enabled backend, persist cache.
/// Optionally `?backend=claude` to refresh a single backend.
/// Auth: same `X-Hermytt-Key` rule as PUT /config.
pub async fn post_init_models(
    state: Arc<ServerState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<InitQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    {
        let snapshot = state.config.read().await;
        if let Some(expected) = snapshot
            .hermytt
            .as_ref()
            .and_then(|h| h.config_token.as_deref())
        {
            let provided = headers.get("x-hermytt-key").and_then(|v| v.to_str().ok());
            if provided != Some(expected) {
                return Err(AppError::BadRequest("unauthorized: invalid X-Hermytt-Key".into()));
            }
        }
    }

    let cache_path = ModelsCache::path_for(&state.config_path);
    let snapshot = state.config.read().await.clone();

    let result = if let Some(name) = q.backend {
        let kind = BackendKind::parse(&name)
            .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")))?;
        info!(backend = kind.as_str(), "probing one backend for models");
        let entry = models::init_one(kind, &snapshot, &cache_path).await;
        let mut out = ModelsCache::load(&cache_path).unwrap_or_default();
        out.set(kind, entry);
        out
    } else {
        info!("probing all enabled backends for models");
        models::init_all(&snapshot, &cache_path).await
    };

    Ok(Json(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null)))
}

#[derive(Debug, serde::Deserialize)]
pub struct InitQuery {
    pub backend: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_ask_request_minimal() {
        let json = r#"{"prompt": "hello"}"#;
        let req: AskRequestBody = serde_json::from_str(json).unwrap();
        assert_eq!(req.prompt, "hello");
        assert!(req.session_id.is_none());
        assert!(req.backend.is_none());
    }

    #[test]
    fn deserialize_ask_request_with_backend() {
        let json = r#"{"prompt": "hi", "backend": "ollama", "model": "llama3.2"}"#;
        let req: AskRequestBody = serde_json::from_str(json).unwrap();
        assert_eq!(req.backend.as_deref(), Some("ollama"));
        assert_eq!(req.model.as_deref(), Some("llama3.2"));
    }

    #[test]
    fn resolve_backend_uses_request_override() {
        let cfg = PersistedConfig {
            active: Some(BackendKind::Claude),
            ..Default::default()
        };
        let kind = resolve_backend(&cfg, Some("ollama")).unwrap();
        assert_eq!(kind, BackendKind::Ollama);
    }

    #[test]
    fn resolve_backend_falls_back_to_active() {
        let cfg = PersistedConfig {
            active: Some(BackendKind::Claude),
            ..Default::default()
        };
        let kind = resolve_backend(&cfg, None).unwrap();
        assert_eq!(kind, BackendKind::Claude);
    }

    #[test]
    fn resolve_backend_no_active_errors() {
        let cfg = PersistedConfig::default();
        assert!(resolve_backend(&cfg, None).is_err());
    }

    #[test]
    fn resolve_backend_unknown_name_errors() {
        let cfg = PersistedConfig::default();
        assert!(resolve_backend(&cfg, Some("bogus")).is_err());
    }
}
