//! Normalized streaming events emitted by every backend.
//!
//! Each backend reads its own native stream format (Claude/Gemini stream-json,
//! Copilot JSONL, Ollama HTTP NDJSON) and emits a uniform sequence of
//! `StreamEvent`s into an mpsc channel. The HTTP handler turns these into SSE.

use serde::Serialize;

use crate::backend::Response;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Incremental text chunk. Concatenate to build the full response.
    Delta { text: String },
    /// The model started a tool call. `input_summary` is a one-line preview
    /// (matches the shape returned by GET /backends/{name}/sessions/{sid}/messages).
    ToolUse {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_summary: Option<String>,
    },
    /// The tool call returned. No body — just the "now done" signal so a
    /// progressive UI can clear the spinner / advance.
    ToolResult { name: String },
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
            Self::ToolUse { .. } => "tool_use",
            Self::ToolResult { .. } => "tool_result",
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
        assert_eq!(
            StreamEvent::ToolUse { name: "Bash".into(), input_summary: None }.sse_event(),
            "tool_use",
        );
        assert_eq!(
            StreamEvent::ToolResult { name: "Bash".into() }.sse_event(),
            "tool_result",
        );
    }

    #[test]
    fn tool_use_serializes() {
        let e = StreamEvent::ToolUse {
            name: "Bash".into(),
            input_summary: Some("git status".into()),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"tool_use\""));
        assert!(s.contains("\"name\":\"Bash\""));
        assert!(s.contains("\"input_summary\":\"git status\""));
    }

    #[test]
    fn tool_use_omits_summary_when_none() {
        let e = StreamEvent::ToolUse { name: "Bash".into(), input_summary: None };
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains("input_summary"));
    }

    #[test]
    fn tool_result_serializes() {
        let e = StreamEvent::ToolResult { name: "Bash".into() };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"tool_result\""));
        assert!(s.contains("\"name\":\"Bash\""));
    }
}
