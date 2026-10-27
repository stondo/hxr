//! Goose adapter (Block's goose).
//!
//! Sessions live in `$XDG_DATA_HOME/goose/sessions/sessions.db` (sqlite;
//! default `~/.local/share/goose/sessions/sessions.db`). One `sessions` row
//! per chat with `working_dir`, `name`/`description`, `session_type` and
//! per-session token columns; conversation turns live in `messages` as
//! `content_json` — an anthropic-style parts array
//! (`[{"type":"text","text":…}]`, errors as `{"type":"error",…}`).
//!
//! Resume: `goose session --resume --session-id <id>` from the session's
//! `working_dir`. (Older goose kept `~/.config/goose/sessions/*.jsonl`
//! files — the `--path` legacy shim in `goose session --help` still
//! extracts ids from such filenames; only the sqlite store is read here.)
//!
//! Pitfalls encoded: goose injects a `<turn-context>` user message
//! (working dir / timestamp boilerplate) into every turn — title extraction
//! must skip `<`-prefixed text, like claude's command-caveats. A session
//! row is created before the first model reply, so rows whose only
//! messages are an error part still show up (kept: they are real, resumable
//! sessions). `created_timestamp` on messages is epoch **seconds**;
//! `sessions.updated_at` is a `YYYY-MM-DD HH:MM:SS` **UTC** string.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Goose;

impl Harness for Goose {
    fn name(&self) -> &'static str {
        "goose"
    }

    fn binary(&self) -> &'static str {
        "goose"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let db = db_path()?;
        let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("goose: opening db {}", db.display()))?;

        let mut stmt = conn.prepare(
            "SELECT id, name, coalesce(nullif(description, ''), ''), \
                    CASE WHEN user_set_name THEN 1 ELSE 0 END, working_dir, updated_at, \
                    total_tokens, input_tokens, output_tokens \
             FROM sessions ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let Ok((
                id,
                name,
                description,
                user_set,
                working_dir,
                updated_at,
                total,
                input,
                output,
            )) = row
            else {
                continue; // skip malformed row
            };
            let cwd = working_dir.map(PathBuf::from).unwrap_or_default();
            if cwd.as_os_str().is_empty() {
                continue; // no working dir recorded → nothing to resume in
            }
            let mut extra = BTreeMap::new();
            if let Some(t) = total.filter(|t| *t > 0) {
                extra.insert("tokens".to_string(), t.to_string());
            } else if let (Some(i), Some(o)) = (input.filter(|v| *v > 0), output.filter(|v| *v > 0))
            {
                extra.insert("tokens".to_string(), format!("{i} + {o}"));
            }
            out.push(Session {
                id,
                harness: "goose".to_string(),
                cwd,
                // user-renamed name wins; else the llm description beats the
                // generic per-binary name ("CLI Session")
                title: if user_set != 0 {
                    name
                } else if !description.is_empty() {
                    util::first_line_cut(&description, 80)
                } else {
                    name
                },
                updated_ms: updated_at
                    .as_deref()
                    .and_then(parse_sqlite_utc)
                    .unwrap_or_else(|| util::mtime_ms(&db)),
                extra,
            });
        }
        if out.is_empty() {
            anyhow::bail!("goose: no readable sessions in {}", db.display());
        }
        Ok(out)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec![
            "goose".to_string(),
            "session".to_string(),
            "--resume".to_string(),
            "--session-id".to_string(),
            session.id.clone(),
        ];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        let conn = self.conn().ok()?;
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
                "SELECT content_json FROM messages \
                 WHERE session_id = ?1 AND content_json LIKE ?2 \
                 ORDER BY created_timestamp, id",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session.id, like], |r| {
                r.get::<_, String>(0)
            })
            .ok()?;
        for content_json in rows.flatten() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&content_json) else {
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
        let conn = self.conn().ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT role, content_json FROM messages \
                 WHERE session_id = ?1 ORDER BY created_timestamp, id",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .ok()?;
        let mut out = String::new();
        for (role, content_json) in rows.flatten() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&content_json) else {
                continue;
            };
            let Some(parts) = v.as_array() else { continue };
            for part in parts {
                if part.get("type").and_then(|t| t.as_str()) != Some("text") {
                    continue;
                }
                let Some(text) = part.get("text").and_then(|t| t.as_str()) else {
                    continue;
                };
                let text = text.trim();
                if text.is_empty() || text.starts_with("<turn-context>") {
                    continue;
                }
                let text = if text.chars().count() > 2000 {
                    let mut t: String = text.chars().take(1999).collect();
                    t.push('(');
                    t.push(')');
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

impl Goose {
    fn conn(&self) -> Result<Connection> {
        let db = db_path()?;
        Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("goose: opening db {}", db.display()))
    }
}

/// `$XDG_DATA_HOME/goose/sessions/sessions.db`; under HXR_HOME the XDG
/// default `.local/share` is used (tests / sandboxes).
fn db_path() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("HXR_HOME") {
        let home = PathBuf::from(h);
        let xdg = home.join(".local/share/goose/sessions/sessions.db");
        if xdg.is_file() {
            return Ok(xdg);
        }
        return Ok(home.join(".config/goose/sessions/sessions.db"));
    }
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(x) => PathBuf::from(x),
        None => super::home_dir()?.join(".local/share"),
    };
    Ok(base.join("goose").join("sessions").join("sessions.db"))
}

/// sqlite `CURRENT_TIMESTAMP` strings are UTC: `YYYY-MM-DD HH:MM:SS`.
fn parse_sqlite_utc(s: &str) -> Option<i64> {
    let normalized = s.trim_end_matches('Z').replace(' ', "T");
    let dt: jiff::civil::DateTime = normalized.parse().ok()?;
    let zoned = dt.to_zoned(jiff::tz::TimeZone::UTC).ok()?;
    Some(zoned.timestamp().as_millisecond())
}
