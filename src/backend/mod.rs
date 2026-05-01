pub mod claude;
pub mod copilot;
pub mod gemini;
pub mod ollama;

use serde::{Deserialize, Serialize};

use crate::persist::BackendConfig;
use crate::stream::StreamEvent;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Claude,
    Copilot,
    Gemini,
    Ollama,
}

impl BackendKind {
    pub const ALL: [BackendKind; 4] = [
        BackendKind::Claude,
        BackendKind::Copilot,
        BackendKind::Gemini,
        BackendKind::Ollama,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Claude => "claude",
            BackendKind::Copilot => "copilot",
            BackendKind::Gemini => "gemini",
            BackendKind::Ollama => "ollama",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "copilot" => Some(Self::Copilot),
            "gemini" => Some(Self::Gemini),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Unified request shape for all backends.
#[derive(Debug, Clone, Default)]
pub struct AskRequest {
    pub prompt: String,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Per-request working directory override. Together with `session_id`, this
    /// forms the full session key `(backend, dir, session_id)` — required because
    /// Claude/Copilot/Gemini scope sessions to the cwd they were started in.
    /// Ignored by Ollama (it's HTTP, no cwd semantics).
    pub dir: Option<String>,
}

/// Unified response shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub response: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub backend: String,
    pub error: Option<String>,
}

/// Dispatch a streaming request — returns a receiver of normalized events.
/// Each backend spawns a task that reads its native stream format and emits
/// `StreamEvent`s. The terminal `Done`/`Error` event always carries the
/// final session_id, cost, etc.
pub fn dispatch_stream(
    kind: BackendKind,
    cfg: BackendConfig,
    req: AskRequest,
) -> mpsc::Receiver<StreamEvent> {
    let (tx, rx) = mpsc::channel(128);
    let kind_str = kind.to_string();
    tokio::spawn(async move {
        let result = match kind {
            BackendKind::Claude => claude::ask_stream(&cfg, &req, &tx).await,
            BackendKind::Copilot => copilot::ask_stream(&cfg, &req, &tx).await,
            BackendKind::Gemini => gemini::ask_stream(&cfg, &req, &tx).await,
            BackendKind::Ollama => ollama::ask_stream(&cfg, &req, &tx).await,
        };
        match result {
            Ok(mut response) => {
                response.backend = kind_str;
                let _ = tx.send(StreamEvent::Done { response }).await;
            }
            Err(e) => {
                let _ = tx
                    .send(StreamEvent::Error {
                        error: format!("{kind_str}: {e}"),
                    })
                    .await;
            }
        }
    });
    rx
}

/// Dispatch a request to the appropriate backend using its stored config.
pub async fn dispatch(kind: BackendKind, cfg: &BackendConfig, req: &AskRequest) -> Response {
    let result = match kind {
        BackendKind::Claude => claude::ask(cfg, req).await,
        BackendKind::Copilot => copilot::ask(cfg, req).await,
        BackendKind::Gemini => gemini::ask(cfg, req).await,
        BackendKind::Ollama => ollama::ask(cfg, req).await,
    };

    match result {
        Ok(mut r) => {
            r.backend = kind.to_string();
            r
        }
        Err(e) => Response {
            response: String::new(),
            session_id: None,
            cost_usd: None,
            backend: kind.to_string(),
            error: Some(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_parse() {
        assert_eq!(BackendKind::parse("claude"), Some(BackendKind::Claude));
        assert_eq!(BackendKind::parse("COPILOT"), Some(BackendKind::Copilot));
        assert_eq!(BackendKind::parse("Gemini"), Some(BackendKind::Gemini));
        assert_eq!(BackendKind::parse("ollama"), Some(BackendKind::Ollama));
        assert_eq!(BackendKind::parse("bogus"), None);
    }

    #[test]
    fn backend_kind_display() {
        assert_eq!(BackendKind::Claude.to_string(), "claude");
        assert_eq!(BackendKind::Copilot.to_string(), "copilot");
        assert_eq!(BackendKind::Gemini.to_string(), "gemini");
        assert_eq!(BackendKind::Ollama.to_string(), "ollama");
    }

    #[test]
    fn backend_kind_all() {
        assert_eq!(BackendKind::ALL.len(), 4);
    }
}
