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

use crate::attachments::{self, Attachment};
use crate::backend::{dispatch, dispatch_stream, AskRequest, BackendKind};
use crate::error::AppError;
use crate::customizations;
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
    /// Registry of in-flight /api/ask calls, keyed by a unique token. Each entry
    /// is `(backend, sid, abort_handle)`. Cancellation endpoints look up entries
    /// by `(backend, sid)` (cancel by session) or drain all (kill switch).
    /// Sessionless calls get a synthetic sid prefixed `__nosession__<uuid>`.
    pub in_flight: Mutex<HashMap<u64, InFlight>>,
    /// Monotonic counter for in_flight tokens.
    pub next_in_flight: std::sync::atomic::AtomicU64,
}

#[derive(Debug)]
pub struct InFlight {
    pub backend: String,
    pub sid: String,
    pub abort: tokio::task::AbortHandle,
}

impl ServerState {
    pub fn new(config: PersistedConfig, config_path: PathBuf) -> Self {
        Self {
            config: RwLock::new(config),
            config_path,
            session_locks: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
            next_in_flight: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Register an in-flight /api/ask call and return its token. Use the token
    /// to deregister via `unregister_in_flight` when the call returns. The
    /// `AbortHandle` is what the cancel endpoints invoke.
    pub async fn register_in_flight(
        &self,
        backend: &str,
        sid: &str,
        abort: tokio::task::AbortHandle,
    ) -> u64 {
        use std::sync::atomic::Ordering;
        let token = self.next_in_flight.fetch_add(1, Ordering::SeqCst);
        self.in_flight.lock().await.insert(
            token,
            InFlight {
                backend: backend.to_string(),
                sid: sid.to_string(),
                abort,
            },
        );
        token
    }

    pub async fn unregister_in_flight(&self, token: u64) {
        self.in_flight.lock().await.remove(&token);
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
    /// Per-request claude agent override. Maps to `claude --agent <name>`.
    pub agent: Option<String>,
    /// Optional file attachments. Each `path` must be absolute and exist on
    /// the apytti host's filesystem. Apytti prepends a reference line per
    /// attachment to the prompt and (for the claude CLI backend) mints a
    /// per-call `Read(<path>)` allow rule so reads succeed without
    /// `--dangerously-skip-permissions`. When `[security] attachment_roots`
    /// is set in config, paths must live inside one of those roots.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// If set, apytti reads `~/.claude/commands/<command>.md`, substitutes
    /// `$ARGUMENTS` with `prompt`, and submits the expanded text as the
    /// actual prompt. Slash-style command templating without needing claude's
    /// TUI to be running.
    pub command: Option<String>,
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

    if body.prompt.is_empty() && body.attachments.is_empty() {
        return Err(AppError::BadRequest("prompt or attachments required".into()));
    }

    // If `command` is set, expand the template (substituting $ARGUMENTS with
    // body.prompt) and use the expanded text as the actual prompt.
    let mut effective_prompt = body.prompt;
    if let Some(cmd_name) = &body.command {
        match customizations::expand_command(cmd_name, &effective_prompt) {
            Ok(expanded) => effective_prompt = expanded,
            Err(e) => {
                return Err(AppError::BadRequest(format!(
                    "command expansion failed: {e}"
                )));
            }
        }
    }

    // Snapshot config once per call so the rest of the handler doesn't hold the lock across await.
    let (kind, cfg, attachment_roots) = {
        let snapshot = state.config.read().await;
        let kind = resolve_backend(&snapshot, body.backend.as_deref())?;
        let cfg = snapshot.backend(kind);
        if !cfg.enabled {
            return Err(AppError::BadRequest(format!(
                "backend {kind} is not enabled. Run `apytti setup` to configure it.",
            )));
        }
        let roots = snapshot
            .security
            .as_ref()
            .map(|s| s.attachment_roots.clone())
            .unwrap_or_default();
        (kind, cfg, roots)
    };

    // Resolve attachments: validate `path` form, materialize `data` form into
    // a per-request inbox dir under ~/.apytti/inbox/. The guard schedules a
    // deferred (5-minute by default) cleanup on drop so external agents who
    // receive the path in the response can still read it.
    let inbox_root = dirs::home_dir()
        .map(|h| h.join(".apytti").join("inbox"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/apytti-inbox"));
    let (resolved_attachments, _inbox_guard) =
        attachments::resolve(&body.attachments, &attachment_roots, &inbox_root)
            .map_err(AppError::BadRequest)?;
    if !resolved_attachments.is_empty() {
        let prefix = attachments::prompt_prefix(&resolved_attachments);
        effective_prompt = format!("{prefix}{effective_prompt}");
    }
    let extra_allow = attachments::allow_rules(&resolved_attachments);

    let prompt_preview: String = effective_prompt.chars().take(100).collect();
    info!(
        backend = kind.as_str(),
        session_id = body.session_id.as_deref().unwrap_or("-"),
        model = body.model.as_deref().unwrap_or("-"),
        effort = body.effort.as_deref().unwrap_or("-"),
        agent = body.agent.as_deref().unwrap_or("-"),
        command = body.command.as_deref().unwrap_or("-"),
        stream = body.stream,
        "ask: {prompt_preview}{}",
        if effective_prompt.len() > 100 { "..." } else { "" }
    );

    let req = AskRequest {
        prompt: effective_prompt,
        session_id: body.session_id,
        model: body.model,
        effort: body.effort,
        dir: body.dir,
        agent: body.agent,
        extra_allow,
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

    // Synthetic sid for sessionless calls so the in-flight registry has a key
    // we can match against in the cancel endpoints.
    let registry_sid = req
        .session_id
        .clone()
        .unwrap_or_else(|| format!("__nosession__{}", uuid::Uuid::new_v4()));

    if body.stream {
        let (rx, abort) = dispatch_stream(kind, cfg, req);
        let token = state
            .register_in_flight(kind.as_str(), &registry_sid, abort)
            .await;
        let state_for_dereg = state.clone();
        let stream = sse_stream_from_rx(rx, move || {
            let s = state_for_dereg.clone();
            tokio::spawn(async move { s.unregister_in_flight(token).await });
        });
        let sse = Sse::new(stream).keep_alive(KeepAlive::default());
        return Ok(sse.into_response());
    }

    // Non-streaming path: spawn dispatch on a tokio task so its AbortHandle
    // can be registered. AskRequest + BackendConfig are cheap to clone (small
    // strings + Vec<String>). When AbortHandle::abort is called, the task's
    // future drops, dropping the underlying tokio::process::Child, which
    // SIGKILLs the subprocess via `kill_on_drop(true)`.
    let start = std::time::Instant::now();
    let req_owned = req.clone();
    let cfg_owned = cfg.clone();
    let join = tokio::spawn(async move { dispatch(kind, &cfg_owned, &req_owned).await });
    let token = state
        .register_in_flight(kind.as_str(), &registry_sid, join.abort_handle())
        .await;
    let outcome = join.await;
    state.unregister_in_flight(token).await;
    let resp = match outcome {
        Ok(resp) => resp,
        Err(e) if e.is_cancelled() => {
            return Err(AppError::BadRequest("cancelled".into()));
        }
        Err(e) => {
            return Err(AppError::Internal(format!("dispatch task failed: {e}")));
        }
    };
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
/// `on_finish` is invoked when the receiver closes (normal completion OR
/// abort) so the caller can deregister from the in-flight registry.
fn sse_stream_from_rx<F>(
    mut rx: tokio::sync::mpsc::Receiver<StreamEvent>,
    on_finish: F,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>>
where
    F: FnOnce() + Send + 'static,
{
    async_stream::stream! {
        let mut cleanup = Some(on_finish);
        while let Some(event) = rx.recv().await {
            let name = event.sse_event();
            let data = serde_json::to_string(&event)
                .unwrap_or_else(|_| String::from("{\"type\":\"error\",\"error\":\"serialize failed\"}"));
            yield Ok(Event::default().event(name).data(data));
        }
        if let Some(cb) = cleanup.take() { cb(); }
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

/// GET /config-ui — tiny self-contained settings page that reads
/// /backends/schema + /config + /health and PUTs back to /config. Lets a
/// standalone apytti install (no hermytt) do first-run setup from a browser
/// instead of editing ~/.apytti/config.toml by hand.
pub async fn config_ui() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("config_ui.html"))
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

// ---------- MCP servers ----------

/// GET /backends/claude/mcp — list registered MCP servers.
pub async fn get_mcp_servers(
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if BackendKind::parse(&name) != Some(BackendKind::Claude) {
        return Err(AppError::BadRequest(format!(
            "MCP discovery only implemented for claude (got: {name})"
        )));
    }
    let servers = customizations::list_mcp_servers();
    Ok(Json(serde_json::json!({"servers": servers})))
}

#[derive(Debug, serde::Deserialize)]
pub struct AddMcpBody {
    pub name: String,
    /// "http" | "sse" | "stdio"
    pub transport: String,
    /// URL for http/sse, command for stdio
    pub target: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub headers: Vec<String>,
    /// "user" | "project" | "local". Defaults to user.
    pub scope: Option<String>,
}

pub async fn post_mcp_server(
    state: Arc<ServerState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<AddMcpBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_hermytt_key(&state, &headers).await?;
    if BackendKind::parse(&name) != Some(BackendKind::Claude) {
        return Err(AppError::BadRequest(format!(
            "MCP add only implemented for claude (got: {name})"
        )));
    }
    customizations::add_mcp_server(
        &body.name,
        &body.transport,
        &body.target,
        &body.args,
        &body.headers,
        body.scope.as_deref(),
    )
    .map_err(|e| AppError::BadRequest(format!("add failed: {e}")))?;
    info!(mcp = body.name, "MCP server added");
    Ok(Json(serde_json::json!({"ok": true})))
}

pub async fn delete_mcp_server(
    state: Arc<ServerState>,
    headers: HeaderMap,
    Path((backend, server)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_hermytt_key(&state, &headers).await?;
    if BackendKind::parse(&backend) != Some(BackendKind::Claude) {
        return Err(AppError::BadRequest("MCP only implemented for claude".into()));
    }
    customizations::remove_mcp_server(&server, None)
        .map_err(|e| AppError::BadRequest(format!("remove failed: {e}")))?;
    info!(mcp = server, "MCP server removed");
    Ok(Json(serde_json::json!({"ok": true})))
}

// ---------- Custom commands ----------

pub async fn get_commands(Path(name): Path<String>) -> Result<Json<serde_json::Value>, AppError> {
    if BackendKind::parse(&name) != Some(BackendKind::Claude) {
        return Err(AppError::BadRequest(
            "commands only implemented for claude".into(),
        ));
    }
    let commands = customizations::list_commands();
    Ok(Json(serde_json::json!({"commands": commands})))
}

pub async fn get_command(
    Path((backend, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    if BackendKind::parse(&backend) != Some(BackendKind::Claude) {
        return Err(AppError::BadRequest(
            "commands only implemented for claude".into(),
        ));
    }
    let cmd = customizations::read_command(&name)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(serde_json::to_value(cmd).unwrap_or(serde_json::Value::Null)))
}

#[derive(Debug, serde::Deserialize)]
pub struct CommandBody {
    pub name: String,
    pub body: String,
}

pub async fn post_command(
    state: Arc<ServerState>,
    headers: HeaderMap,
    Path(backend): Path<String>,
    Json(body): Json<CommandBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_hermytt_key(&state, &headers).await?;
    if BackendKind::parse(&backend) != Some(BackendKind::Claude) {
        return Err(AppError::BadRequest(
            "commands only implemented for claude".into(),
        ));
    }
    customizations::write_command(&body.name, &body.body)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    info!(command = body.name, "custom command written");
    Ok(Json(serde_json::json!({"ok": true})))
}

pub async fn delete_command(
    state: Arc<ServerState>,
    headers: HeaderMap,
    Path((backend, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_hermytt_key(&state, &headers).await?;
    if BackendKind::parse(&backend) != Some(BackendKind::Claude) {
        return Err(AppError::BadRequest(
            "commands only implemented for claude".into(),
        ));
    }
    let removed = customizations::delete_command(&name)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !removed {
        return Err(AppError::BadRequest(format!("command not found: {name}")));
    }
    info!(command = name, "custom command deleted");
    Ok(Json(serde_json::json!({"ok": true})))
}

// ---------- Agents ----------

pub async fn get_agents(Path(name): Path<String>) -> Result<Json<serde_json::Value>, AppError> {
    if BackendKind::parse(&name) != Some(BackendKind::Claude) {
        return Err(AppError::BadRequest(
            "agents only implemented for claude".into(),
        ));
    }
    let agents = customizations::list_agents();
    Ok(Json(serde_json::json!({"agents": agents})))
}

/// Helper: gate write/delete endpoints on `X-Hermytt-Key` when `config_token` is set.
async fn require_hermytt_key(state: &Arc<ServerState>, headers: &HeaderMap) -> Result<(), AppError> {
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
    Ok(())
}

/// GET /backends/{name}/sessions/{sid}/messages — full conversation log.
/// Auth: same X-Hermytt-Key rule as PUT /config — message logs can contain
/// secrets the user typed, so they're never open even when listings are.
pub async fn get_backend_session_messages(
    state: Arc<ServerState>,
    headers: HeaderMap,
    Path((name, sid)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<MessagesQuery>,
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
    let mut log = sessions::read_messages(kind, &sid)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let total = log.messages.len();
    // `since`: if non-negative and <= total, slice [since..]. If out-of-range
    // (greater than total — file got truncated/edited externally), return
    // everything starting at index 0 so the client can detect the reset.
    if let Some(since) = q.since {
        if since > 0 && (since as usize) <= total {
            log.messages.drain(..since as usize);
        }
    }

    let mut out = serde_json::to_value(log).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("total".into(), serde_json::Value::from(total));
    }
    Ok(Json(out))
}

#[derive(Debug, serde::Deserialize)]
pub struct MessagesQuery {
    pub since: Option<i64>,
}

/// POST /backends/{name}/sessions/{sid}/cancel — abort any in-flight
/// /api/ask call(s) for this (backend, sid). Triggers `kill_on_drop` on the
/// underlying Child, SIGKILLing the subprocess.
/// Returns `{"killed": N}` (0 if no matching in-flight call).
pub async fn cancel_backend_session(
    state: Arc<ServerState>,
    Path((name, sid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let kind = BackendKind::parse(&name)
        .ok_or_else(|| AppError::BadRequest(format!("unknown backend: {name}")))?;
    let backend_str = kind.as_str().to_string();
    let mut killed = 0usize;
    let mut registry = state.in_flight.lock().await;
    let to_remove: Vec<u64> = registry
        .iter()
        .filter(|(_, e)| e.backend == backend_str && e.sid == sid)
        .map(|(t, _)| *t)
        .collect();
    for token in to_remove {
        if let Some(entry) = registry.remove(&token) {
            entry.abort.abort();
            killed += 1;
        }
    }
    info!(backend = name, session_id = sid, killed, "cancel by session");
    Ok(Json(serde_json::json!({ "killed": killed })))
}

/// DELETE /api/ask — kill switch. Aborts every in-flight /api/ask call.
/// Returns `{"killed": N}`.
pub async fn cancel_all_ask(
    state: Arc<ServerState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut registry = state.in_flight.lock().await;
    let entries: Vec<InFlight> = registry.drain().map(|(_, v)| v).collect();
    let killed = entries.len();
    for entry in entries {
        entry.abort.abort();
    }
    info!(killed, "kill switch — all in-flight /api/ask aborted");
    Ok(Json(serde_json::json!({ "killed": killed })))
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
