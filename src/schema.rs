//! Static schema describing each backend's configurable fields.
//! Lets hermytt's UI render forms without hardcoding apytti's CLI knowledge.

use serde_json::{json, Value};

pub fn backends_schema() -> Value {
    json!({
        "claude": {
            "fields": [
                {"name": "enabled",          "type": "bool"},
                {"name": "model",            "type": "string", "hint": "sonnet, opus, haiku, or full model ID"},
                {"name": "effort",           "type": "enum",   "options": ["low", "medium", "high", "max"]},
                {"name": "dir",              "type": "path"},
                {"name": "skip_permissions", "type": "bool"},
                {"name": "allow",            "type": "string[]", "hint": "e.g. Bash(git:*), Read(*)"},
                {"name": "resume",           "type": "bool",   "default": true},
                {"name": "session_id",       "type": "string", "hint": "default session UUID to resume"}
            ],
            "supports_effort": true,
            "supports_cost": true,
            "supports_streaming": true
        },
        "copilot": {
            "fields": [
                {"name": "enabled",          "type": "bool"},
                {"name": "model",            "type": "string", "hint": "claude-sonnet-4.6, gpt-5, etc."},
                {"name": "effort",           "type": "enum",   "options": ["low", "medium", "high", "xhigh"]},
                {"name": "dir",              "type": "path"},
                {"name": "skip_permissions", "type": "bool",   "hint": "maps to --allow-all"},
                {"name": "allow",            "type": "string[]", "hint": "tool names, e.g. shell(git)"},
                {"name": "resume",           "type": "bool",   "default": true},
                {"name": "session_id",       "type": "string"}
            ],
            "supports_effort": true,
            "supports_cost": false,
            "supports_streaming": true
        },
        "gemini": {
            "fields": [
                {"name": "enabled",          "type": "bool"},
                {"name": "model",            "type": "string", "hint": "gemini-3-flash-preview, etc."},
                {"name": "dir",              "type": "path"},
                {"name": "skip_permissions", "type": "bool",   "hint": "maps to --yolo"},
                {"name": "resume",           "type": "bool",   "default": true},
                {"name": "session_id",       "type": "string", "hint": "session id, 'latest', or index"}
            ],
            "supports_effort": false,
            "supports_cost": false,
            "supports_streaming": true
        },
        "ollama": {
            "fields": [
                {"name": "enabled",  "type": "bool"},
                {"name": "model",    "type": "string", "hint": "llama3.2, mistral:7b, etc."},
                {"name": "endpoint", "type": "url",    "default": "http://localhost:11434"},
                {"name": "session_id", "type": "string", "hint": "in-memory session id (lost on restart)"}
            ],
            "supports_effort": false,
            "supports_cost": false,
            "supports_streaming": true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_all_four_backends() {
        let s = backends_schema();
        for k in ["claude", "copilot", "gemini", "ollama"] {
            assert!(s.get(k).is_some(), "missing backend: {k}");
            assert!(s[k]["fields"].is_array());
            assert!(!s[k]["fields"].as_array().unwrap().is_empty());
        }
    }

    #[test]
    fn schema_serializes_to_json() {
        let s = backends_schema();
        let serialized = serde_json::to_string(&s).unwrap();
        assert!(serialized.contains("\"claude\""));
        assert!(serialized.contains("\"supports_effort\""));
    }
}
