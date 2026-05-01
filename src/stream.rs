//! Normalized streaming events emitted by every backend.
//!
//! Each backend reads its own native stream format (Claude/Gemini stream-json,
//! Copilot JSONL, Ollama HTTP NDJSON) and emits a uniform sequence of
//! `StreamEvent`s into an mpsc channel. The HTTP handler turns these into SSE.

use serde::Serialize;

use crate::backend::Response;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum StreamEvent {
    /// Incremental text chunk. Concatenate to build the full response.
    Delta { text: String },
    /// Terminal event — request completed.
    Done {
        #[serde(flatten)]
        response: Response,
    },
    /// Terminal event — fatal stream error.
    Error { error: String },
}

impl StreamEvent {
    /// SSE event name for this variant.
    pub fn sse_event(&self) -> &'static str {
        match self {
            Self::Delta { .. } => "delta",
            Self::Done { .. } => "done",
            Self::Error { .. } => "error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_serializes() {
        let e = StreamEvent::Delta { text: "hi".into() };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"delta\""));
        assert!(s.contains("\"text\":\"hi\""));
    }

    #[test]
    fn done_serializes_response_fields() {
        let e = StreamEvent::Done {
            response: Response {
                response: "hello world".into(),
                session_id: Some("abc".into()),
                cost_usd: Some(0.05),
                backend: "claude".into(),
                error: None,
            },
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"done\""));
        assert!(s.contains("\"session_id\":\"abc\""));
        assert!(s.contains("\"backend\":\"claude\""));
    }

    #[test]
    fn error_serializes() {
        let e = StreamEvent::Error {
            error: "boom".into(),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"error\""));
        assert!(s.contains("\"error\":\"boom\""));
    }

    #[test]
    fn sse_event_names() {
        assert_eq!(
            StreamEvent::Delta { text: "x".into() }.sse_event(),
            "delta"
        );
        assert_eq!(StreamEvent::Error { error: "x".into() }.sse_event(), "error");
    }
}
