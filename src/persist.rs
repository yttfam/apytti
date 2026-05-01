use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::backend::BackendKind;

/// Apytti's persisted config at ~/.apytti/config.toml.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedConfig {
    /// Currently active backend, used when none specified per request.
    pub active: Option<BackendKind>,

    /// Per-backend defaults.
    #[serde(default)]
    pub backends: HashMap<String, BackendConfig>,

    /// Hermytt registry settings (optional). If absent, registry announce is skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hermytt: Option<HermyttConfig>,

    /// Security gates (optional). Currently scoped to `attachment_roots`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Whitelist of roots for `attachments[].path` on `/api/ask`. If empty or
    /// unset, no whitelist enforcement (only existence/regular-file checks).
    #[serde(default)]
    pub attachment_roots: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HermyttConfig {
    /// Hermytt registry URL (e.g. http://mista:7777)
    pub url: String,
    /// Bearer-style auth token (sent as X-Hermytt-Key header)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Endpoint apytti advertises to hermytt. Defaults to http://<hostname>:<port>.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Service name announced to hermytt. Defaults to `apytti-<hostname>` so
    /// multiple apytti instances on different hosts coexist in the registry
    /// without stomping each other.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Token required for PUT /config (write protection). If absent, /config writes are open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_token: Option<String>,
}

/// Per-backend configuration. All fields optional; unused keys are silently ignored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackendConfig {
    #[serde(default)]
    pub enabled: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub session_id: Option<String>,
    pub dir: Option<String>,
    #[serde(default)]
    pub skip_permissions: bool,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default = "default_resume")]
    pub resume: bool,

    // Ollama-specific
    pub endpoint: Option<String>,
}

fn default_resume() -> bool {
    true
}

impl PersistedConfig {
    /// Default path: ~/.apytti/config.toml
    pub fn default_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".apytti").join("config.toml"))
    }

    /// Load from given path, or return default empty config if missing.
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(path)?;
        let cfg = toml::from_str(&s)?;
        Ok(cfg)
    }

    /// Save to given path, creating parent dir if needed.
    pub fn save(&self, path: &PathBuf) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }

    /// Get the config for a backend, or default empty.
    pub fn backend(&self, kind: BackendKind) -> BackendConfig {
        self.backends
            .get(kind.as_str())
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_backend(&mut self, kind: BackendKind, cfg: BackendConfig) {
        self.backends.insert(kind.as_str().to_string(), cfg);
    }

    /// Merge another config into this one. Used for partial PUT updates from hermytt.
    /// `other.active` overrides if set. Each backend from `other.backends` replaces this side's
    /// entry wholesale (not field-by-field). `hermytt` is replaced if `other.hermytt` is set.
    pub fn merge(&mut self, other: PersistedConfig) {
        if other.active.is_some() {
            self.active = other.active;
        }
        for (k, v) in other.backends {
            self.backends.insert(k, v);
        }
        if other.hermytt.is_some() {
            self.hermytt = other.hermytt;
        }
        if other.security.is_some() {
            self.security = other.security;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty() {
        let cfg = PersistedConfig::default();
        let s = toml::to_string(&cfg).unwrap();
        let parsed: PersistedConfig = toml::from_str(&s).unwrap();
        assert!(parsed.active.is_none());
        assert!(parsed.backends.is_empty());
    }

    #[test]
    fn roundtrip_full() {
        let mut cfg = PersistedConfig {
            active: Some(BackendKind::Claude),
            backends: HashMap::new(),
            hermytt: None,
            security: None,
        };
        cfg.set_backend(
            BackendKind::Claude,
            BackendConfig {
                enabled: true,
                model: Some("sonnet".into()),
                effort: Some("low".into()),
                resume: true,
                skip_permissions: true,
                allow: vec!["Bash(*)".into()],
                ..Default::default()
            },
        );
        cfg.set_backend(
            BackendKind::Ollama,
            BackendConfig {
                enabled: true,
                model: Some("llama3.2".into()),
                endpoint: Some("http://localhost:11434".into()),
                resume: true,
                ..Default::default()
            },
        );

        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: PersistedConfig = toml::from_str(&s).unwrap();

        assert_eq!(parsed.active, Some(BackendKind::Claude));
        let claude = parsed.backend(BackendKind::Claude);
        assert!(claude.enabled);
        assert_eq!(claude.model.as_deref(), Some("sonnet"));
        let ollama = parsed.backend(BackendKind::Ollama);
        assert_eq!(ollama.endpoint.as_deref(), Some("http://localhost:11434"));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apytti").join("config.toml");

        let mut cfg = PersistedConfig::default();
        cfg.active = Some(BackendKind::Copilot);
        cfg.set_backend(
            BackendKind::Copilot,
            BackendConfig {
                enabled: true,
                model: Some("claude-sonnet-4.6".into()),
                ..Default::default()
            },
        );

        cfg.save(&path).unwrap();

        let loaded = PersistedConfig::load(&path).unwrap();
        assert_eq!(loaded.active, Some(BackendKind::Copilot));
        assert_eq!(
            loaded.backend(BackendKind::Copilot).model.as_deref(),
            Some("claude-sonnet-4.6")
        );
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let cfg = PersistedConfig::load(&path).unwrap();
        assert!(cfg.active.is_none());
        assert!(cfg.backends.is_empty());
    }

    #[test]
    fn missing_backend_returns_default() {
        let cfg = PersistedConfig::default();
        let claude = cfg.backend(BackendKind::Claude);
        assert!(!claude.enabled);
        assert!(claude.model.is_none());
    }
}
