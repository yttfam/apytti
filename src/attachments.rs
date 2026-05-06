//! Attachment handling for `/api/ask`. Pyttch-bridge (and any other caller)
//! can pass `attachments[]` instead of munging file paths into the prompt
//! itself. Apytti validates each path, optionally enforces a config-level
//! whitelist of allowed roots, and lets the claude backend mint per-call
//! `--allowedTools Read(<path>)` rules so the CLI can read the file without
//! `--dangerously-skip-permissions`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// How long materialized inbox files survive after the `/api/ask` request
/// returns. External agents (e.g. Lou over Telegram via pyttch-bridge) read
/// the path from the response and call back to read the file a few seconds
/// later — synchronous request-bound cleanup races with that and deletes
/// the file before the agent can read it. 5 minutes gives the agent
/// (and a human-in-the-loop reviewer) enough headroom.
const DEFAULT_INBOX_TTL_SECS: u64 = 300;

static INBOX_TTL_SECS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(DEFAULT_INBOX_TTL_SECS);

fn inbox_ttl() -> Duration {
    Duration::from_secs(INBOX_TTL_SECS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Override the inbox TTL at runtime. Intended for tests; production code
/// should leave the default in place.
#[doc(hidden)]
pub fn set_inbox_ttl_secs_for_tests(secs: u64) {
    INBOX_TTL_SECS.store(secs, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Absolute path on apytti's filesystem. Mutually exclusive with `data`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Base64-encoded raw bytes. Mutually exclusive with `path`. Apytti
    /// materializes these to a per-request temp dir under `~/.apytti/inbox/`
    /// and cleans up when the request finishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Attachment {
    /// Resolve `kind`: explicit when set, otherwise infer from name/path.
    pub fn effective_kind(&self) -> &str {
        if let Some(k) = self.kind.as_deref() {
            return k;
        }
        let hint = self
            .name
            .as_deref()
            .or(self.path.as_deref())
            .unwrap_or("");
        infer_kind_from_path(hint)
    }

    /// Display name: explicit `name`, else the path's basename, else "attachment".
    pub fn display_name(&self) -> String {
        if let Some(n) = self.name.as_deref() {
            return n.to_string();
        }
        if let Some(p) = self.path.as_deref() {
            return Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string());
        }
        "attachment".to_string()
    }
}

fn infer_kind_from_path(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" | "heif" => "image",
        "mp3" | "wav" | "flac" | "m4a" | "aac" => "audio",
        "ogg" | "oga" | "opus" => "voice",
        "mp4" | "mov" | "mkv" | "webm" | "avi" => "video",
        _ => "document",
    }
}

/// An attachment after path-vs-data resolution. The `path` is always a real
/// file on apytti's filesystem; `display_name` and `kind` carry through.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub path: String,
    pub display_name: String,
    pub kind: String,
}

/// Guard for inbox temp dirs created by `materialize`. On drop, schedules
/// deletion of the dir after `APYTTI_INBOX_TTL_SECS` (default 5 min) so
/// that downstream agents who receive the path in the API response have
/// time to read the file before it disappears.
#[derive(Debug)]
pub struct InboxGuard {
    dir: Option<PathBuf>,
}

impl Drop for InboxGuard {
    fn drop(&mut self) {
        if let Some(d) = self.dir.take() {
            let ttl = inbox_ttl();
            if ttl.is_zero() {
                let _ = std::fs::remove_dir_all(&d);
            } else {
                std::thread::spawn(move || {
                    std::thread::sleep(ttl);
                    let _ = std::fs::remove_dir_all(&d);
                });
            }
        }
    }
}

/// Resolve attachments: validate paths, materialize base64 `data` to a
/// per-request temp dir under `inbox_root` (typically `~/.apytti/inbox/`).
/// Returns the resolved entries plus a guard whose Drop cleans up the temp
/// dir. Caller must hold the guard until after dispatch returns.
pub fn resolve(
    attachments: &[Attachment],
    roots: &[String],
    inbox_root: &Path,
) -> Result<(Vec<Resolved>, InboxGuard), String> {
    if attachments.is_empty() {
        return Ok((Vec::new(), InboxGuard { dir: None }));
    }

    let mut needs_temp_dir = false;
    for att in attachments {
        match (att.path.as_deref(), att.data.as_deref()) {
            (Some(_), Some(_)) => {
                return Err("attachment has both `path` and `data`; provide exactly one".into());
            }
            (None, None) => {
                return Err("attachment must have either `path` or `data`".into());
            }
            (None, Some(_)) => needs_temp_dir = true,
            (Some(_), None) => {}
        }
    }

    let temp_dir = if needs_temp_dir {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let d = inbox_root.join(&suffix[..16]);
        std::fs::create_dir_all(&d)
            .map_err(|e| format!("failed to create inbox dir {}: {e}", d.display()))?;
        Some(d)
    } else {
        None
    };

    let guard = InboxGuard { dir: temp_dir.clone() };

    let mut out = Vec::with_capacity(attachments.len());
    for (idx, att) in attachments.iter().enumerate() {
        let resolved = if let Some(p) = att.path.as_deref() {
            validate_path(p, roots)?;
            Resolved {
                path: p.to_string(),
                display_name: att.display_name(),
                kind: att.effective_kind().to_string(),
            }
        } else {
            let data_b64 = att.data.as_deref().expect("checked above");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data_b64.trim())
                .map_err(|e| format!("attachment[{idx}].data: invalid base64 ({e})"))?;
            let dir = temp_dir
                .as_deref()
                .expect("temp dir created when needed");
            let filename = sanitize_filename(&att.display_name(), idx, att.effective_kind());
            let path = dir.join(&filename);
            std::fs::write(&path, &bytes)
                .map_err(|e| format!("failed to write attachment[{idx}]: {e}"))?;
            Resolved {
                path: path.to_string_lossy().into_owned(),
                display_name: att.display_name(),
                kind: att.effective_kind().to_string(),
            }
        };
        out.push(resolved);
    }

    Ok((out, guard))
}

fn sanitize_filename(name: &str, idx: usize, kind: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c == '_');
    if trimmed.is_empty() {
        let ext = match kind {
            "image" => "bin",
            "voice" => "ogg",
            "audio" => "mp3",
            "video" => "mp4",
            _ => "bin",
        };
        format!("att_{idx}.{ext}")
    } else {
        format!("{idx}_{}", trimmed)
    }
}

fn validate_path(path: &str, roots: &[String]) -> Result<(), String> {
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return Err(format!("attachment path must be absolute: {path}"));
    }
    let meta = std::fs::metadata(&p)
        .map_err(|e| format!("attachment not accessible: {path} ({e})"))?;
    if !meta.is_file() {
        return Err(format!("attachment is not a regular file: {path}"));
    }
    if !roots.is_empty() {
        let canonical = std::fs::canonicalize(&p).unwrap_or(p.clone());
        let allowed = roots.iter().any(|r| {
            let root_canon = std::fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r));
            canonical.starts_with(&root_canon)
        });
        if !allowed {
            return Err(format!("attachment path {path} is outside allowed roots"));
        }
    }
    Ok(())
}

/// Build the prompt prefix that gets prepended to the user's prompt for
/// CLI-wrapped backends. Empty string when no attachments.
pub fn prompt_prefix(resolved: &[Resolved]) -> String {
    if resolved.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for r in resolved {
        out.push_str(&format!(
            "[attached {}: {} -> {}]\n",
            r.kind, r.display_name, r.path
        ));
    }
    out.push('\n');
    out
}

/// `Read(<path>)` allow rules, one per resolved attachment, for `--allowedTools`.
pub fn allow_rules(resolved: &[Resolved]) -> Vec<String> {
    resolved
        .iter()
        .map(|r| format!("Read({})", r.path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(path: &str) -> Attachment {
        Attachment {
            path: Some(path.into()),
            data: None,
            kind: None,
            name: None,
        }
    }

    fn att_data(b64: &str, name: Option<&str>, kind: Option<&str>) -> Attachment {
        Attachment {
            path: None,
            data: Some(b64.into()),
            name: name.map(String::from),
            kind: kind.map(String::from),
        }
    }

    #[test]
    fn infer_kind_from_extension() {
        assert_eq!(att("/tmp/a.jpg").effective_kind(), "image");
        assert_eq!(att("/tmp/a.PDF").effective_kind(), "document");
        assert_eq!(att("/tmp/a.ogg").effective_kind(), "voice");
        assert_eq!(att("/tmp/a.mp4").effective_kind(), "video");
        assert_eq!(att("/tmp/a.mp3").effective_kind(), "audio");
        assert_eq!(att("/tmp/no_ext").effective_kind(), "document");
    }

    #[test]
    fn explicit_kind_wins() {
        let a = Attachment {
            path: Some("/tmp/a.jpg".into()),
            data: None,
            kind: Some("document".into()),
            name: None,
        };
        assert_eq!(a.effective_kind(), "document");
    }

    #[test]
    fn data_kind_inferred_from_name() {
        let a = att_data("aGVsbG8=", Some("photo.jpg"), None);
        assert_eq!(a.effective_kind(), "image");
    }

    #[test]
    fn display_name_falls_back_to_basename() {
        assert_eq!(att("/tmp/foo/bar.jpg").display_name(), "bar.jpg");
        let a = Attachment {
            path: Some("/tmp/foo/bar.jpg".into()),
            data: None,
            kind: None,
            name: Some("kitchen.jpg".into()),
        };
        assert_eq!(a.display_name(), "kitchen.jpg");
    }

    fn inbox() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn rejects_both_path_and_data() {
        let a = Attachment {
            path: Some("/tmp/x".into()),
            data: Some("aGk=".into()),
            kind: None,
            name: None,
        };
        let inb = inbox();
        let err = resolve(&[a], &[], inb.path()).unwrap_err();
        assert!(err.contains("both"));
    }

    #[test]
    fn rejects_neither_path_nor_data() {
        let a = Attachment { path: None, data: None, kind: None, name: None };
        let inb = inbox();
        let err = resolve(&[a], &[], inb.path()).unwrap_err();
        assert!(err.contains("either"));
    }

    #[test]
    fn rejects_relative_path() {
        let inb = inbox();
        let err = resolve(&[att("relative.jpg")], &[], inb.path()).unwrap_err();
        assert!(err.contains("absolute"));
    }

    #[test]
    fn rejects_missing_file() {
        let inb = inbox();
        let err = resolve(
            &[att("/tmp/__apytti_does_not_exist_xyz")],
            &[],
            inb.path(),
        )
        .unwrap_err();
        assert!(err.contains("not accessible"));
    }

    #[test]
    fn accepts_real_file_no_roots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, b"hi").unwrap();
        let inb = inbox();
        let (resolved, _g) = resolve(&[att(path.to_str().unwrap())], &[], inb.path()).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].path, path.to_str().unwrap());
    }

    #[test]
    fn enforces_roots_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, b"hi").unwrap();
        let inb = inbox();
        let a = att(path.to_str().unwrap());

        let err = resolve(&[a.clone()], &["/var/lib/apytti".into()], inb.path()).unwrap_err();
        assert!(err.contains("outside allowed roots"));

        let root = dir.path().to_str().unwrap().to_string();
        let (resolved, _g) = resolve(&[a], &[root], inb.path()).unwrap();
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn rejects_non_regular_file() {
        let inb = inbox();
        let err = resolve(&[att("/tmp")], &[], inb.path()).unwrap_err();
        assert!(err.contains("not a regular file"));
    }

    // Tests that mutate the global INBOX_TTL_SECS must serialize.
    static TTL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn data_form_writes_file_and_cleans_up() {
        let _g = TTL_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Force immediate cleanup for this test; default is 300s deferred.
        set_inbox_ttl_secs_for_tests(0);
        let inb = inbox();
        let a = att_data("aGVsbG8gd29ybGQ=", Some("greeting.txt"), None);
        let (resolved, guard) = resolve(&[a], &[], inb.path()).unwrap();
        assert_eq!(resolved.len(), 1);
        let materialized = PathBuf::from(&resolved[0].path);
        assert!(materialized.exists(), "materialized file should exist");
        assert_eq!(std::fs::read(&materialized).unwrap(), b"hello world");
        assert!(materialized.starts_with(inb.path()));

        let parent = materialized.parent().unwrap().to_path_buf();
        drop(guard);
        assert!(!parent.exists(), "guard drop with TTL=0 should remove temp dir");
        set_inbox_ttl_secs_for_tests(DEFAULT_INBOX_TTL_SECS);
    }

    #[test]
    fn data_form_defers_cleanup_by_default() {
        let _g = TTL_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_inbox_ttl_secs_for_tests(DEFAULT_INBOX_TTL_SECS);
        // Default TTL is 5 minutes — file must still exist after guard drop
        // so the downstream agent can read it.
        let inb = inbox();
        let a = att_data("aGVsbG8=", Some("note.txt"), None);
        let (resolved, guard) = resolve(&[a], &[], inb.path()).unwrap();
        let materialized = PathBuf::from(&resolved[0].path);
        drop(guard);
        assert!(
            materialized.exists(),
            "file must outlive request-bound guard for downstream agent reads"
        );
    }

    #[test]
    fn data_form_rejects_invalid_base64() {
        let inb = inbox();
        let a = att_data("not!!!base64!!!", Some("x.jpg"), None);
        let err = resolve(&[a], &[], inb.path()).unwrap_err();
        assert!(err.contains("invalid base64"));
    }

    #[test]
    fn data_form_sanitizes_filename() {
        let inb = inbox();
        let a = att_data("aGk=", Some("../../etc/passwd"), None);
        let (resolved, _g) = resolve(&[a], &[], inb.path()).unwrap();
        let p = PathBuf::from(&resolved[0].path);
        assert!(p.starts_with(inb.path()), "must not escape inbox");
        let basename = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!basename.contains('/'));
        assert!(!basename.contains(".."));
    }

    #[test]
    fn data_form_no_temp_dir_when_only_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, b"hi").unwrap();
        let inb = inbox();
        let inbox_path = inb.path().to_path_buf();
        let (_resolved, _g) = resolve(&[att(path.to_str().unwrap())], &[], &inbox_path).unwrap();
        let entries: Vec<_> = std::fs::read_dir(&inbox_path).unwrap().collect();
        assert!(entries.is_empty(), "no temp dir created when no `data` attachments");
    }

    #[test]
    fn prefix_empty_when_no_attachments() {
        assert_eq!(prompt_prefix(&[]), "");
    }

    #[test]
    fn prefix_renders_lines() {
        let resolved = vec![Resolved {
            path: "/tmp/k.jpg".into(),
            display_name: "kitchen.jpg".into(),
            kind: "image".into(),
        }];
        let out = prompt_prefix(&resolved);
        assert!(out.contains("[attached image: kitchen.jpg -> /tmp/k.jpg]"));
        assert!(out.ends_with("\n\n"));
    }

    #[test]
    fn allow_rules_one_per_path() {
        let resolved = vec![
            Resolved { path: "/tmp/a.jpg".into(), display_name: "a.jpg".into(), kind: "image".into() },
            Resolved { path: "/tmp/b.pdf".into(), display_name: "b.pdf".into(), kind: "document".into() },
        ];
        let rules = allow_rules(&resolved);
        assert_eq!(rules, vec!["Read(/tmp/a.jpg)", "Read(/tmp/b.pdf)"]);
    }
}
