use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};

use super::{AskRequest, Response};
use crate::persist::BackendConfig;
use crate::stream::StreamEvent;

const DEFAULT_ENDPOINT: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "llama3.2";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: Option<Message>,
}

/// In-memory session store: session_id -> message history.
/// Lost on restart (KISS for v1).
fn sessions() -> &'static Mutex<HashMap<String, Vec<Message>>> {
    static STORE: OnceLock<Mutex<HashMap<String, Vec<Message>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn ask(cfg: &BackendConfig, req: &AskRequest) -> anyhow::Result<Response> {
    let endpoint = cfg.endpoint.as_deref().unwrap_or(DEFAULT_ENDPOINT);
    let model = req
        .model
        .as_deref()
        .or(cfg.model.as_deref())
        .unwrap_or(DEFAULT_MODEL);

    // Resolve session: caller-provided id, or config default, or generate new
    let session_id = req
        .session_id
        .clone()
        .or_else(|| cfg.session_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let store = sessions();
    let mut history = {
        let s = store.lock().await;
        s.get(&session_id).cloned().unwrap_or_default()
    };

    history.push(Message {
        role: "user".into(),
        content: req.prompt.clone(),
    });

    let url = format!("{}/api/chat", endpoint.trim_end_matches('/'));
    let body = ChatRequest {
        model,
        messages: &history,
        stream: false,
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            let mut msg = format!("ollama request failed: {e} (endpoint: {url})");
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                msg.push_str(&format!("\n  caused by: {s}"));
                src = s.source();
            }
            anyhow::anyhow!(msg)
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Ok(Response {
            response: String::new(),
            session_id: Some(session_id),
            cost_usd: None,
            backend: String::new(),
            error: Some(format!("ollama returned {status}: {text}")),
        });
    }

    let chat: ChatResponse = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("failed to parse ollama response: {e}"))?;

    let assistant = chat
        .message
        .ok_or_else(|| anyhow::anyhow!("ollama response missing message field"))?;

    history.push(assistant.clone());

    {
        let mut s = store.lock().await;
        s.insert(session_id.clone(), history);
    }

    Ok(Response {
        response: assistant.content,
        session_id: Some(session_id),
        cost_usd: None,
        backend: String::new(),
        error: None,
    })
}

/// Streaming variant — POST /api/chat with stream=true returns NDJSON of:
///   `{"message":{"role":"assistant","content":"H"},"done":false}`
///   `{"message":{"role":"assistant","content":"i"},"done":false}`
///   `{"message":{"role":"assistant","content":""},"done":true,...}`
///
/// Each chunk's content is the delta. We accumulate for session history.
pub async fn ask_stream(
    cfg: &BackendConfig,
    req: &AskRequest,
    tx: &mpsc::Sender<StreamEvent>,
) -> anyhow::Result<Response> {
    use futures_util::StreamExt;

    let endpoint = cfg.endpoint.as_deref().unwrap_or(DEFAULT_ENDPOINT);
    let model = req
        .model
        .as_deref()
        .or(cfg.model.as_deref())
        .unwrap_or(DEFAULT_MODEL);

    let session_id = req
        .session_id
        .clone()
        .or_else(|| cfg.session_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let store = sessions();
    let mut history = {
        let s = store.lock().await;
        s.get(&session_id).cloned().unwrap_or_default()
    };

    history.push(Message {
        role: "user".into(),
        content: req.prompt.clone(),
    });

    let url = format!("{}/api/chat", endpoint.trim_end_matches('/'));
    let body = ChatRequest {
        model,
        messages: &history,
        stream: true,
    };

    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("ollama request failed: {e} (endpoint: {url})"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Ok(Response {
            response: String::new(),
            session_id: Some(session_id),
            cost_usd: None,
            backend: String::new(),
            error: Some(format!("ollama returned {status}: {text}")),
        });
    }

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut full_text = String::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // NDJSON: split on newlines, keep partial trailing fragment in buffer
        while let Some(idx) = buffer.find('\n') {
            let line = buffer[..idx].trim().to_string();
            buffer.drain(..=idx);
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    full_text.push_str(content);
                    if tx
                        .send(StreamEvent::Delta {
                            text: content.to_string(),
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    }

    history.push(Message {
        role: "assistant".into(),
        content: full_text.clone(),
    });
    {
        let mut s = store.lock().await;
        s.insert(session_id.clone(), history);
    }

    Ok(Response {
        response: full_text,
        session_id: Some(session_id),
        cost_usd: None,
        backend: String::new(),
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_serializes() {
        let m = Message {
            role: "user".into(),
            content: "hello".into(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"hello\""));
    }

    #[test]
    fn chat_response_parses() {
        let json = r#"{"model":"llama3.2","message":{"role":"assistant","content":"hi"},"done":true}"#;
        let r: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.message.unwrap().content, "hi");
    }

    #[tokio::test]
    async fn session_store_isolates_sessions() {
        let store = sessions();
        {
            let mut s = store.lock().await;
            s.clear();
            s.insert(
                "a".into(),
                vec![Message {
                    role: "user".into(),
                    content: "hi from a".into(),
                }],
            );
            s.insert(
                "b".into(),
                vec![Message {
                    role: "user".into(),
                    content: "hi from b".into(),
                }],
            );
        }
        let s = store.lock().await;
        assert_eq!(s.get("a").unwrap()[0].content, "hi from a");
        assert_eq!(s.get("b").unwrap()[0].content, "hi from b");
    }
}
