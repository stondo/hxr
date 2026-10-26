//! Codex adapter.
//!
//! Primary index: newest `~/.codex/state_*.sqlite` → `threads` table, which
//! already carries id, cwd, title and millisecond timestamps. Explicit column
//! selects only — rows embed full conversation text and must never be `SELECT *`-ed.
//! Fallback when no state db exists: `~/.codex/session_index.jsonl`
//! (id + thread_name + updated_at, no cwd).
//!
//! Resume: `codex resume <uuid>` — codex restores the recorded cwd itself.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Codex;

impl Harness for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn binary(&self) -> &'static str {
        "codex"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("codex: home dir unavailable")?;
        let root = home.join(".codex");
        if !root.is_dir() {
            anyhow::bail!("codex: {} is not a directory", root.display());
        }

        if let Some(db) = newest_state_db(&root) {
            return from_state_db(&db);
        }
        from_session_index(&root)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec![
            "codex".to_string(),
            "resume".to_string(),
            session.id.clone(),
        ];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let path = session.extra.get("path")?;
        util::jsonl_content_match(std::path::Path::new(path), pat)
    }

    fn render(&self, session: &Session) -> Option<String> {
        let path = session.extra.get("path")?;
        util::render_jsonl(std::path::Path::new(path), |v| {
            if v.get("type").and_then(|t| t.as_str()) != Some("response_item") {
                return None;
            }
            let payload = v.get("payload")?;
            if payload.get("type").and_then(|t| t.as_str()) != Some("message") {
                return None;
            }
            let role = payload.get("role")?.as_str()?.to_string();
            let text = payload
                .get("content")?
                .as_array()?
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            Some((role, text))
        })
    }
}

/// Highest-versioned `state_*.sqlite` under the codex root.
fn newest_state_db(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name.strip_suffix(".sqlite") else {
            continue;
        };
        let Some(ver) = stem.strip_prefix("state_") else {
            continue;
        };
        if let Ok(v) = ver.parse::<u64>()
            && best.as_ref().map(|(b, _)| v > *b).unwrap_or(true)
        {
            best = Some((v, entry.path()));
        }
    }
    best.map(|(_, p)| p)
}

fn from_state_db(db: &Path) -> Result<Vec<Session>> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("codex: opening {}", db.display()))?;

    let mut stmt = conn.prepare(
        "SELECT id, cwd, coalesce(nullif(title, ''), substr(first_user_message, 1, 120)), \
         coalesce(updated_at_ms, updated_at * 1000, recency_at * 1000, created_at * 1000), \
         substr(first_user_message, 1, 600), model, git_branch, rollout_path, tokens_used \
         FROM threads WHERE coalesce(archived, 0) = 0",
    )?;

    let rows = stmt.query_map([], |r| {
        let id: String = r.get(0)?;
        let cwd: String = r.get(1)?;
        let title: Option<String> = r.get(2)?;
        let updated: i64 = r.get(3)?;
        let first_msg: Option<String> = r.get(4)?;
        let model: Option<String> = r.get(5)?;
        let branch: Option<String> = r.get(6)?;
        let rollout: Option<String> = r.get(7)?;
        let tokens: Option<i64> = r.get(8)?;
        Ok((
            id, cwd, title, updated, first_msg, model, branch, rollout, tokens,
        ))
    })?;

    let mut sessions = Vec::new();
    for row in rows {
        let Ok((id, cwd, title, updated_ms, first_msg, model, branch, rollout, tokens)) = row
        else {
            continue;
        };
        let mut extra = std::collections::BTreeMap::new();
        if let Some(m) = first_msg {
            extra.insert("first_user_message".to_string(), m);
        }
        if let Some(m) = model {
            extra.insert("model".to_string(), m);
        }
        if let Some(b) = branch {
            extra.insert("branch".to_string(), b);
        }
        if let Some(p) = rollout {
            extra.insert("path".to_string(), p);
        }
        if let Some(t) = tokens
            && t > 0
        {
            extra.insert("tokens".to_string(), t.to_string());
        }
        sessions.push(Session {
            id,
            harness: "codex".to_string(),
            cwd: PathBuf::from(cwd),
            // threads.title occasionally stores a whole pasted message — cap it
            title: util::first_line_cut(&title.unwrap_or_default(), 80),
            updated_ms,
            extra,
        });
    }
    Ok(sessions)
}

/// Fallback: session_index.jsonl lines {"id","thread_name","updated_at"(RFC3339)}.
fn from_session_index(root: &Path) -> Result<Vec<Session>> {
    let path = root.join("session_index.jsonl");
    let file = std::fs::File::open(&path)
        .with_context(|| format!("codex: no state db and no {}", path.display()))?;
    let home = super::home_dir().unwrap_or_default();
    let mut sessions = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(id), Some(title), Some(updated)) = (
            v.get("id").and_then(|s| s.as_str()),
            v.get("thread_name").and_then(|s| s.as_str()),
            v.get("updated_at").and_then(|s| s.as_str()),
        ) else {
            continue;
        };
        let updated_ms = updated
            .parse::<jiff::Timestamp>()
            .map(|t| t.as_millisecond())
            .unwrap_or(0);
        sessions.push(Session {
            id: id.to_string(),
            harness: "codex".to_string(),
            cwd: home.clone(), // codex resume restores the real cwd itself
            title: title.to_string(),
            updated_ms,
            extra: Default::default(),
        });
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_db_version_pick() {
        let dir = std::env::temp_dir().join("hxr-test-codex");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "state_4.sqlite",
            "state_10.sqlite",
            "state_9.sqlite",
            "state_x.sqlite",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let picked = newest_state_db(&dir).unwrap();
        assert!(picked.ends_with("state_10.sqlite"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
