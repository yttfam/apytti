//! MCP servers, custom commands, and agents — three small adjacent surfaces
//! that hermytt's UI wants to expose.
//!
//! Storage layout (claude convention):
//!   - MCP servers: `~/.claude.json` under `mcpServers` key, managed via `claude mcp` CLI
//!   - Custom commands: `~/.claude/commands/<name>.md` (markdown templates with $ARGUMENTS)
//!   - User agents: `~/.claude/agents/<name>.md`
//!   - Plugin commands/agents: `~/.claude/plugins/.../{commands,agents}/<name>.md`

use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;

const ARGUMENTS_PLACEHOLDER: &str = "$ARGUMENTS";

#[derive(Debug, Clone, Serialize)]
pub struct McpServer {
    pub name: String,
    /// "http", "sse", "stdio"
    pub transport: String,
    /// URL for http/sse, command for stdio
    pub target: String,
    /// User config | Project config (from `claude mcp get` output)
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomCommand {
    pub name: String,
    /// "user" (~/.claude/commands/) or "plugin:<plugin-name>"
    pub scope: String,
    pub path: String,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    pub name: String,
    /// "user" or "plugin:<plugin-name>"
    pub scope: String,
    pub path: String,
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn user_commands_dir() -> Option<PathBuf> {
    home().map(|h| h.join(".claude/commands"))
}

fn user_agents_dir() -> Option<PathBuf> {
    home().map(|h| h.join(".claude/agents"))
}

fn plugins_dir() -> Option<PathBuf> {
    home().map(|h| h.join(".claude/plugins/cache"))
}

// ---------- MCP servers ----------

/// Shell out to `claude mcp list` and parse the output.
pub fn list_mcp_servers() -> Vec<McpServer> {
    let Ok(output) = Command::new("claude").arg("mcp").arg("list").output() else {
        return vec![];
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Checking") {
            continue;
        }
        // Format: "<name>: <url-or-command> (HTTP|SSE) - <status>"
        // or:     "<name>: <url> - ! <error>"
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let rest = rest.trim();
        if name.is_empty() {
            continue;
        }
        // Strip the trailing " - <status>" / " - ✓ Connected" / etc.
        let rest = rest.split(" - ").next().unwrap_or(rest).trim();
        // Detect transport from optional "(HTTP)" / "(SSE)" suffix
        let (target, transport) = if let Some(idx) = rest.rfind('(') {
            let target = rest[..idx].trim().to_string();
            let inside = rest[idx + 1..].trim_end_matches(')').to_lowercase();
            (target, inside)
        } else {
            // No parens — likely stdio command or http URL with no annotation
            let transport = if rest.starts_with("http://") || rest.starts_with("https://") {
                "http".into()
            } else {
                "stdio".into()
            };
            (rest.to_string(), transport)
        };
        out.push(McpServer {
            name: name.to_string(),
            transport,
            target,
            scope: None,
        });
    }
    out
}

/// `claude mcp add <name> [args] <commandOrUrl>` — wraps the canonical CLI.
/// `transport` is one of "http", "sse", "stdio". For stdio, `target` is the
/// command and `args` is its arguments. For http/sse, `target` is the URL.
/// Returns the MCP server entry on success.
pub fn add_mcp_server(
    name: &str,
    transport: &str,
    target: &str,
    args: &[String],
    headers: &[String],
    scope: Option<&str>,
) -> anyhow::Result<()> {
    let mut cmd = Command::new("claude");
    cmd.arg("mcp").arg("add");
    cmd.arg("--transport").arg(transport);
    if let Some(s) = scope {
        cmd.arg("--scope").arg(s);
    }
    for h in headers {
        cmd.arg("--header").arg(h);
    }
    cmd.arg(name).arg(target);
    for a in args {
        cmd.arg(a);
    }

    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "claude mcp add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub fn remove_mcp_server(name: &str, scope: Option<&str>) -> anyhow::Result<()> {
    let mut cmd = Command::new("claude");
    cmd.arg("mcp").arg("remove");
    if let Some(s) = scope {
        cmd.arg("--scope").arg(s);
    }
    cmd.arg(name);
    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "claude mcp remove failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

// ---------- Custom commands ----------

pub fn list_commands() -> Vec<CustomCommand> {
    let mut out = Vec::new();

    if let Some(dir) = user_commands_dir() {
        scan_md_dir(&dir, "user", &mut out, false);
    }

    walk_plugin_subdirs("commands", |scope, dir| scan_md_dir(dir, scope, &mut out, false));

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn read_command(name: &str) -> anyhow::Result<CustomCommand> {
    let path = user_commands_dir()
        .map(|d| d.join(format!("{name}.md")))
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    if !path.exists() {
        // Fallback to plugin commands
        for cmd in list_commands() {
            if cmd.name == name {
                let body = std::fs::read_to_string(&cmd.path).ok();
                return Ok(CustomCommand { body, ..cmd });
            }
        }
        anyhow::bail!("command not found: {name}");
    }
    let body = std::fs::read_to_string(&path)?;
    Ok(CustomCommand {
        name: name.to_string(),
        scope: "user".into(),
        path: path.display().to_string(),
        body: Some(body),
    })
}

pub fn write_command(name: &str, body: &str) -> anyhow::Result<()> {
    let dir = user_commands_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.md"));
    std::fs::write(&path, body)?;
    Ok(())
}

pub fn delete_command(name: &str) -> anyhow::Result<bool> {
    let path = user_commands_dir()
        .map(|d| d.join(format!("{name}.md")))
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)?;
    Ok(true)
}

/// Expand a custom command template with the given arguments. Substitutes
/// `$ARGUMENTS` in the body. Used by the /api/ask handler when `command` is set.
pub fn expand_command(name: &str, arguments: &str) -> anyhow::Result<String> {
    let cmd = read_command(name)?;
    let body = cmd
        .body
        .ok_or_else(|| anyhow::anyhow!("command {name} has no body"))?;
    Ok(body.replace(ARGUMENTS_PLACEHOLDER, arguments))
}

// ---------- Agents ----------

pub fn list_agents() -> Vec<Agent> {
    let mut out = Vec::new();

    if let Some(dir) = user_agents_dir() {
        scan_md_for_agents(&dir, "user", &mut out);
    }

    walk_plugin_subdirs("agents", |scope, dir| scan_md_for_agents(dir, scope, &mut out));

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Walk `~/.claude/plugins/cache/<marketplace>/<plugin>/<version>/<subdir>` and
/// invoke `f(scope, &subdir)` for each match. Plugin layout has 4 levels.
fn walk_plugin_subdirs<F: FnMut(&str, &PathBuf)>(subdir: &str, mut f: F) {
    let Some(plugins) = plugins_dir() else { return };
    let Ok(marketplaces) = std::fs::read_dir(&plugins) else { return };
    for marketplace in marketplaces.flatten() {
        let Ok(plugins_in) = std::fs::read_dir(marketplace.path()) else { continue };
        for plugin in plugins_in.flatten() {
            let pname = plugin.file_name().to_string_lossy().into_owned();
            let Ok(versions) = std::fs::read_dir(plugin.path()) else { continue };
            for v in versions.flatten() {
                let dir = v.path().join(subdir);
                if dir.exists() {
                    f(&format!("plugin:{pname}"), &dir);
                }
            }
        }
    }
}

// ---------- shared helpers ----------

fn scan_md_dir(
    dir: &PathBuf,
    scope: &str,
    out: &mut Vec<CustomCommand>,
    _include_body: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(CustomCommand {
            name: name.to_string(),
            scope: scope.to_string(),
            path: path.display().to_string(),
            body: None,
        });
    }
}

fn scan_md_for_agents(dir: &PathBuf, scope: &str, out: &mut Vec<Agent>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(Agent {
            name: name.to_string(),
            scope: scope.to_string(),
            path: path.display().to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_placeholder() {
        // Standalone helper test — doesn't need fs
        let body = "Run /loop with these args: $ARGUMENTS — repeat every minute.";
        let expanded = body.replace(ARGUMENTS_PLACEHOLDER, "check the logs");
        assert_eq!(
            expanded,
            "Run /loop with these args: check the logs — repeat every minute."
        );
    }

    #[test]
    fn parse_mcp_list_line_http() {
        // Single-line parsing logic matches what list_mcp_servers does internally;
        // test it inline since list_mcp_servers shells out to a real claude binary.
        let line = "palazzo: http://10.10.0.3:6335/mcp (HTTP) - ✓ Connected";
        let (name, rest) = line.split_once(':').unwrap();
        let rest = rest.trim().split(" - ").next().unwrap().trim();
        assert_eq!(name.trim(), "palazzo");
        assert!(rest.ends_with("(HTTP)"));
    }
}
