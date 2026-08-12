//! Self-update for the macOS `.app` install.
//!
//! Checks GitHub releases on a timer; applies only when asked (`POST /update/apply`
//! or the menu-bar item). The split is deliberate: a background check is cheap and
//! reversible, whereas swapping the binary is code execution, so a human stays in
//! the loop.
//!
//! # Why not `installer -pkg`
//!
//! A `.pkg` install needs admin rights, which means either an interactive prompt
//! or a root helper daemon whose whole job is installing code fetched from the
//! network. Neither is necessary here: `/Applications` is group-`admin` writable,
//! apytti runs as that user, and macOS's App Management restriction doesn't stop
//! an app replacing *itself*. So we unpack the pkg payload ourselves and do an
//! atomic rename swap. The `.pkg` remains the first-install path (it lays down the
//! `/usr/local/bin/apytti` symlink and needs admin once); updates never touch
//! `installer` again. The symlink points at a path, not an inode, so it stays
//! valid across swaps.
//!
//! # Verification is not optional
//!
//! With `installer` out of the loop, nothing else validates the download — so this
//! module does all of it before touching `/Applications`: SHA-256 against the
//! release's `SHA256SUMS`, `codesign --verify --deep --strict`, a Gatekeeper
//! assessment, a pinned Team ID, and a bundle-identifier match against the running
//! app. Any failure aborts and leaves the running version alone; there is no
//! "install anyway" path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

const REPO: &str = "yttfam/apytti";
/// Pinned signing identity. "Validly signed by someone" is not good enough — this
/// must be *our* Developer ID, or the swap is an arbitrary-code-execution path.
const TEAM_ID: &str = "XJQQCN392F";
const CHECK_INTERVAL_SECS: u64 = 3600;
const USER_AGENT: &str = concat!("apytti/", env!("CARGO_PKG_VERSION"));

/// Result of the most recent update check.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateStatus {
    pub current: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    pub available: bool,
    /// False when this install can't self-update (not macOS, or not running from
    /// a `.app` bundle — e.g. a Homebrew/cargo binary or a Linux daemon).
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub type SharedUpdateState = Arc<RwLock<UpdateStatus>>;

pub fn initial_state() -> SharedUpdateState {
    Arc::new(RwLock::new(UpdateStatus {
        current: env!("CARGO_PKG_VERSION").to_string(),
        supported: bundle_root().is_some(),
        ..Default::default()
    }))
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize, Clone)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// Path of the `.app` bundle we're running from, if any.
///
/// `current_exe()` is `<bundle>/Contents/MacOS/apytti`, so the bundle is three
/// levels up. Returns None when that shape doesn't hold, which is how a
/// non-bundle install (cargo, Homebrew, Linux) reports "can't self-update".
pub fn bundle_root() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    if bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        return None;
    }
    if !bundle.join("Contents/Info.plist").exists() {
        return None;
    }
    Some(bundle.to_path_buf())
}

/// Parse a dotted version into comparable numbers. Tolerates a leading `v`.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    // Tolerate trailing pre-release junk on the patch component (0.6.11-rc1).
    let patch_raw = parts.next().unwrap_or("0");
    let patch = patch_raw
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("0")
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

/// True when `latest` is strictly newer than `current`.
///
/// Numeric comparison, not string equality — string compare can't tell newer from
/// older, so it would happily "update" you to an older release after a rollback.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

async fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .unwrap_or_default()
}

async fn fetch_latest_release() -> anyhow::Result<GhRelease> {
    let release: GhRelease = http()
        .await
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(release)
}

/// Run one update check and fold the result into shared state.
pub async fn check_now(state: &SharedUpdateState) -> UpdateStatus {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let supported = bundle_root().is_some();

    let mut status = UpdateStatus {
        current: current.clone(),
        supported,
        checked_at: Some(crate::models::epoch_to_iso(now_secs())),
        ..Default::default()
    };

    match fetch_latest_release().await {
        Ok(release) => {
            let latest = release.tag_name.trim_start_matches('v').to_string();
            status.available = is_newer(&latest, &current);
            status.latest = Some(latest);
        }
        Err(e) => {
            status.error = Some(format!("update check failed: {e}"));
            warn!("update check failed: {e}");
        }
    }

    *state.write().await = status.clone();
    status
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Background loop: check on startup, then hourly.
///
/// Jittered by port so several apytti instances behind one NAT don't stampede the
/// GitHub API in lockstep.
pub async fn check_loop(state: SharedUpdateState, jitter_seed: u16) {
    let jitter = (jitter_seed as u64) % 300;
    tokio::time::sleep(std::time::Duration::from_secs(5 + jitter)).await;
    loop {
        let s = check_now(&state).await;
        if s.available {
            info!(
                current = s.current,
                latest = s.latest.as_deref().unwrap_or("-"),
                "update available — apply with POST /update/apply"
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(CHECK_INTERVAL_SECS + jitter)).await;
    }
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

/// Download, verify, and swap in the latest release, then relaunch.
///
/// Returns the version that was staged. The caller is expected to flush its HTTP
/// response and then exit the process — the relaunch helper spawned here waits for
/// that exit before starting the new build.
pub async fn apply(state: &SharedUpdateState) -> anyhow::Result<String> {
    let bundle = bundle_root().ok_or_else(|| {
        anyhow::anyhow!(
            "self-update is only supported for the macOS .app install \
             (not running from a bundle)"
        )
    })?;

    let current = env!("CARGO_PKG_VERSION");
    let release = fetch_latest_release().await?;
    let latest = release.tag_name.trim_start_matches('v').to_string();

    if !is_newer(&latest, current) {
        anyhow::bail!("already up to date (running {current}, latest {latest})");
    }

    let workdir = std::env::temp_dir().join(format!("apytti-update-{latest}"));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir)?;

    let pkg_name = format!("apytti-{latest}.pkg");
    let pkg_asset = release
        .assets
        .iter()
        .find(|a| a.name == pkg_name)
        .ok_or_else(|| anyhow::anyhow!("release {latest} has no asset named {pkg_name}"))?
        .clone();
    let sums_asset = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS")
        .ok_or_else(|| anyhow::anyhow!("release {latest} has no SHA256SUMS"))?
        .clone();

    let pkg_path = workdir.join(&pkg_name);
    download(&pkg_asset.browser_download_url, &pkg_path).await?;
    let sums_path = workdir.join("SHA256SUMS");
    download(&sums_asset.browser_download_url, &sums_path).await?;

    verify_sha256(&pkg_path, &sums_path, &pkg_name)?;
    info!(version = latest, "update: sha256 verified");

    // Unpack the payload rather than running `installer` — see module docs.
    let expanded = workdir.join("expanded");
    run("/usr/sbin/pkgutil", &["--expand-full", pkg_path.to_str().unwrap(), expanded.to_str().unwrap()])?;
    let new_app = expanded.join("Payload/Applications/Apytti.app");
    if !new_app.exists() {
        anyhow::bail!("unexpected pkg layout: {} not found", new_app.display());
    }

    verify_bundle(&new_app, &bundle, &latest)?;
    info!(version = latest, "update: signature, team, and identity verified");

    // Stage on the same volume as the destination so the swap is a rename, not a
    // copy — a copy has a window where the bundle is half-written.
    let parent = bundle
        .parent()
        .ok_or_else(|| anyhow::anyhow!("bundle has no parent dir"))?;
    let staging = parent.join(".apytti-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let staged_app = staging.join("Apytti.app");
    run("/usr/bin/ditto", &[new_app.to_str().unwrap(), staged_app.to_str().unwrap()])?;

    // Re-verify after the copy: ditto preserves signatures, but this is the copy
    // we're actually going to run, so check that one rather than trusting that it
    // matches what we validated a moment ago.
    verify_bundle(&staged_app, &bundle, &latest)?;

    let previous = parent.join("Apytti.app.previous");
    let _ = std::fs::remove_dir_all(&previous);
    std::fs::rename(&bundle, &previous)
        .map_err(|e| anyhow::anyhow!("failed to move current bundle aside: {e}"))?;
    if let Err(e) = std::fs::rename(&staged_app, &bundle) {
        // Put it back rather than leaving the machine with no app at all.
        let _ = std::fs::rename(&previous, &bundle);
        anyhow::bail!("failed to move new bundle into place (rolled back): {e}");
    }
    let _ = std::fs::remove_dir_all(&staging);
    info!(version = latest, "update: bundle swapped, relaunching");

    spawn_relaunch_helper(&bundle, &previous, &latest)?;

    let mut s = state.write().await;
    s.latest = Some(latest.clone());
    s.available = false;

    Ok(latest)
}

async fn download(url: &str, dest: &Path) -> anyhow::Result<()> {
    let bytes = http()
        .await
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    std::fs::write(dest, &bytes)?;
    Ok(())
}

/// Check the download against the release's own SHA256SUMS.
pub fn verify_sha256(file: &Path, sums: &Path, name: &str) -> anyhow::Result<()> {
    let sums_text = std::fs::read_to_string(sums)?;
    let expected = sums_text
        .lines()
        .find_map(|l| {
            let mut parts = l.split_whitespace();
            let hash = parts.next()?;
            let fname = parts.next()?.trim_start_matches('*');
            (fname == name).then(|| hash.to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("{name} not listed in SHA256SUMS"))?;

    let out = std::process::Command::new("/usr/bin/shasum")
        .args(["-a", "256", file.to_str().unwrap()])
        .output()?;
    let actual = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();

    if actual != expected {
        anyhow::bail!("sha256 mismatch for {name}: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Full trust chain for a candidate bundle. Any failure here aborts the update.
fn verify_bundle(candidate: &Path, running: &Path, expected_version: &str) -> anyhow::Result<()> {
    let path = candidate.to_str().unwrap();

    run("/usr/bin/codesign", &["--verify", "--deep", "--strict", path])
        .map_err(|e| anyhow::anyhow!("codesign verification failed: {e}"))?;

    run("/usr/sbin/spctl", &["-a", "--type", "execute", path])
        .map_err(|e| anyhow::anyhow!("Gatekeeper rejected the download: {e}"))?;

    let details = std::process::Command::new("/usr/bin/codesign")
        .args(["-dv", path])
        .output()?;
    // codesign -dv writes to stderr.
    let details = String::from_utf8_lossy(&details.stderr).into_owned();

    let team = field(&details, "TeamIdentifier=");
    if team.as_deref() != Some(TEAM_ID) {
        anyhow::bail!(
            "team identifier mismatch: expected {TEAM_ID}, got {}",
            team.as_deref().unwrap_or("<none>")
        );
    }

    let new_id = field(&details, "Identifier=");
    let running_id = bundle_identifier(running);
    if let (Some(new_id), Some(running_id)) = (&new_id, &running_id) {
        if new_id != running_id {
            // Changing the bundle id would drop TCC grants (Local Network in
            // particular) and re-prompt the user.
            anyhow::bail!(
                "bundle identifier mismatch: running {running_id}, download {new_id}"
            );
        }
    }

    let staged_version = bundle_version(candidate);
    if let Some(v) = &staged_version {
        if v != expected_version {
            anyhow::bail!(
                "version mismatch: release claims {expected_version}, bundle contains {v}"
            );
        }
    }

    Ok(())
}

/// Pull `Key=value` out of `codesign -dv` output.
fn field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix(key))
        .map(|v| v.trim().to_string())
}

fn plist_value(bundle: &Path, key: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/defaults")
        .arg("read")
        .arg(bundle.join("Contents/Info.plist"))
        .arg(key)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

pub fn bundle_identifier(bundle: &Path) -> Option<String> {
    plist_value(bundle, "CFBundleIdentifier")
}

pub fn bundle_version(bundle: &Path) -> Option<String> {
    plist_value(bundle, "CFBundleShortVersionString")
}

fn run(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let out = std::process::Command::new(bin).args(args).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("{bin} {} failed: {}", args.join(" "), err.trim());
    }
    Ok(())
}

/// Write and spawn a detached helper that relaunches the app once we exit, then
/// rolls back if the new build doesn't come up healthy.
///
/// This has to be an external process: we're about to replace and exit, so nothing
/// in-process can supervise the result.
fn spawn_relaunch_helper(bundle: &Path, previous: &Path, version: &str) -> anyhow::Result<()> {
    let script_path = std::env::temp_dir().join("apytti-relaunch.sh");
    let script = format!(
        r#"#!/bin/sh
# Generated by apytti self-update. Waits for the old process to exit, starts the
# new build, and restores the previous bundle if it never reports healthy.
set -u
BUNDLE="{bundle}"
PREVIOUS="{previous}"
VERSION="{version}"

sleep 2
/usr/bin/open -a "$BUNDLE"

# Give the new build up to 60s to answer /health with the expected version.
i=0
while [ $i -lt 30 ]; do
    sleep 2
    got=$(/usr/bin/curl -s --max-time 3 http://127.0.0.1:{port}/health 2>/dev/null)
    case "$got" in
        *"\"version\":\"$VERSION\""*)
            /bin/rm -rf "$PREVIOUS"
            exit 0
            ;;
    esac
    i=$((i + 1))
done

# Never came up. Put the old bundle back and start it.
/usr/bin/pkill -f "$BUNDLE/Contents/MacOS/apytti" 2>/dev/null
/bin/rm -rf "$BUNDLE"
/bin/mv "$PREVIOUS" "$BUNDLE"
/usr/bin/open -a "$BUNDLE"
exit 1
"#,
        bundle = bundle.display(),
        previous = previous.display(),
        version = version,
        port = crate::update_port(),
    );
    std::fs::write(&script_path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
    }

    std::process::Command::new("/bin/sh")
        .arg(&script_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_version() {
        assert_eq!(parse_version("0.6.11"), Some((0, 6, 11)));
    }

    #[test]
    fn parses_v_prefixed_tag() {
        assert_eq!(parse_version("v0.6.11"), Some((0, 6, 11)));
    }

    #[test]
    fn parses_prerelease_patch() {
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_version("not-a-version"), None);
    }

    #[test]
    fn newer_detects_patch_bump() {
        assert!(is_newer("0.6.11", "0.6.8"));
    }

    #[test]
    fn numeric_not_lexicographic() {
        // The bug string comparison would introduce: "0.6.8" > "0.6.11" as text.
        assert!(is_newer("0.6.11", "0.6.8"));
        assert!(!is_newer("0.6.8", "0.6.11"));
    }

    #[test]
    fn same_version_is_not_newer() {
        assert!(!is_newer("0.6.11", "0.6.11"));
    }

    #[test]
    fn older_is_not_newer() {
        // Guards against "updating" backwards after a rollback.
        assert!(!is_newer("0.6.10", "0.6.11"));
    }

    #[test]
    fn minor_and_major_bumps() {
        assert!(is_newer("0.7.0", "0.6.99"));
        assert!(is_newer("1.0.0", "0.99.99"));
    }

    #[test]
    fn unparseable_never_triggers_update() {
        assert!(!is_newer("garbage", "0.6.11"));
        assert!(!is_newer("0.6.12", "garbage"));
    }

    #[test]
    fn sha256_accepts_matching_digest() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("thing.pkg");
        std::fs::write(&f, b"hello world").unwrap();
        let digest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let sums = dir.path().join("SHA256SUMS");
        std::fs::write(&sums, format!("{digest}  thing.pkg\n")).unwrap();

        assert!(verify_sha256(&f, &sums, "thing.pkg").is_ok());
    }

    #[test]
    fn sha256_rejects_tampered_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("thing.pkg");
        std::fs::write(&f, b"tampered").unwrap();
        let sums = dir.path().join("SHA256SUMS");
        std::fs::write(
            &sums,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9  thing.pkg\n",
        )
        .unwrap();

        let err = verify_sha256(&f, &sums, "thing.pkg").unwrap_err().to_string();
        assert!(err.contains("sha256 mismatch"), "{err}");
    }

    #[test]
    fn sha256_rejects_unlisted_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("thing.pkg");
        std::fs::write(&f, b"x").unwrap();
        let sums = dir.path().join("SHA256SUMS");
        std::fs::write(&sums, "abc  other.pkg\n").unwrap();

        let err = verify_sha256(&f, &sums, "thing.pkg").unwrap_err().to_string();
        assert!(err.contains("not listed"), "{err}");
    }

    #[test]
    fn field_extracts_codesign_keys() {
        let out = "Executable=/Applications/Apytti.app/Contents/MacOS/apytti\n\
                   Identifier=net.calii.apytti.app\n\
                   TeamIdentifier=XJQQCN392F\n";
        assert_eq!(field(out, "Identifier="), Some("net.calii.apytti.app".into()));
        assert_eq!(field(out, "TeamIdentifier="), Some("XJQQCN392F".into()));
    }

    #[test]
    fn field_missing_key_is_none() {
        assert_eq!(field("Identifier=x\n", "TeamIdentifier="), None);
    }
}
