//! Gemini CLI adapter (Google).
//!
//! Sessions are jsonl transcripts under `~/.gemini/tmp/<project>/chats/
//! session-<ts><hash>.jsonl`. Line 1 is a header (`sessionId` uuid,
//! `projectHash`, `startTime`/`lastUpdated`, `kind: "main"`); following
//! lines are chat entries `{id, timestamp, type: "user"|"assistant",
//! content: [{text}]}` interleaved with operator records `{"$set":
//! {"lastUpdated": …}}` and `{"$rewindTo": …}` on retried turns. The first
//! user entry is a `<session_context>` boilerplate dump (skipped, like
//! claude's command-caveats).
//!
//! The transcript records no cwd. `~/.gemini/projects.json` maps
//! cwd → project name; the session's cwd is recovered by inverting that
//! map over the parent directory name (counter-suffixed names like
//! `alpha-1` appear when two projects share a basename).
//!
//! Resume: `gemini --session-file <path>` from the original cwd (the
//! `--resume` flag only takes `latest` or a per-project index number —
//! not a session id — so the file path is the stable handle).
//!
//! Pitfalls encoded: aborted sessions (user prompt recorded, model reply
//! never completed) are real resume points — listed, not filtered.
//! Sessions with no cwd mapping (project moved / projects.json pruned)
//! fall back to home.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Gemini;

impl Harness for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn binary(&self) -> &'static str {
        "gemini"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("gemini: home dir unavailable")?;
        let root = home.join(".gemini");
        if !root.is_dir() {
            anyhow::bail!("gemini: {} is not a directory", root.display());
        }

        // cwd → project name, inverted for lookups by directory name
        let mut by_name: HashMap<String, PathBuf> = HashMap::new();
        if let Ok(raw) = std::fs::read_to_string(root.join("projects.json"))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw)
            && let Some(projects) = v.get("projects").and_then(|p| p.as_object())
        {
            for (cwd, name) in projects {
                if let Some(name) = name.as_str() {
                    by_name.insert(name.to_string(), PathBuf::from(cwd.as_str()));
                }
            }
        }

        let mut files: Vec<PathBuf> = Vec::new();
        let tmp = root.join("tmp");
        if let Ok(projects) = std::fs::read_dir(&tmp) {
            for proj in projects.flatten() {
                let chats = proj.path().join("chats");
                let Ok(entries) = std::fs::read_dir(&chats) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        files.push(path);
                    }
                }
            }
        }

        let sessions: Vec<Session> = files
            .par_iter()
            .filter_map(|p| transcribe(p, &home, &by_name))
            .collect();
        if sessions.is_empty() {
            anyhow::bail!("gemini: no session transcripts under {}", tmp.display());
        }
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let path = session.extra.get("path").cloned().unwrap_or_default();
        let mut argv = vec![
            "gemini".to_string(),
            "--skip-trust".to_string(),
            "--session-file".to_string(),
            path,
        ];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let path = session.extra.get("path")?;
        util::jsonl_content_match(Path::new(path), pat)
    }

    fn render(&self, session: &Session) -> Option<String> {
        let path = session.extra.get("path")?;
        util::render_jsonl(Path::new(path), extract_turn)
    }
}

/// One transcript → Session. Bounded head read: the header and the first
/// real user prompt live near the top; recency comes from file mtime
/// (the jsonl's own `$set.lastUpdated` tail is not worth a full scan).
fn transcribe(path: &Path, home: &Path, by_name: &HashMap<String, PathBuf>) -> Option<Session> {
    let head = util::read_jsonl_head(path, 64 * 1024);
    // the sessionId header is normally line 1, but rewind records can be
    // prepended on retried turns — scan for it
    let header = head
        .iter()
        .find(|v| v.get("sessionId").is_some())
        .or_else(|| head.first())?;
    let id = header
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })?;

    let project_dir = path.parent()?.parent()?; // …/tmp/<project>/chats/file
    let project_name = project_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cwd = by_name
        .get(&project_name)
        .cloned()
        .unwrap_or_else(|| home.to_path_buf());

    // first non-boilerplate user entry → title
    let mut title = String::new();
    for v in &head {
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        if let Some(text) = util::text_from_message(v) {
            let text = text.trim();
            if !text.is_empty() && !text.starts_with('<') {
                title = util::first_line_cut(text, 80);
                break;
            }
        }
    }

    let mut extra = BTreeMap::new();
    extra.insert("path".to_string(), path.to_string_lossy().into_owned());
    if let Some(model) = header.get("model").and_then(|m| m.as_str()) {
        extra.insert("model".to_string(), model.to_string());
    }

    Some(Session {
        id,
        harness: "gemini".to_string(),
        cwd,
        title,
        updated_ms: util::mtime_ms(path),
        extra,
    })
}

/// Chat entries carry `content` directly (no message envelope).
fn extract_turn(v: &serde_json::Value) -> Option<(String, String)> {
    let role = v.get("type")?.as_str()?.to_string();
    if !matches!(role.as_str(), "user" | "assistant") {
        return None;
    }
    let text = util::text_from_message(v)?;
    Some((role, text))
}
