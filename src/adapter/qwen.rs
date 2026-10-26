//! Qwen Code adapter.
//!
//! History lives in `~/.qwen/projects/<munged-cwd>/chats/<uuid>.jsonl`
//! (`~/.qwen/sessions/` is a decoy: runtime pid locks, not history).
//! Line 1 is a user turn carrying `sessionId`, `cwd`, `timestamp` and
//! `gitBranch`; the message body uses the gemini `parts` shape. Resume:
//! `qwen -r <id>` from the original cwd.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Qwen;

impl Harness for Qwen {
    fn name(&self) -> &'static str {
        "qwen"
    }

    fn binary(&self) -> &'static str {
        "qwen"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("qwen: home dir unavailable")?;
        let root = home.join(".qwen").join("projects");
        if !root.is_dir() {
            anyhow::bail!("qwen: {} is not a directory", root.display());
        }

        let mut files: Vec<PathBuf> = Vec::new();
        for proj in std::fs::read_dir(&root)? {
            let Ok(proj) = proj else { continue };
            let chats = proj.path().join("chats");
            if !chats.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&chats)? {
                let Ok(entry) = entry else { continue };
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }

        Ok(files.par_iter().filter_map(|p| transcribe(p)).collect())
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec!["qwen".to_string(), "-r".to_string(), session.id.clone()];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let path = session.extra.get("path")?;
        util::jsonl_content_match(std::path::Path::new(path), pat)
    }
}

fn transcribe(path: &Path) -> Option<Session> {
    let head = util::read_jsonl_head(path, 64 * 1024);
    let first = head.first()?;

    let id = first
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    let cwd = head
        .iter()
        .filter_map(|v| v.get("cwd"))
        .filter_map(|c| c.as_str())
        .find(|c| !c.is_empty())
        .map(PathBuf::from)?;

    let mut extra = std::collections::BTreeMap::new();
    extra.insert("path".to_string(), path.to_string_lossy().into_owned());
    if let Some(branch) = first.get("gitBranch").and_then(|b| b.as_str()) {
        extra.insert("branch".to_string(), branch.to_string());
    }

    Some(Session {
        id,
        harness: "qwen".to_string(),
        cwd,
        title: util::first_user_text(&head, "user").unwrap_or_default(),
        updated_ms: util::mtime_ms(path),
        extra,
    })
}
