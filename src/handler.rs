use std::sync::Arc;

use axum::Json;
use tracing::{debug, info};

use crate::backend::{dispatch, AskRequest, BackendKind, Response};
use crate::error::AppError;
use crate::persist::PersistedConfig;

#[derive(Debug, Clone)]
pub struct ServerState {
    pub config: PersistedConfig,
}

#[derive(Debug, serde::Deserialize)]
pub struct AskRequestBody {
    pub prompt: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub backend: Option<String>,
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
) -> Result<Json<Response>, AppError> {
    if body.prompt.is_empty() {
        return Err(AppError::BadRequest("prompt is required".into()));
    }

    let kind = resolve_backend(&state.config, body.backend.as_deref())?;
    let cfg = state.config.backend(kind);
    if !cfg.enabled {
        return Err(AppError::BadRequest(format!(
            "backend {kind} is not enabled. Run `apytti setup` to configure it.",
        )));
    }

    let prompt_preview: String = body.prompt.chars().take(100).collect();
    info!(
        backend = kind.as_str(),
        session_id = body.session_id.as_deref().unwrap_or("-"),
        model = body.model.as_deref().unwrap_or("-"),
        effort = body.effort.as_deref().unwrap_or("-"),
        "ask: {prompt_preview}{}",
        if body.prompt.len() > 100 { "..." } else { "" }
    );

    let req = AskRequest {
        prompt: body.prompt,
        session_id: body.session_id,
        model: body.model,
        effort: body.effort,
    };

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

    Ok(Json(resp))
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
    let active_backend = state.config.active.map(|k| k.to_string());
    let enabled_backends: Vec<String> = BackendKind::ALL
        .iter()
        .filter(|k| state.config.backend(**k).enabled)
        .map(|k| k.to_string())
        .collect();

    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        active_backend,
        enabled_backends,
    })
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
