//! Harness adapters: one implementation per coding-agent CLI.
//!
//! An adapter is a read-only view over one harness session store plus the
//! knowledge of how to resume one of its sessions. Adapters must be failure
//! isolated: a broken store yields an error (or zero sessions) and never
//! affects the other adapters.

pub mod claude;
pub mod codex;
pub mod copilot;
#[cfg(test)]
mod fixtures_test;
pub mod goose;
pub mod hermes;
pub mod kimi;
pub mod omp;
pub mod opencode;
pub mod pi;
pub mod qwen;
pub mod reasonix;
pub mod util;

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::model::Session;

pub trait Harness: Send + Sync {
    /// Stable lowercase harness name, also used as the tab name.
    fn name(&self) -> &'static str;

    /// The CLI binary this adapter resumes through (used for detection).
    fn binary(&self) -> &'static str;

    /// Read-only scan of the harness session store. Individual bad rows are
    /// skipped by the adapter; an `Err` means the whole store is unreadable.
    fn discover(&self) -> Result<Vec<Session>>;

    /// Build the resume command (argv including the binary, plus the working
    /// directory to run it in).
    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf);

    /// Content search (`-G`): return a snippet around the first match of
    /// `pattern` in this session's conversation bodies, or None when the
    /// session does not match (or the adapter has no content source).
    /// Implementations match against string *values* of parsed JSON, not raw
    /// lines, so JSON keys never false-positive.
    fn content_match(&self, _session: &Session, _pattern: &SearchPattern) -> Option<String> {
        None
    }

    /// Transcript rendering (`hxr show`): the session's conversation as
    /// `[role]
    /// text` blocks, or None when the adapter cannot render transcripts.
    fn render(&self, _session: &Session) -> Option<String> {
        None
    }

    /// Usage stats for the preview pane ((key, value) pairs, e.g. tokens).
    /// None when the adapter does not compute stats. Called only for the
    /// highlighted session, so implementations may do bounded extra I/O.
    fn stats(&self, _session: &Session) -> Option<std::collections::BTreeMap<String, String>> {
        None
    }
}

/// Adapter registry in tab order. `include_all` is `--all`: also list
/// child/subagent sessions where an adapter distinguishes them.
pub fn registry(include_all: bool) -> Vec<Box<dyn Harness>> {
    vec![
        Box::new(opencode::Opencode {
            include_children: include_all,
        }),
        Box::new(claude::Claude),
        Box::new(codex::Codex),
        Box::new(pi::Pi),
        Box::new(omp::Omp),
        Box::new(qwen::Qwen),
        Box::new(kimi::Kimi),
        Box::new(copilot::Copilot),
        Box::new(reasonix::Reasonix),
        Box::new(hermes::Hermes {
            include_children: include_all,
        }),
        Box::new(goose::Goose),
    ]
}

/// Root home for harness stores: `$HXR_HOME` (tests / sandboxes), `$HOME`,
/// or `$USERPROFILE` (Windows).
pub fn home_dir() -> Result<std::path::PathBuf> {
    std::env::var_os("HXR_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .context("hxr: neither HXR_HOME, HOME nor USERPROFILE is set")
}

/// A compiled content-search pattern (`-G`). Literal patterns (no regex
/// metacharacters) keep their plain form so sqlite-backed adapters can
/// prefilter with `LIKE` instead of streaming rows into Rust.
pub struct SearchPattern {
    pub re: regex::Regex,
    pub literal: Option<String>,
}

impl SearchPattern {
    pub fn new(pattern: &str) -> anyhow::Result<Self> {
        let literal = is_literal(pattern).then(|| pattern.to_string());
        let re = regex::Regex::new(pattern)
            .with_context(|| format!("hxr: bad --grep pattern {pattern:?}"))?;
        Ok(Self { re, literal })
    }

    /// First match in `haystack`, if any.
    pub fn find<'a>(&self, haystack: &'a str) -> Option<regex::Match<'a>> {
        self.re.find(haystack)
    }
}

fn is_literal(p: &str) -> bool {
    !p.chars().any(|c| "\\.+*?()|[]{}^$#&-~".contains(c))
}

impl SearchPattern {
    /// True when the pattern is a literal that JSON encoding leaves verbatim
    /// (printable ASCII, no quotes/backslashes), so a raw-line probe before
    /// JSON parsing cannot miss matches. Regex patterns and literals with
    /// escapable characters always go through full parsing.
    pub fn probe_safe(&self) -> bool {
        self.literal.as_deref().is_some_and(|l| {
            l.chars()
                .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
        })
    }
}

/// Short, single-line snippet around a match for display.
pub fn snippet(text: &str, m: std::ops::Range<usize>) -> String {
    let start = text[..m.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = text[m.end..]
        .find('\n')
        .map(|i| m.end + i)
        .unwrap_or(text.len());
    let line = text[start..end].trim();
    let mut out: String = line.chars().take(119).collect();
    if line.chars().count() > 120 {
        out.push('…');
    }
    out
}

/// True if `bin` resolves to an executable file on PATH.
pub fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = dir.join(bin);
            std::fs::metadata(&f)
                .map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
                .unwrap_or(false)
        }
        #[cfg(windows)]
        {
            // PATHEXT resolution: try the bare name, then common extensions
            ["", ".exe", ".cmd", ".bat", ".com"]
                .iter()
                .any(|ext| dir.join(format!("{bin}{ext}")).is_file())
        }
    })
}
