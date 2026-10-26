//! reasonix adapter.
//!
//! Sessions are named handles: `~/.reasonix/sessions/<name>.jsonl` with a
//! sibling `<name>.meta.json` carrying `summary`, `workspace`, `branch`,
//! `turnCount`, `totalCostUsd`. Archived sessions carry `__archive_<ts>`
//! suffixes and are skipped. Resume: `reasonix -r <path> --dir <workspace>`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::Harness;
use super::util;
use crate::model::Session;

pub struct Reasonix;

impl Harness for Reasonix {
    fn name(&self) -> &'static str {
        "reasonix"
    }

    fn binary(&self) -> &'static str {
        "reasonix"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("reasonix: home dir unavailable")?;
        let root = home.join(".reasonix").join("sessions");
        if !root.is_dir() {
            anyhow::bail!("reasonix: {} is not a directory", root.display());
        }

        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&root)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !stem.ends_with(".meta") || stem.contains("__archive_") {
                continue;
            }
            let name = stem.strip_suffix(".meta").unwrap_or(stem);
            if let Some(s) = transcribe(&path, name, &root) {
                sessions.push(s);
            }
        }
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let path = session.extra.get("path").cloned().unwrap_or_default();
        let mut argv = vec!["reasonix".to_string(), "-r".to_string(), path];
        if session.extra.contains_key("has_dir") {
            argv.push("--dir".to_string());
            argv.push(session.cwd.to_string_lossy().into_owned());
        }
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }
}

fn transcribe(meta_path: &Path, name: &str, root: &Path) -> Option<Session> {
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(meta_path).ok()?).ok()?;
    let summary = meta.get("summary").and_then(|s| s.as_str()).unwrap_or("");
    let workspace = meta
        .get("workspace")
        .and_then(|w| w.as_str())
        .filter(|w| !w.is_empty());

    let transcript = root.join(format!("{name}.jsonl"));
    let updated_ms = if transcript.is_file() {
        util::mtime_ms(&transcript)
    } else {
        util::mtime_ms(meta_path)
    };

    let mut extra = std::collections::BTreeMap::new();
    extra.insert(
        "path".to_string(),
        transcript.to_string_lossy().into_owned(),
    );
    for key in ["branch", "turnCount", "totalCostUsd"] {
        if let Some(v) = meta.get(key)
            && !v.is_null()
        {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) if key == "totalCostUsd" => n
                    .as_f64()
                    .map(|f| format!("{f:.4}"))
                    .unwrap_or_else(|| n.to_string()),
                other => other.to_string(),
            };
            extra.insert(key.to_string(), s);
        }
    }

    let cwd = workspace
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::adapter::home_dir().unwrap_or_default());
    if workspace.is_some() {
        extra.insert("has_dir".to_string(), "1".to_string());
    }

    Some(Session {
        id: name.to_string(),
        harness: "reasonix".to_string(),
        cwd,
        title: util::first_line_cut(summary, 80),
        updated_ms,
        extra,
    })
}
