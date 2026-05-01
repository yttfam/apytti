//! Attachment handling for `/api/ask`. Pyttch-bridge (and any other caller)
//! can pass `attachments[]` instead of munging file paths into the prompt
//! itself. Apytti validates each path, optionally enforces a config-level
//! whitelist of allowed roots, and lets the claude backend mint per-call
//! `--allowedTools Read(<path>)` rules so the CLI can read the file without
//! `--dangerously-skip-permissions`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Attachment {
    /// Resolve `kind`: explicit when set, otherwise infer from extension.
    pub fn effective_kind(&self) -> &str {
        if let Some(k) = self.kind.as_deref() {
            return k;
        }
        infer_kind_from_path(&self.path)
    }

    /// Display name: explicit `name`, else the file's basename.
    pub fn display_name(&self) -> String {
        if let Some(n) = self.name.as_deref() {
            return n.to_string();
        }
        Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone())
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

/// Validate one attachment. Returns Err with a human-readable reason on failure.
/// `roots` empty means "no whitelist enforcement".
pub fn validate(att: &Attachment, roots: &[String]) -> Result<(), String> {
    let p = PathBuf::from(&att.path);
    if !p.is_absolute() {
        return Err(format!("attachment path must be absolute: {}", att.path));
    }

    let meta = std::fs::metadata(&p)
        .map_err(|e| format!("attachment not accessible: {} ({e})", att.path))?;
    if !meta.is_file() {
        return Err(format!("attachment is not a regular file: {}", att.path));
    }

    if !roots.is_empty() {
        // Best-effort canonicalize; fall back to the provided path.
        let canonical = std::fs::canonicalize(&p).unwrap_or(p.clone());
        let allowed = roots.iter().any(|r| {
            let root_canon = std::fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r));
            canonical.starts_with(&root_canon)
        });
        if !allowed {
            return Err(format!(
                "attachment path {} is outside allowed roots",
                att.path
            ));
        }
    }
    Ok(())
}

/// Build the prompt prefix that gets prepended to the user's prompt for
/// CLI-wrapped backends. Empty string when no attachments.
pub fn prompt_prefix(attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for att in attachments {
        out.push_str(&format!(
            "[attached {}: {} -> {}]\n",
            att.effective_kind(),
            att.display_name(),
            att.path
        ));
    }
    out.push('\n');
    out
}

/// `Read(<path>)` allow rules, one per attachment, for `--allowedTools`.
pub fn allow_rules(attachments: &[Attachment]) -> Vec<String> {
    attachments
        .iter()
        .map(|a| format!("Read({})", a.path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(path: &str) -> Attachment {
        Attachment {
            path: path.into(),
            kind: None,
            name: None,
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
            path: "/tmp/a.jpg".into(),
            kind: Some("document".into()),
            name: None,
        };
        assert_eq!(a.effective_kind(), "document");
    }

    #[test]
    fn display_name_falls_back_to_basename() {
        assert_eq!(att("/tmp/foo/bar.jpg").display_name(), "bar.jpg");
        let a = Attachment {
            path: "/tmp/foo/bar.jpg".into(),
            kind: None,
            name: Some("kitchen.jpg".into()),
        };
        assert_eq!(a.display_name(), "kitchen.jpg");
    }

    #[test]
    fn rejects_relative_path() {
        let err = validate(&att("relative.jpg"), &[]).unwrap_err();
        assert!(err.contains("absolute"));
    }

    #[test]
    fn rejects_missing_file() {
        let err = validate(&att("/tmp/__apytti_does_not_exist_xyz"), &[]).unwrap_err();
        assert!(err.contains("not accessible"));
    }

    #[test]
    fn accepts_real_file_no_roots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, b"hi").unwrap();
        let a = att(path.to_str().unwrap());
        assert!(validate(&a, &[]).is_ok());
    }

    #[test]
    fn enforces_roots_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, b"hi").unwrap();
        let a = att(path.to_str().unwrap());

        // Outside roots -> fail
        let err = validate(&a, &["/var/lib/apytti".into()]).unwrap_err();
        assert!(err.contains("outside allowed roots"));

        // Inside roots -> pass
        let root = dir.path().to_str().unwrap().to_string();
        assert!(validate(&a, &[root]).is_ok());
    }

    #[test]
    fn rejects_non_regular_file() {
        // /tmp itself is a directory.
        let err = validate(&att("/tmp"), &[]).unwrap_err();
        assert!(err.contains("not a regular file"));
    }

    #[test]
    fn prefix_empty_when_no_attachments() {
        assert_eq!(prompt_prefix(&[]), "");
    }

    #[test]
    fn prefix_renders_lines() {
        let a = Attachment {
            path: "/tmp/k.jpg".into(),
            kind: None,
            name: Some("kitchen.jpg".into()),
        };
        let out = prompt_prefix(&[a]);
        assert!(out.contains("[attached image: kitchen.jpg -> /tmp/k.jpg]"));
        assert!(out.ends_with("\n\n"));
    }

    #[test]
    fn allow_rules_one_per_path() {
        let a1 = att("/tmp/a.jpg");
        let a2 = att("/tmp/b.pdf");
        let rules = allow_rules(&[a1, a2]);
        assert_eq!(rules, vec!["Read(/tmp/a.jpg)", "Read(/tmp/b.pdf)"]);
    }
}
