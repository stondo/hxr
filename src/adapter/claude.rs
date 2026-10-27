//! Claude Code adapter.
//!
//! Sessions are transcripts: `~/.claude/projects/<munged-cwd>/<session-uuid>.jsonl`.
//! Extraction per transcript (bounded I/O — never full-file scans):
//! - id: filename stem (uuid)
//! - cwd: first `cwd` field seen in the head of the file
//! - title: last `ai-title` entry (`aiTitle`) in the tail; fallback first
//!   user message text; final fallback `~/.claude/history.jsonl`
//!   first typed prompt for that sessionId
//! - updated: file mtime
//!
//! Resume: `claude -r <id>` from the session's original cwd.

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Claude;

impl Harness for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn binary(&self) -> &'static str {
        "claude"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("claude: home dir unavailable")?;
        let projects = home.join(".claude").join("projects");
        if !projects.is_dir() {
            anyhow::bail!("claude: {} is not a directory", projects.display());
        }

        // (transcript, project dir) pairs; skip non-jsonl entries (memory/, etc.)
        let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
        for proj in std::fs::read_dir(&projects)? {
            let Ok(proj) = proj else { continue };
            let proj_path = proj.path();
            if !proj_path.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&proj_path)? {
                let Ok(entry) = entry else { continue };
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push((path, proj_path.clone()));
                }
            }
        }

        let mut sessions: Vec<Session> = files
            .par_iter()
            .filter_map(|(file, proj)| transcribe(file, proj))
            .collect();

        // Title fallback pass: first typed prompt per session from history.jsonl.
        if let Ok(history) = load_history(&home) {
            for s in &mut sessions {
                if s.title.is_empty()
                    && let Some(title) = history.get(&s.id)
                {
                    s.title = title.clone();
                }
            }
        }
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec!["claude".to_string(), "-r".to_string(), session.id.clone()];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let path = session.extra.get("path")?;
        util::jsonl_content_match(std::path::Path::new(path), pat)
    }

    fn render(&self, session: &Session) -> Option<String> {
        let path = session.extra.get("path")?;
        util::render_jsonl(std::path::Path::new(path), util::extract_message_envelope)
    }
}

/// Extract one Session from a transcript with bounded reads
/// (head: 256 KiB for cwd + first user text; tail: 64 KiB for the last ai-title).
fn transcribe(path: &Path, proj_dir: &Path) -> Option<Session> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let updated_ms = util::mtime_ms(path);

    let head = util::read_jsonl_head(path, 256 * 1024);
    let mut cwd = head
        .iter()
        .filter_map(|v| v.get("cwd"))
        .filter_map(|c| c.as_str())
        .find(|c| !c.is_empty())
        .map(PathBuf::from);
    let title = last_ai_title(path).or_else(|| util::first_user_text(&head, "user"));

    #[cfg(unix)]
    if cwd.is_none() {
        cwd = demunge(proj_dir.file_name()?.to_str()?).into_iter().next();
    }
    let cwd = cwd?; // no cwd derivable at all -> unusable session

    let mut extra = std::collections::BTreeMap::new();
    extra.insert("path".to_string(), path.to_string_lossy().into_owned());

    Some(Session {
        id,
        harness: "claude".to_string(),
        cwd,
        title: title.unwrap_or_default(),
        updated_ms,
        extra,
    })
}

/// Last `{"type":"ai-title","aiTitle":...}` in the final 64 KiB of the file.
fn last_ai_title(path: &Path) -> Option<String> {
    util::read_jsonl_tail(path, 64 * 1024)
        .iter()
        .rev()
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("ai-title"))
        .filter_map(|v| v.get("aiTitle").and_then(|t| t.as_str()))
        .map(str::to_string)
        .next()
}

/// Demunge a claude project dir name back to plausible cwd candidates.
/// The munge maps `/`, `.` and `-` all to `-`, so `/a/b` and `/a.b` collide;
/// try candidates in order and let callers verify with `is_dir`.
#[cfg(unix)]
fn demunge(name: &str) -> Vec<PathBuf> {
    let slashed = format!("/{}", name.replace('-', "/"));
    let dotted = slashed.replace("//", "/.");
    match slashed == dotted {
        true => vec![PathBuf::from(slashed)],
        false => vec![PathBuf::from(slashed), PathBuf::from(dotted)],
    }
}

/// sessionId -> first typed prompt (skipping slash-commands), from history.jsonl.
fn load_history(home: &std::path::Path) -> Result<HashMap<String, String>> {
    let path = home.join(".claude").join("history.jsonl");
    let file = std::fs::File::open(&path)?;
    let mut titles: HashMap<String, String> = HashMap::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(id), Some(display)) = (
            v.get("sessionId").and_then(|s| s.as_str()),
            v.get("display").and_then(|s| s.as_str()),
        ) else {
            continue;
        };
        if display.starts_with('/') || display.trim().is_empty() {
            continue; // slash command
        }
        titles
            .entry(id.to_string())
            .or_insert_with(|| util::first_line_cut(display, 80));
    }
    Ok(titles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn demunge_candidates() {
        let c = demunge("home-user-PARA-Projects-myproj");
        assert_eq!(c[0], PathBuf::from("/home/user/PARA/Projects/myproj"));
        let dotted = demunge("home-user--dotdir-instances");
        assert!(dotted.contains(&PathBuf::from("/home/user/.dotdir/instances")));
    }
}
