use std::path::PathBuf;

use crate::config::InstallArgs;

#[cfg(target_os = "macos")]
const DAEMON_LABEL: &str = "net.calii.apytti";

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    PathBuf::from("/Library/LaunchDaemons").join(format!("{DAEMON_LABEL}.plist"))
}

#[cfg(target_os = "linux")]
fn unit_path() -> PathBuf {
    // User-level systemd unit (no root required)
    let cfg = dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config"));
    cfg.join("systemd/user/apytti.service")
}

#[cfg(target_os = "macos")]
pub fn install(args: &InstallArgs) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
    let plist = plist_path();
    let log_dir = dirs::home_dir()
        .map(|h| h.join(".apytti/logs"))
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    std::fs::create_dir_all(&log_dir).ok();

    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>run</string>
        <string>--port</string>
        <string>{port}</string>
        <string>--host</string>
        <string>{host}</string>
    </array>
    <key>UserName</key>
    <string>{user}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_dir}/apytti.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/apytti.err.log</string>
</dict>
</plist>
"#,
        label = DAEMON_LABEL,
        exe = exe.display(),
        port = args.port,
        host = args.host,
        user = user,
        log_dir = log_dir.display(),
    );

    println!("Writing LaunchDaemon plist to {}", plist.display());
    println!("(requires sudo)");
    println!();
    println!("Plist content:");
    println!("---");
    println!("{xml}");
    println!("---");
    println!();
    println!("Run these commands manually:");
    println!("  sudo tee {} > /dev/null <<'EOF'", plist.display());
    println!("{xml}");
    println!("EOF");
    println!("  sudo launchctl bootstrap system {}", plist.display());
    println!();
    println!("Logs: {log_dir}/apytti.{{log,err.log}}", log_dir = log_dir.display());
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> anyhow::Result<()> {
    let plist = plist_path();
    println!("Run these commands manually to remove:");
    println!("  sudo launchctl bootout system {}", plist.display());
    println!("  sudo rm {}", plist.display());
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn install(args: &InstallArgs) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let unit = unit_path();
    let log_dir = dirs::home_dir()
        .map(|h| h.join(".apytti/logs"))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&log_dir).ok();
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let content = format!(
        "[Unit]\n\
         Description=apytti — multi-backend AI CLI gateway\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} run --port {port} --host {host}\n\
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
        log_dir = log_dir.display(),
    );

    std::fs::write(&unit, &content)?;
    println!("Wrote systemd user unit: {}", unit.display());
    println!();
    println!("Activate:");
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
    println!("Run as administrator:");
    println!(
        "  sc.exe create apytti binPath= \"{} run --port {} --host {}\" start= auto",
        exe.display(),
        args.port,
        args.host
    );
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
