//! Crush adapter (charmbracelet).
//!
//! Crush keeps a per-project sqlite store: `~/.local/share/crush/
//! projects.json` registers every project (`path` + `data_dir` +
//! `last_accessed`), and `<data_dir>/crush.db` (by default `<project>/.crush/`)
//! holds `sessions` (uuid id, title, token/cost columns, epoch-second
//! timestamps, `parent_session_id` for subagent children) and `messages`
//! (role, `parts` = typed JSON part arrays: `text`, `reasoning`, `tool_call`,
//! `tool_result`, `finish`).
//!
//! Resume: `crush -s <id>` from the project path (`-s`/`--session` also
//! exists on `crush run` for headless continuation).
//!
//! Pitfalls encoded: session titles are lazily assigned ("Untitled
//! Session" until the TUI titles them) — the first user `text` part is the
//! reliable title. `finish`/`tool_*` parts carry no prose; only `text`
//! parts render. Timestamps are epoch **seconds**. A failed run still
//! registers the project in projects.json and creates the db — empty
//! stores are skipped, error-only sessions stay listed.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Crush {
    /// `--all`: also list child sessions (parent_session_id set).
    pub include_children: bool,
}

impl Harness for Crush {
    fn name(&self) -> &'static str {
        "crush"
    }

    fn binary(&self) -> &'static str {
        "crush"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("crush: home dir unavailable")?;
        let index = home.join(".local/share/crush/projects.json");
        let raw = std::fs::read_to_string(&index)
            .with_context(|| format!("crush: reading {}", index.display()))?;
        let v: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("crush: parsing {}", index.display()))?;
        let projects = v
            .get("projects")
            .and_then(|p| p.as_array())
            .context("crush: projects.json has no projects array")?;

        let mut sessions = Vec::new();
        for proj in projects {
            let (Some(path), Some(data_dir)) = (
                proj.get("path").and_then(|p| p.as_str()),
                proj.get("data_dir").and_then(|d| d.as_str()),
            ) else {
                continue;
            };
            let db = PathBuf::from(data_dir).join("crush.db");
            let Ok(conn) =
                Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            else {
                continue; // registered project without a store yet
            };
            let mut found = collect(&conn, path, self.include_children).unwrap_or_default();
            sessions.append(&mut found);
        }

        if sessions.is_empty() {
            anyhow::bail!("crush: no sessions in any registered project");
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec!["crush".to_string(), "-s".to_string(), session.id.clone()];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let db = session.extra.get("db").map(PathBuf::from)?;
        let conn =
            Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        // literal patterns prefilter with LIKE; regex patterns wildcard
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
            .prepare(
                "SELECT parts FROM messages \
                 WHERE session_id = ?1 AND parts LIKE ?2 ORDER BY created_at, id",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session.id, like], |r| {
                r.get::<_, String>(0)
            })
            .ok()?;
        for parts in rows.flatten() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&parts) else {
                continue;
            };
            let mut values = Vec::new();
            util::json_string_values(&v, &mut values);
            for value in values {
                if let Some(m) = pat.find(value) {
                    return Some(super::snippet(value, m.range()));
                }
            }
        }
        None
    }

    fn render(&self, session: &Session) -> Option<String> {
        let db = session.extra.get("db").map(PathBuf::from)?;
        let conn =
            Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT role, parts FROM messages \
                 WHERE session_id = ?1 ORDER BY created_at, id",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .ok()?;
        let mut out = String::new();
        for (role, parts_json) in rows.flatten() {
            let Ok(parts) = serde_json::from_str::<serde_json::Value>(&parts_json) else {
                continue;
            };
            let Some(items) = parts.as_array() else {
                continue;
            };
            for part in items {
                if part.get("type").and_then(|t| t.as_str()) != Some("text") {
                    continue;
                }
                let Some(text) = part
                    .pointer("/data/text")
                    .and_then(|t| t.as_str())
                    .map(str::trim)
                else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let text = if text.chars().count() > 2000 {
                    let mut t: String = text.chars().take(1999).collect();
                    t.push('…');
                    t
                } else {
                    text.to_string()
                };
                out.push_str(&format!("[{role}]\n{text}\n\n"));
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

/// One project's crush.db → sessions (cwd = the project path).
fn collect(conn: &Connection, cwd: &str, include_children: bool) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, parent_session_id, message_count, \
                prompt_tokens, completion_tokens, cost, updated_at \
         FROM sessions",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<f64>>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let Ok((id, title, parent, count, prompt, completion, cost, updated)) = row else {
            continue; // skip malformed row
        };
        if !include_children && parent.is_some() {
            continue;
        }
        let mut extra = BTreeMap::new();
        extra.insert(
            "db".to_string(),
            conn.path().unwrap_or_default().to_string(),
        );
        if let Some(c) = count {
            extra.insert("turnCount".to_string(), c.to_string());
        }
        if let (Some(p), Some(c)) = (prompt.filter(|v| *v > 0), completion.filter(|v| *v > 0)) {
            extra.insert("tokens".to_string(), format!("in {p} / out {c}"));
        }
        if let Some(cost) = cost.filter(|c| *c > 0.0) {
            extra.insert("totalCostUsd".to_string(), format!("{cost:.4}"));
        }
        let title = if title.is_empty() || title == "Untitled Session" {
            first_user_text(conn, &id).unwrap_or_default()
        } else {
            title
        };
        out.push(Session {
            id,
            harness: "crush".to_string(),
            cwd: PathBuf::from(cwd),
            title,
            updated_ms: updated.saturating_mul(1000),
            extra,
        });
    }
    Ok(out)
}

/// First user `text` part of a session — the real title before the TUI's
/// lazy titling kicks in.
fn first_user_text(conn: &Connection, id: &str) -> Option<String> {
    let parts: String = conn
        .query_row(
            "SELECT parts FROM messages \
             WHERE session_id = ?1 AND role = 'user' ORDER BY created_at, id LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .ok()?;
    let v = serde_json::from_str::<serde_json::Value>(&parts).ok()?;
    let text = v.as_array()?.iter().find_map(|p| {
        (p.get("type").and_then(|t| t.as_str()) == Some("text"))
            .then(|| p.pointer("/data/text").and_then(|t| t.as_str()))
    })??;
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| util::first_line_cut(text, 80))
}
