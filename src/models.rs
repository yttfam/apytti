//! Model discovery cache.
//!
//! Asks each enabled backend "what models can you use?" once, then caches
//! to `~/.apytti/models.json`. Cache is keyed per-backend (per-machine).
//! Re-probe is explicit (POST /models/init or `apytti init-models`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::backend::{self, AskRequest, BackendKind};
use crate::persist::{BackendConfig, PersistedConfig};

const PROBE_PROMPT: &str =
    "List the model IDs you can be invoked with, one per line. No commentary, no formatting, no markdown.";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelsCache {
    #[serde(flatten)]
    pub backends: HashMap<String, BackendModels>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendModels {
    pub models: Vec<String>,
    pub fetched_at: String,
    /// "live" (Ollama HTTP), "probe" (LLM-asked), or "error"
    pub via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ModelsCache {
    /// Cache file lives next to the config file.
    pub fn path_for(config_path: &PathBuf) -> PathBuf {
        config_path
            .parent()
            .map(|p| p.join("models.json"))
            .unwrap_or_else(|| PathBuf::from("models.json"))
    }

    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = std::fs::read_to_string(path)?;
        let cache = serde_json::from_str(&s)?;
        Ok(cache)
    }

    pub fn save(&self, path: &PathBuf) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = serde_json::to_string_pretty(self)?;
        // Atomic write: tmp + rename. Avoids torn reads when probes write concurrently.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn get(&self, kind: BackendKind) -> Option<&BackendModels> {
        self.backends.get(kind.as_str())
    }

    pub fn set(&mut self, kind: BackendKind, models: BackendModels) {
        self.backends.insert(kind.as_str().to_string(), models);
    }
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    epoch_to_iso(secs)
}

/// Convert unix epoch seconds to a minimal RFC3339 timestamp (no chrono dep).
pub fn epoch_to_iso(secs: u64) -> String {
    let t = time_split(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.0, t.1, t.2, t.3, t.4, t.5
    )
}

/// Convert unix epoch seconds to (year, month, day, hour, min, sec).
fn time_split(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    let days = secs / 86400;
    let (y, mo, d) = days_to_ymd(days);
    (y, mo, d, h as u32, m as u32, s as u32)
}

fn days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    let mut year = 1970u32;
    loop {
        let leap = is_leap(year);
        let yd = if leap { 366 } else { 365 };
        if days < yd {
            break;
        }
        days -= yd;
        year += 1;
    }
    let months = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u32;
    for &m in &months {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    (year, month, days as u32 + 1)
}

fn is_leap(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Strip noise from an LLM model-list response. Keeps short tokens that look like model IDs.
pub fn parse_probe_response(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '-' || c == '*' || c == '`' || c == '#'))
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter(|l| !l.contains(' ') || l.len() < 80)
        .map(|l| {
            // Drop trailing punctuation/parens like "sonnet (default)"
            l.split_whitespace().next().unwrap_or(l).to_string()
        })
        .filter(|l| {
            // Heuristic: model IDs are alnum + _ . : - / @
            !l.is_empty()
                && l.chars()
                    .all(|c| c.is_alphanumeric() || ".:_-/@".contains(c))
        })
        .collect()
}

/// Probe a single backend. Returns either a populated BackendModels or one with `via=error`.
pub async fn probe_backend(kind: BackendKind, cfg: &BackendConfig) -> BackendModels {
    let result = match kind {
        BackendKind::Ollama => probe_ollama(cfg).await,
        _ => probe_subprocess(kind, cfg).await,
    };

    match result {
        Ok((models, via)) => BackendModels {
            models,
            fetched_at: now_iso(),
            via,
            error: None,
        },
        Err(e) => BackendModels {
            models: vec![],
            fetched_at: now_iso(),
            via: "error".into(),
            error: Some(e.to_string()),
        },
    }
}

async fn probe_ollama(cfg: &BackendConfig) -> anyhow::Result<(Vec<String>, String)> {
    let endpoint = cfg
        .endpoint
        .clone()
        .unwrap_or_else(|| "http://localhost:11434".into());
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));

    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("ollama /api/tags failed: {e}"))?;

    if !resp.status().is_success() {
        anyhow::bail!("ollama /api/tags returned {}", resp.status());
    }

    let body: serde_json::Value = resp.json().await?;
    let models = body["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok((models, "live".into()))
}

async fn probe_subprocess(
    kind: BackendKind,
    cfg: &BackendConfig,
) -> anyhow::Result<(Vec<String>, String)> {
    // Force skip_permissions for the probe — we're only generating text, no tools needed.
    // Drop session_id so we get a fresh, fast turn.
    let probe_cfg = BackendConfig {
        skip_permissions: true,
        session_id: None,
        ..cfg.clone()
    };
    let req = AskRequest {
        prompt: PROBE_PROMPT.into(),
        ..Default::default()
    };

    let resp = backend::dispatch(kind, &probe_cfg, &req).await;
    if let Some(err) = resp.error {
        anyhow::bail!("{kind} probe failed: {err}");
    }
    let models = parse_probe_response(&resp.response);
    if models.is_empty() {
        anyhow::bail!("{kind} probe returned no parseable model IDs (raw: {})", resp.response);
    }
    Ok((models, "probe".into()))
}

/// Build a placeholder entry for a backend that's currently being probed.
/// Hermytt's UI distinguishes `via=probing` from `via=missing` to render a spinner.
fn probing_entry() -> BackendModels {
    BackendModels {
        models: vec![],
        fetched_at: now_iso(),
        via: "probing".into(),
        error: None,
    }
}

/// Probe every enabled backend in parallel. Each probe writes its result to disk
/// as soon as it completes, so `GET /models` can surface progress to clients
/// polling during a long probe (e.g. Gemini's ~9-min CLI loop).
///
/// Initial state on disk: every enabled backend has `via=probing` and empty models.
pub async fn init_all(config: &PersistedConfig, cache_path: &PathBuf) -> ModelsCache {
    // Seed cache with "probing" placeholders for every enabled backend.
    let mut initial = ModelsCache::default();
    let enabled: Vec<BackendKind> = BackendKind::ALL
        .iter()
        .copied()
        .filter(|k| config.backend(*k).enabled)
        .collect();
    for kind in &enabled {
        initial.set(*kind, probing_entry());
    }
    let cache = Arc::new(Mutex::new(initial));
    if let Err(e) = cache.lock().await.save(cache_path) {
        tracing::warn!("failed to persist initial probing cache: {e}");
    }

    // Spawn one task per backend; each persists its slice on completion.
    let mut handles = Vec::new();
    for kind in enabled {
        let cfg = config.backend(kind);
        let cache = cache.clone();
        let path = cache_path.clone();
        handles.push(tokio::spawn(async move {
            let entry = probe_backend(kind, &cfg).await;
            let mut c = cache.lock().await;
            c.set(kind, entry);
            if let Err(e) = c.save(&path) {
                tracing::warn!("failed to persist probe result for {kind}: {e}");
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    cache.lock().await.clone()
}

/// Probe a single backend by kind. Marks it as `via=probing` on disk before starting,
/// then writes the real result on completion. Loads existing cache so other backends
/// aren't lost.
pub async fn init_one(
    kind: BackendKind,
    config: &PersistedConfig,
    cache_path: &PathBuf,
) -> BackendModels {
    // Mark this backend as probing so polling clients see it immediately.
    {
        let mut cache = ModelsCache::load(cache_path).unwrap_or_default();
        cache.set(kind, probing_entry());
        if let Err(e) = cache.save(cache_path) {
            tracing::warn!("failed to persist probing placeholder for {kind}: {e}");
        }
    }

    let cfg = config.backend(kind);
    let entry = probe_backend(kind, &cfg).await;
    let mut cache = ModelsCache::load(cache_path).unwrap_or_default();
    cache.set(kind, entry.clone());
    if let Err(e) = cache.save(cache_path) {
        tracing::warn!("failed to persist models cache: {e}");
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strips_markdown() {
        let raw = "- sonnet\n- opus\n* haiku\n  claude-opus-4-7\n";
        assert_eq!(
            parse_probe_response(raw),
            vec!["sonnet", "opus", "haiku", "claude-opus-4-7"]
        );
    }

    #[test]
    fn parse_drops_commentary() {
        let raw = "Here are the available models you can use:\n\nsonnet\nopus\nhaiku\n\nThat's the list of currently supported model identifiers for the Claude CLI tool.";
        let parsed = parse_probe_response(raw);
        assert!(parsed.contains(&"sonnet".to_string()));
        assert!(parsed.contains(&"opus".to_string()));
        assert!(parsed.contains(&"haiku".to_string()));
    }

    #[test]
    fn parse_keeps_dashed_ids() {
        let raw = "claude-sonnet-4-6\nclaude-opus-4-7\n";
        let parsed = parse_probe_response(raw);
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains(&"claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn parse_handles_colons_for_ollama_style() {
        let raw = "mistral:7b\nllama3.2\ndeepseek-coder-v2:16b";
        let parsed = parse_probe_response(raw);
        assert_eq!(parsed.len(), 3);
        assert!(parsed.contains(&"mistral:7b".to_string()));
    }

    #[test]
    fn parse_drops_trailing_parens() {
        let raw = "sonnet (default)\nopus (premium)\nhaiku (cheap)";
        let parsed = parse_probe_response(raw);
        assert_eq!(parsed, vec!["sonnet", "opus", "haiku"]);
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");

        let mut cache = ModelsCache::default();
        cache.set(
            BackendKind::Claude,
            BackendModels {
                models: vec!["sonnet".into(), "opus".into()],
                fetched_at: "2026-04-20T12:00:00Z".into(),
                via: "probe".into(),
                error: None,
            },
        );
        cache.save(&path).unwrap();

        let loaded = ModelsCache::load(&path).unwrap();
        let claude = loaded.get(BackendKind::Claude).unwrap();
        assert_eq!(claude.models, vec!["sonnet", "opus"]);
        assert_eq!(claude.via, "probe");
    }

    #[test]
    fn cache_path_relative_to_config() {
        let p = PathBuf::from("/home/x/.apytti/config.toml");
        assert_eq!(ModelsCache::path_for(&p), PathBuf::from("/home/x/.apytti/models.json"));
    }

    #[test]
    fn now_iso_format() {
        let s = now_iso();
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
        assert!(s.contains('T'));
    }

    #[test]
    fn iso_known_date() {
        let (y, m, d, h, mi, s) = time_split(1745161200); // 2025-04-20T15:00:00Z
        assert_eq!((y, m, d, h, mi, s), (2025, 4, 20, 15, 0, 0));
    }
}
