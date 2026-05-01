use std::path::PathBuf;

use crate::config::InstallArgs;

#[cfg(target_os = "macos")]
const DAEMON_LABEL: &str = "net.calii.apytti";

/// macOS LaunchAgent path (per-user, runs in user session — required for
/// Local Network Privacy attribution to the .app bundle).
#[cfg(target_os = "macos")]
fn launchagent_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{DAEMON_LABEL}.plist")))
}

#[cfg(target_os = "linux")]
fn unit_path() -> PathBuf {
    let cfg = dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config"));
    cfg.join("systemd/user/apytti.service")
}

#[cfg(target_os = "macos")]
fn default_working_dir() -> String {
    dirs::home_dir()
        .map(|h| h.join(".apytti").display().to_string())
        .unwrap_or_else(|| "/tmp/apytti".into())
}

#[cfg(target_os = "linux")]
fn default_working_dir() -> String {
    "/var/lib/apytti".into()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn default_working_dir() -> String {
    ".".into()
}

#[cfg(target_os = "macos")]
pub fn install(args: &InstallArgs) -> anyhow::Result<()> {
    // The signed/notarized .pkg installs the .app to /Applications and the
    // postinstall script seeds ~/.apytti/config.toml + opens the app once so
    // the user can grant Local Network Privacy. This subcommand only writes
    // the LaunchAgent that auto-starts the app at login.
    //
    // Per palazzo (id=1777633948413) and Apple DTS guidance:
    //   - LaunchAgent (user session) + .app bundle = correct, runs as cali, gets LN grant
    //   - LaunchDaemon as root (no UserName) = fallback when bundle approach isn't possible
    //   - LaunchDaemon with UserName=non-root = unsupported mixed context, don't use
    let user = std::env::var("USER").unwrap_or_else(|_| "cali".into());
    let home = std::env::var("HOME").unwrap_or_else(|_| format!("/Users/{user}"));
    let app_binary = "/Applications/Apytti.app/Contents/MacOS/apytti";
    let work_dir = args.dir.clone().unwrap_or_else(default_working_dir);
    let log_dir = format!("{home}/Library/Logs/Apytti");
    let plist_path = launchagent_path().ok_or_else(|| anyhow::anyhow!("could not resolve home dir"))?;

    let env_block = build_env_block(args);

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{app_binary}</string>
        <string>run</string>
        <string>--port</string>
        <string>{port}</string>
        <string>--host</string>
        <string>{host}</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{work_dir}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>{env_block}
    <key>StandardOutPath</key>
    <string>{log_dir}/apytti.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/apytti.err.log</string>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        label = DAEMON_LABEL,
        app_binary = app_binary,
        port = args.port,
        host = args.host,
        work_dir = work_dir,
        env_block = env_block,
    );

    println!("Writing LaunchAgent plist to {}", plist_path.display());
    println!("(runs in your user session as $USER — Local Network grant follows the .app bundle)");
    println!();
    println!("Plist content:");
    println!("---");
    println!("{xml}");
    println!("---");
    println!();
    println!("Run these commands:");
    println!("  mkdir -p {} {log_dir}", plist_path.parent().unwrap().display());
    println!("  mkdir -p {work_dir}");
    println!("  cat > {} <<'EOF'", plist_path.display());
    println!("{xml}");
    println!("EOF");
    println!("  launchctl bootstrap gui/$UID {}", plist_path.display());
    println!();
    println!("First-run note: open Apytti.app once and click Allow on the Local Network");
    println!("prompt. macOS keys this grant on the .app bundle ID — once granted, every");
    println!("subsequent invocation (including this LaunchAgent) inherits the grant.");
    println!();
    println!("Logs: {log_dir}/apytti.{{log,err.log}}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn build_env_block(args: &InstallArgs) -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "cali".into());
    let home = std::env::var("HOME").unwrap_or_else(|_| format!("/Users/{user}"));
    let path = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

    let mut env_pairs: Vec<(&str, String)> = vec![
        ("HOME", home),
        ("USER", user),
        ("PATH", path.into()),
    ];
    if let Some(url) = &args.hermytt_url {
        env_pairs.push(("APYTTI_HERMYTT_URL", url.clone()));
    }
    if let Some(token) = &args.hermytt_token {
        env_pairs.push(("APYTTI_HERMYTT_TOKEN", token.clone()));
    }
    let mut s = String::from("\n    <key>EnvironmentVariables</key>\n    <dict>\n");
    for (k, v) in env_pairs {
        s.push_str(&format!("        <key>{k}</key>\n        <string>{v}</string>\n"));
    }
    s.push_str("    </dict>");
    s
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> anyhow::Result<()> {
    let plist = launchagent_path().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    println!("Run these commands to remove the LaunchAgent:");
    println!("  launchctl bootout gui/$UID {}", plist.display());
    println!("  rm {}", plist.display());
    println!();
    println!("To also remove the app + config:");
    println!("  rm -rf /Applications/Apytti.app");
    println!("  rm -f /usr/local/bin/apytti");
    println!("  rm -rf ~/.apytti");
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn install(args: &InstallArgs) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let unit = unit_path();
    let log_dir = dirs::home_dir()
        .map(|h| h.join(".apytti/logs"))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let work_dir = args.dir.clone().unwrap_or_else(default_working_dir);

    std::fs::create_dir_all(&log_dir).ok();
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let user = std::env::var("USER").unwrap_or_else(|_| "cali".into());
    let home = std::env::var("HOME").unwrap_or_else(|_| format!("/home/{user}"));
    let path = "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin";
    let mut environment = format!(
        "Environment=\"HOME={home}\"\n         \
         Environment=\"USER={user}\"\n         \
         Environment=\"PATH={path}\"\n         "
    );
    if let Some(url) = &args.hermytt_url {
        environment.push_str(&format!("Environment=\"APYTTI_HERMYTT_URL={url}\"\n         "));
    }
    if let Some(token) = &args.hermytt_token {
        environment.push_str(&format!("Environment=\"APYTTI_HERMYTT_TOKEN={token}\"\n         "));
    }

    let content = format!(
        "[Unit]\n\
         Description=apytti — multi-backend AI CLI gateway\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         WorkingDirectory={work_dir}\n\
         {environment}ExecStart={exe} run --port {port} --host {host} --no-menu\n\
         Restart=always\n\
         RestartSec=5\n\
         StandardOutput=append:{log_dir}/apytti.log\n\
         StandardError=append:{log_dir}/apytti.err.log\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display(),
        port = args.port,
        host = args.host,
        work_dir = work_dir,
        environment = environment,
        log_dir = log_dir.display(),
    );

    std::fs::write(&unit, &content)?;
    println!("Wrote systemd user unit: {}", unit.display());
    println!();
    println!("Activate:");
    println!("  mkdir -p {work_dir}");
    println!("  systemctl --user daemon-reload");
    println!("  systemctl --user enable --now apytti");
    println!();
    println!("Logs: journalctl --user -u apytti -f");
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn uninstall() -> anyhow::Result<()> {
    let unit = unit_path();
    println!("Run these commands manually to remove:");
    println!("  systemctl --user disable --now apytti");
    println!("  rm {}", unit.display());
    println!("  systemctl --user daemon-reload");
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn install(args: &InstallArgs) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let work_dir = args.dir.clone().unwrap_or_else(default_working_dir);
    println!("Run as administrator:");
    println!(
        "  sc.exe create apytti binPath= \"{} run --port {} --host {}\" start= auto",
        exe.display(),
        args.port,
        args.host
    );
    if let Some(url) = &args.hermytt_url {
        println!("  setx APYTTI_HERMYTT_URL \"{}\"", url);
    }
    if let Some(token) = &args.hermytt_token {
        println!("  setx APYTTI_HERMYTT_TOKEN \"{}\"", token);
    }
    println!("  mkdir {}", work_dir);
    println!("  sc.exe start apytti");
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn uninstall() -> anyhow::Result<()> {
    println!("Run as administrator:");
    println!("  sc.exe stop apytti");
    println!("  sc.exe delete apytti");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn install(_args: &InstallArgs) -> anyhow::Result<()> {
    anyhow::bail!("install not supported on this OS")
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn uninstall() -> anyhow::Result<()> {
    anyhow::bail!("uninstall not supported on this OS")
}

/// Print daemon status as JSON: installed, running, version, paths.
pub fn status() -> anyhow::Result<()> {
    let report = status_report();
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn status_report() -> serde_json::Value {
    #[cfg(target_os = "macos")]
    {
        let plist = launchagent_path().unwrap_or_default();
        let installed = plist.exists();
        let running = if installed {
            std::process::Command::new("launchctl")
                .args(["print", &format!("gui/$UID/{DAEMON_LABEL}")])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            false
        };
        return serde_json::json!({
            "installed": installed,
            "running": running,
            "version": env!("CARGO_PKG_VERSION"),
            "platform": "macos",
            "launchagent_path": plist.display().to_string(),
            "label": DAEMON_LABEL,
            "app_path": "/Applications/Apytti.app",
        });
    }

    #[cfg(target_os = "linux")]
    {
        let unit = unit_path();
        let installed = unit.exists();
        let running = if installed {
            std::process::Command::new("systemctl")
                .args(["--user", "is-active", "apytti"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
                .unwrap_or(false)
        } else {
            false
        };
        return serde_json::json!({
            "installed": installed,
            "running": running,
            "version": env!("CARGO_PKG_VERSION"),
            "platform": "linux",
            "unit_path": unit.display().to_string(),
        });
    }

    #[cfg(target_os = "windows")]
    {
        let installed = std::process::Command::new("sc.exe")
            .args(["query", "apytti"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let running = if installed {
            std::process::Command::new("sc.exe")
                .args(["query", "apytti"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains("RUNNING"))
                .unwrap_or(false)
        } else {
            false
        };
        return serde_json::json!({
            "installed": installed,
            "running": running,
            "version": env!("CARGO_PKG_VERSION"),
            "platform": "windows",
        });
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    serde_json::json!({
        "installed": false,
        "running": false,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": "unknown",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_report_has_required_fields() {
        let r = status_report();
        assert!(r["installed"].is_boolean());
        assert!(r["running"].is_boolean());
        assert_eq!(r["version"], env!("CARGO_PKG_VERSION"));
        assert!(r["platform"].is_string());
    }
}
