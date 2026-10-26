//! GitHub Copilot CLI adapter.
//!
//! Sessions live as directories under `~/.copilot/session-state/<uuid>/`,
//! each with a flat `workspace.yaml` carrying id, cwd, git info, created /
//! updated timestamps and a name (the opening prompt). The sibling
//! `session.db` holds only todos and the logs are plain text, so this is a
//! listing-only adapter: no content search, no transcript rendering.
//!
//! Resume: `copilot -r <uuid>` from the session's cwd.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::Harness;
use crate::model::Session;

pub struct Copilot;

impl Harness for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn binary(&self) -> &'static str {
        "copilot"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("copilot: home dir unavailable")?;
        let root = home.join(".copilot").join("session-state");
        if !root.is_dir() {
            // binary installed but never used: normal state, not a warning
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&root)? {
            let Ok(entry) = entry else { continue };
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            if let Some(s) = transcribe(&dir) {
                sessions.push(s);
            }
        }
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec!["copilot".to_string(), "-r".to_string(), session.id.clone()];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }
}

/// Parse one session dir from its workspace.yaml (flat `key: value` lines;
/// the name field may be long but stays single-line in practice).
fn transcribe(dir: &Path) -> Option<Session> {
    let yaml = std::fs::read_to_string(dir.join("workspace.yaml")).ok()?;
    let mut id = dir.file_name()?.to_string_lossy().into_owned();
    let mut cwd = None;
    let mut title = String::new();
    let mut updated = None;
    for line in yaml.lines() {
        let (key, value) = line.split_once(':')?;
        let value = value.trim();
        match key.trim() {
            "id" if !value.is_empty() => id = value.to_string(),
            "cwd" if !value.is_empty() => cwd = Some(PathBuf::from(value)),
            "name" | "summary" if !value.is_empty() && title.is_empty() => {
                title = value.to_string();
            }
            "updated_at" if !value.is_empty() => {
                updated = value
                    .parse::<jiff::Timestamp>()
                    .ok()
                    .map(|t| t.as_millisecond());
            }
            _ => {}
        }
    }
    let cwd = cwd?;
    Some(Session {
        id,
        harness: "copilot".to_string(),
        cwd,
        title: super::util::first_line_cut(&title, 80),
        updated_ms: updated.unwrap_or_else(|| super::util::mtime_ms(dir)),
        extra: Default::default(),
    })
}
