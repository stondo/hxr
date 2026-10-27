//! aider adapter.
//!
//! aider keeps no session directory: each project accumulates a markdown
//! chat log at `<project>/.aider.chat.history.md` (plus an
//! `.aider.input.history` readline dump), with sessions appended and
//! delimited by `# aider chat started at <local timestamp>` header lines.
//! Inside a block, `#### <prompt>` lines are user turns, bare text is the
//! assistant reply (model reasoning arrives wrapped in
//! `<thinking-content-…>` tags), and `> `-prefixed lines are the aider UI
//! log — including the useful `> Model: …` and `> Tokens: N sent, M
//! received.` records. A global fallback log lives at
//! `~/.aider/chat-history.md` for runs outside any project.
//!
//! Resume contract: aider has **no session-id resume**. `--restore-chat-
//! history` reloads the newest chat from the default per-project file —
//! every session listed for a project therefore resumes the same way
//! (continue-latest-in-project mapping, as agreed in issue #2). Block ids
//! are `#N` suffixes on the history-file path, only for listing and
//! `-G` scoping.
//!
//! Pitfalls encoded: timestamps in the delimiters are *local* naive
//! strings — recency comes from the file mtime instead (all blocks of a
//! file share it). Discovery is a bounded home walk for the well-known
//! filename (depth-capped, noisy dirs skipped) — aider gives no index.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

/// Chat-log filename aider writes into the project root.
const HISTORY: &str = ".aider.chat.history.md";
/// Home-walk depth cap (project roots like ~/PARA/Projects/x sit at ≤4).
const MAX_DEPTH: usize = 6;

pub struct Aider;

impl Harness for Aider {
    fn name(&self) -> &'static str {
        "aider"
    }

    fn binary(&self) -> &'static str {
        "aider"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("aider: home dir unavailable")?;

        let mut files = vec![home.join(".aider").join("chat-history.md")];
        walk(&home, 0, &mut files);
        files.retain(|p| p.is_file());

        let sessions: Vec<Session> = files.par_iter().flat_map(|p| transcribe(p)).collect();
        if sessions.is_empty() {
            anyhow::bail!("aider: no {} files under {}", HISTORY, home.display());
        }
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec!["aider".to_string(), "--restore-chat-history".to_string()];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let path = session.extra.get("path").map(PathBuf::from)?;
        let raw = std::fs::read_to_string(path).ok()?;
        let block_no: usize = session
            .extra
            .get("block")
            .and_then(|b| b.parse().ok())
            .unwrap_or(0);
        let blocks = split_blocks(&raw);
        let block = blocks.get(block_no)?;
        for line in block.lines() {
            if let Some(m) = pat.find(line) {
                return Some(super::snippet(line, m.range()));
            }
        }
        None
    }

    fn render(&self, session: &Session) -> Option<String> {
        let path = session.extra.get("path").map(PathBuf::from)?;
        let raw = std::fs::read_to_string(path).ok()?;
        let block_no: usize = session
            .extra
            .get("block")
            .and_then(|b| b.parse().ok())
            .unwrap_or(0);
        let block = split_blocks(&raw).get(block_no)?.to_string();
        render_block(&block)
    }
}

/// Depth-capped walk collecting `HISTORY` files, skipping the usual
/// noisy/hidden trees (a project root is never hidden itself in practice).
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = path.is_dir();
        if is_dir {
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            walk(&path, depth + 1, out);
        } else if name == HISTORY {
            out.push(path);
        }
    }
}

/// One history file → one Session per `# aider chat started` block.
fn transcribe(path: &Path) -> Vec<Session> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let cwd = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mtime = util::mtime_ms(path);
    let blocks = split_blocks(&raw);

    blocks
        .iter()
        .enumerate()
        .filter_map(|(n, block)| {
            let title = block
                .lines()
                .find_map(|l| l.strip_prefix("#### "))
                .map(|t| util::first_line_cut(t.trim(), 80))
                .unwrap_or_default();
            if title.is_empty() {
                return None; // no user prompt recorded — not a session
            }
            let mut extra = BTreeMap::new();
            extra.insert("path".to_string(), path.to_string_lossy().into_owned());
            extra.insert("block".to_string(), n.to_string());
            if let Some(model) = block.lines().find_map(|l| l.strip_prefix("> Model: ")) {
                extra.insert(
                    "model".to_string(),
                    model.split_whitespace().next().unwrap_or(model).to_string(),
                );
            }
            if let Some(tokens) = block
                .lines()
                .rev()
                .find_map(|l| l.strip_prefix("> Tokens: "))
                .and_then(parse_tokens)
            {
                extra.insert("tokens".to_string(), tokens);
            }
            Some(Session {
                id: format!("{}#{n}", path.to_string_lossy()),
                harness: "aider".to_string(),
                cwd: cwd.clone(),
                title,
                updated_ms: mtime,
                extra,
            })
        })
        .collect()
}

fn split_blocks(raw: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut start = 0usize;
    let mut started = false;
    for (i, line) in raw.lines().enumerate() {
        if line.starts_with("# aider chat started at ") {
            if started {
                let byte_i = line_ptr(raw, i);
                blocks.push(&raw[start..byte_i]);
            }
            started = true;
            start = line_ptr(raw, i);
        }
    }
    if started {
        blocks.push(&raw[start..]);
    }
    if blocks.is_empty() {
        blocks.push(raw); // headerless log — one block
    }
    blocks
}

/// byte offset of line `n` (lines() offsets: rebuild by walking once).
fn line_ptr(raw: &str, want: usize) -> usize {
    let mut off = 0usize;
    for (i, line) in raw.split_inclusive('\n').enumerate() {
        if i == want {
            return off;
        }
        off += line.len();
    }
    off
}

/// `> Tokens: 588 sent, 139 received.` → `588 + 139`.
fn parse_tokens(t: &str) -> Option<String> {
    let mut nums = t
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty());
    let sent: i64 = nums.next()?.parse().ok()?;
    let recv: i64 = nums.next()?.parse().ok()?;
    Some(format!("{sent} + {recv}"))
}

/// Markdown block → `[role] text` transcript: `####` prompts are user
/// turns; bare lines (not `> `-UI log, not the delimiter header) are the
/// assistant reply. Thinking wrappers are kept as assistant text (capped).
fn render_block(block: &str) -> Option<String> {
    let mut out = String::new();
    let mut reply = String::new();
    for line in block.lines() {
        if let Some(prompt) = line.strip_prefix("#### ") {
            if !reply.trim().is_empty() {
                push_turn(&mut out, "assistant", &reply);
                reply.clear();
            }
            push_turn(&mut out, "user", prompt);
        } else if line.starts_with("# aider chat started")
            || line.starts_with("> ")
            || line.trim().is_empty()
        {
            continue;
        } else {
            reply.push_str(line);
            reply.push('\n');
        }
    }
    if !reply.trim().is_empty() {
        push_turn(&mut out, "assistant", &reply);
    }
    (!out.is_empty()).then_some(out)
}

fn push_turn(out: &mut String, role: &str, text: &str) {
    let text = text.trim();
    let text = if text.chars().count() > 2000 {
        let mut t: String = text.chars().take(1999).collect();
        t.push('…');
        t
    } else {
        text.to_string()
    };
    out.push_str(&format!("[{role}]\n{text}\n\n"));
}
