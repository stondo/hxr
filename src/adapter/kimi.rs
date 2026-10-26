//! Kimi (kimi-code CLI) adapter.
//!
//! Index: `~/.kimi-code/session_index.jsonl` with one line per session:
//! `{"sessionId","sessionDir","workDir"}` (id + cwd, no timestamps).
//! Transcripts are event streams at `<sessionDir>/agents/main/wire.jsonl`:
//! the first `turn.prompt` entry carries the first user text and the first
//! `llm.request` the model. Recency = wire mtime.
//!
//! Resume: `kimi -S <session_<uuid>>` from the session's workDir.

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Kimi;

impl Harness for Kimi {
    fn name(&self) -> &'static str {
        "kimi"
    }

    fn binary(&self) -> &'static str {
        "kimi"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("kimi: home dir unavailable")?;
        let root = home.join(".kimi-code");
        if !root.is_dir() {
            anyhow::bail!("kimi: {} is not a directory", root.display());
        }
        let index = root.join("session_index.jsonl");
        let file = std::fs::File::open(&index)
            .with_context(|| format!("kimi: no session index at {}", index.display()))?;

        let mut entries: Vec<(String, PathBuf, PathBuf)> = Vec::new();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let (Some(id), Some(dir), Some(work_dir)) = (
                v.get("sessionId").and_then(|s| s.as_str()),
                v.get("sessionDir").and_then(|s| s.as_str()),
                v.get("workDir").and_then(|s| s.as_str()),
            ) else {
                continue;
            };
            entries.push((id.to_string(), PathBuf::from(dir), PathBuf::from(work_dir)));
        }

        Ok(entries
            .par_iter()
            .filter_map(|(id, dir, work_dir)| transcribe(id, dir, work_dir))
            .collect())
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec!["kimi".to_string(), "-S".to_string(), session.id.clone()];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let path = session.extra.get("path")?;
        util::jsonl_content_match(std::path::Path::new(path), pat)
    }
}

fn transcribe(id: &str, session_dir: &Path, work_dir: &Path) -> Option<Session> {
    let wire = session_dir.join("agents").join("main").join("wire.jsonl");
    if !wire.is_file() {
        return None; // index entry without transcript (migrated/cleaned)
    }
    let updated_ms = util::mtime_ms(&wire);

    let head = util::read_jsonl_head(&wire, 64 * 1024);
    let title = head
        .iter()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("turn.prompt"))
        .find_map(|v| {
            v.get("input")?
                .as_array()?
                .iter()
                .find_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
        })
        .map(|t| util::first_line_cut(&t, 80))
        .unwrap_or_default();
    let model = head
        .iter()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("llm.request"))
        .find_map(|v| v.get("model").and_then(|m| m.as_str()))
        .map(str::to_string);

    let mut extra = std::collections::BTreeMap::new();
    extra.insert("path".to_string(), wire.to_string_lossy().into_owned());
    if let Some(m) = model {
        extra.insert("model".to_string(), m);
    }

    Some(Session {
        id: id.to_string(),
        harness: "kimi".to_string(),
        cwd: work_dir.to_path_buf(),
        title,
        updated_ms,
        extra,
    })
}
