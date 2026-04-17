use std::path::PathBuf;

use clap::{Parser, Subcommand};

const LONG_ABOUT: &str = "\
Multi-backend REST gateway for AI CLIs (Claude, Copilot, Gemini, Ollama).

ENDPOINTS:

  POST /api/ask
    Request:
      {
        \"prompt\":     \"your question\",   (required)
        \"backend\":    \"claude\",          (optional: claude|copilot|gemini|ollama)
        \"session_id\": \"uuid\",            (optional, resumes session)
        \"model\":      \"sonnet\",          (optional, overrides default)
        \"effort\":     \"low\"              (optional, overrides default)
      }
    Response:
      {
        \"response\":   \"...\",
        \"session_id\": \"uuid\",
        \"cost_usd\":   0.05,
        \"backend\":    \"claude\",
        \"error\":      null
      }

  GET /health     {\"status\": \"ok\", ...}
  GET /help       HTML API documentation

SUBCOMMANDS:
  apytti run              Start the HTTP server (default)
  apytti setup            Interactive backend configuration menu
  apytti install          Install as OS daemon (launchd/systemd/sc)
  apytti uninstall        Remove the daemon

CONFIG: ~/.apytti/config.toml";

#[derive(Parser, Debug, Clone)]
#[command(
    name = "apytti",
    version,
    about = "Multi-backend REST gateway for AI CLIs",
    long_about = LONG_ABOUT
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to config file [default: ~/.apytti/config.toml]
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Start the HTTP server (default if no subcommand given)
    Run(RunArgs),
    /// Interactive backend configuration menu
    Setup,
    /// Install as a system daemon
    Install(InstallArgs),
    /// Remove the system daemon
    Uninstall,
}

#[derive(Parser, Debug, Clone, Default)]
pub struct RunArgs {
    /// Listen port
    #[arg(long, default_value = "7781")]
    pub port: u16,

    /// Bind address [default: 0.0.0.0]
    #[arg(long)]
    pub host: Option<String>,

    /// Bind to 127.0.0.1 only (shorthand for --host 127.0.0.1)
    #[arg(long)]
    pub localhost: bool,

    /// Enable verbose logging (requests, responses, timing)
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Parser, Debug, Clone, Default)]
pub struct InstallArgs {
    /// Listen port the daemon will use
    #[arg(long, default_value = "7781")]
    pub port: u16,

    /// Bind address for the daemon [default: 127.0.0.1]
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
}

impl RunArgs {
    pub fn bind_addr(&self) -> &str {
        if self.localhost {
            return "127.0.0.1";
        }
        self.host.as_deref().unwrap_or("0.0.0.0")
    }
}

/// Backwards-compat alias for the binary's main args.
pub type Config = Cli;

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> RunArgs {
        RunArgs::default()
    }

    #[test]
    fn bind_addr_default() {
        assert_eq!(args().bind_addr(), "0.0.0.0");
    }

    #[test]
    fn bind_addr_localhost_flag() {
        let a = RunArgs {
            localhost: true,
            ..args()
        };
        assert_eq!(a.bind_addr(), "127.0.0.1");
    }

    #[test]
    fn bind_addr_custom_host() {
        let a = RunArgs {
            host: Some("10.0.0.5".into()),
            ..args()
        };
        assert_eq!(a.bind_addr(), "10.0.0.5");
    }

    #[test]
    fn bind_addr_localhost_overrides_host() {
        let a = RunArgs {
            host: Some("10.0.0.5".into()),
            localhost: true,
            ..args()
        };
        assert_eq!(a.bind_addr(), "127.0.0.1");
    }
}
