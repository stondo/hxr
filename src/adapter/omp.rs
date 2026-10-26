//! omp (oh-my-pi) adapter.
//!
//! Primary index: `~/.omp/agent/history.db` (SQLite):
//! - `session_titles(session_id, title, updated_at)` — AI titles
//! - `history(id, prompt, created_at, cwd, session_id, use_count)` — one row
//!   per prompt; folded per session into first prompt (title fallback) and
//!   latest cwd/created_at
//!
//! omp's `sessions/<munged>/` dir names are unreliable munges and inner files
//! are mostly subagent transcripts — everything comes from the db.
//! Resume: `omp -r <session-id-prefix>` from the session cwd.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Omp;

/// Per-session fold of the history table (rows arrive oldest-first).
#[derive(Default)]
struct Hist {
    first_prompt: Option<String>,
    cwd: Option<String>,
    last_created: i64,
}

impl Harness for Omp {
    fn name(&self) -> &'static str {
        "omp"
    }

    fn binary(&self) -> &'static str {
        "omp"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("omp: home dir unavailable")?;
        let db = home.join(".omp").join("agent").join("history.db");
        if !db.is_file() {
            anyhow::bail!("omp: {} is not a file", db.display());
        }
        let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("omp: opening {}", db.display()))?;

        // session_id -> (title, updated_at)
        let mut titles: HashMap<String, (String, i64)> = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT session_id, title, updated_at FROM session_titles")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows.flatten() {
                titles.insert(row.0, (row.1, row.2));
            }
        }

        // session_id -> fold(prompt/cwd/created_at), oldest-first input order
        let mut hist: HashMap<String, Hist> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT session_id, cwd, prompt, created_at FROM history \
                 WHERE session_id IS NOT NULL ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for (sid, cwd, prompt, created) in rows.flatten() {
                let h = hist.entry(sid).or_default();
                if h.first_prompt.is_none() && !prompt.starts_with('/') {
                    h.first_prompt = Some(prompt);
                }
                if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
                    h.cwd = Some(cwd); // latest wins (rows are oldest-first)
                }
                h.last_created = h.last_created.max(created);
            }
        }

        // Sessions come from history rows (they carry the cwd); titles upgrade
        // the display when an AI title exists. Title-only sessions without
        // history rows have no derivable cwd and are skipped.
        let mut sessions = Vec::new();
        for (id, h) in hist {
            let Some(cwd) = h.cwd else { continue };
            let (title, updated_ms) = match titles.get(&id) {
                Some((t, ts)) => (t.clone(), ts * 1000),
                None => (
                    h.first_prompt.clone().unwrap_or_default(),
                    h.last_created * 1000,
                ),
            };
            let mut extra = std::collections::BTreeMap::new();
            if let Some(p) = &h.first_prompt {
                extra.insert("first_prompt".to_string(), p.clone());
            }
            sessions.push(Session {
                id: id.clone(),
                harness: "omp".to_string(),
                cwd: PathBuf::from(cwd),
                title: util::first_line_cut(&title, 80),
                updated_ms,
                extra,
            });
        }
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec!["omp".to_string(), "-r".to_string(), session.id.clone()];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let home = super::home_dir().ok()?;
        let db = home.join(".omp").join("agent").join("history.db");
        let conn =
            Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let like = pat
            .literal
            .as_ref()
            .map(|l| {
                format!(
                    "%{}%",
                    l.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                )
            })
            .unwrap_or_else(|| "%".to_string());
        let mut stmt = conn
            .prepare("SELECT prompt FROM history WHERE session_id = ?1 AND prompt LIKE ?2")
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session.id, like], |r| {
                r.get::<_, String>(0)
            })
            .ok()?;
        for prompt in rows.flatten() {
            if let Some(m) = pat.find(&prompt) {
                return Some(super::snippet(&prompt, m.range()));
            }
        }
        None
    }
}
