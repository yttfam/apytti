//! Hermytt registry announce + heartbeat.
//!
//! Cargo-culted from grytti/src/api.rs. Posts to `<hermytt>/registry/announce`
//! with our endpoint, role=`gateway`, and metadata. Repeats every 15s.

use std::time::Duration;

use serde_json::json;
use tracing::{info, warn};

use crate::persist::HermyttConfig;

const HEARTBEAT_SECS: u64 = 15;
const ROLE: &str = "gateway";

pub async fn announce(cfg: &HermyttConfig, endpoint: &str, version: &str) {
    let body = json!({
        "name": resolve_name(cfg),
        "role": ROLE,
        "endpoint": endpoint,
        "version": version,
        "host": hostname(),
    });

    let url = format!("{}/registry/announce", cfg.url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.post(&url);
    if let Some(token) = &cfg.token {
        req = req.header("X-Hermytt-Key", token);
    }
    match req.json(&body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                info!(%status, "announced to hermytt");
            } else {
                warn!(%status, "hermytt registry returned non-2xx");
            }
        }
        Err(e) => warn!("failed to announce to hermytt: {e}"),
    }
}

/// Background task: announce + heartbeat forever.
pub async fn heartbeat_loop(cfg: HermyttConfig, endpoint: String, version: String) {
    info!(
        url = cfg.url,
        endpoint = endpoint,
        "starting hermytt registry heartbeat (every {}s)",
        HEARTBEAT_SECS
    );
    let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    loop {
        interval.tick().await;
        announce(&cfg, &endpoint, &version).await;
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Compute the endpoint apytti advertises to hermytt.
/// Priority: explicit `cfg.endpoint` > `http://<hostname>:<port>`.
pub fn resolve_endpoint(cfg: &HermyttConfig, port: u16) -> String {
    if let Some(ep) = &cfg.endpoint {
        return ep.clone();
    }
    format!("http://{}:{}", hostname(), port)
}

/// Compute the service name announced to hermytt.
/// Priority: explicit `cfg.name` > `apytti-<hostname>`. Falls back to `apytti` if
/// hostname resolution somehow fails (only happens on broken hosts).
pub fn resolve_name(cfg: &HermyttConfig) -> String {
    if let Some(name) = &cfg.name {
        return name.clone();
    }
    let h = hostname();
    if h.is_empty() || h == "unknown" {
        "apytti".into()
    } else {
        format!("apytti-{h}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_explicit() {
        let cfg = HermyttConfig {
            url: "http://h:7777".into(),
            endpoint: Some("http://specific:1234".into()),
            ..Default::default()
        };
        assert_eq!(resolve_endpoint(&cfg, 7781), "http://specific:1234");
    }

    #[test]
    fn endpoint_default_uses_hostname() {
        let cfg = HermyttConfig {
            url: "http://h:7777".into(),
            ..Default::default()
        };
        let ep = resolve_endpoint(&cfg, 7781);
        assert!(ep.starts_with("http://"));
        assert!(ep.ends_with(":7781"));
    }

    #[test]
    fn name_explicit_wins() {
        let cfg = HermyttConfig {
            url: "http://h:7777".into(),
            name: Some("apytti-staging".into()),
            ..Default::default()
        };
        assert_eq!(resolve_name(&cfg), "apytti-staging");
    }

    #[test]
    fn name_default_uses_hostname() {
        let cfg = HermyttConfig {
            url: "http://h:7777".into(),
            ..Default::default()
        };
        let name = resolve_name(&cfg);
        assert!(name.starts_with("apytti-") || name == "apytti");
    }
}
