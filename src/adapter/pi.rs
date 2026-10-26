//! pi (pi-coding-agent) adapter.
//!
//! Sessions: `~/.pi/agent/sessions/<munged-cwd>/<ISO-ts>_<uuid>.jsonl`.
//! Line 1 is the session meta (`{"type":"session","id","timestamp","cwd"}`).
//! No native titles — derive from the first user message. Resume:
//! `pi --session <path>` (full path is unambiguous) from the session cwd.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Pi;

impl Harness for Pi {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn binary(&self) -> &'static str {
        "pi"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("pi: home dir unavailable")?;
        let root = home.join(".pi").join("agent").join("sessions");
        if !root.is_dir() {
            anyhow::bail!("pi: {} is not a directory", root.display());
        }

        let mut files: Vec<PathBuf> = Vec::new();
        for proj in std::fs::read_dir(&root)? {
            let Ok(proj) = proj else { continue };
            let dir = proj.path();
            if !dir.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&dir)? {
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
        let path = session
            .extra
            .get("path")
            .cloned()
            .unwrap_or_else(|| session.id.clone());
        let mut argv = vec!["pi".to_string(), "--session".to_string(), path];
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
    let meta = head
        .iter()
        .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("session"))?;
    let id = meta
        .get("id")
        .and_then(|i| i.as_str())
        .map(str::to_string)
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    let cwd = meta
        .get("cwd")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)?;

    let mut extra = std::collections::BTreeMap::new();
    extra.insert("path".to_string(), path.to_string_lossy().into_owned());

    Some(Session {
        id,
        harness: "pi".to_string(),
        cwd,
        title: util::first_user_text(&head, "message").unwrap_or_default(),
        updated_ms: util::mtime_ms(path),
        extra,
    })
}
