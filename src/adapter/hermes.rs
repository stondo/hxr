//! Hermes adapter (Nous Hermes Agent).
//!
//! Two eras of storage, read as a union:
//!
//! - current: `~/.hermes/state.db` (sqlite, WAL — safe to open read-only
//!   while the gateway daemon is running). Sessions live in the `sessions`
//!   table: `title`, `cwd` (only populated by newer versions; older rows
//!   fall back to the home dir), `started_at`/`last_activity_at` as epoch
//!   seconds, token/cost columns, `git_branch`, and `parent_session_id` for
//!   compression continuations (children are hidden unless `--all`, like
//!   opencode subagents). Conversation bodies are the `messages` table
//!   (`role`, plain-text `content`, `timestamp`).
//! - legacy: `~/.hermes/sessions/session_<id>.json` — one flat JSON object
//!   per session (`session_id`, `model`, ISO `session_start`/`last_updated`,
//!   OpenAI-shaped `messages`). No cwd recorded anywhere in the file.
//!
//! Resume: `hermes --resume <id>` — the CLI resolves id-or-title against
//!   state.db only, so legacy file-only sessions are listed but their resume
//!   is best-effort. A `--resume` by default also cds into the recorded
//!   working directory itself; we still run it from `session.cwd`.
//!
//! Pitfalls encoded: `sessions.json` in the legacy dir is an (often empty)
//!   index, not a session — the glob must not match it. `archived`,
//!   `hidden` and `auto_archived` rows are housekeeping, not user sessions.
//!   `source` can be non-cli (e.g. `whatsapp` gateway chats) — still
//!   resumable, so they are listed like everything else.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::Harness;
use super::SearchPattern;
use super::util;
use crate::model::Session;

pub struct Hermes {
    /// `--all`: also list child sessions (parent_session_id set —
    /// compression continuations in hermes terms).
    pub include_children: bool,
}

impl Harness for Hermes {
    fn name(&self) -> &'static str {
        "hermes"
    }

    fn binary(&self) -> &'static str {
        "hermes"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let home = super::home_dir().context("hermes: home dir unavailable")?;
        let root = home.join(".hermes");
        if !root.is_dir() {
            anyhow::bail!("hermes: {} is not a directory", root.display());
        }

        let mut sessions = Vec::new();
        let mut db_ids: HashSet<String> = HashSet::new();

        let db = root.join("state.db");
        if db.is_file() {
            let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("hermes: opening db {}", db.display()))?;
            for s in collect_db(&conn, self.include_children, &home)? {
                db_ids.insert(s.id.clone());
                sessions.push(s);
            }
        }

        // Legacy flat files: everything the db does not already carry
        // (db wins on overlap — it is the only store `--resume` resolves).
        let legacy_dir = root.join("sessions");
        if legacy_dir.is_dir() {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(&legacy_dir)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                    matches!(name.as_deref(), Some(n)
                        if n.starts_with("session_") && n.ends_with(".json"))
                })
                .collect();
            paths.sort_by_key(|p| std::cmp::Reverse(util::mtime_ms(p)));
            for path in paths {
                if let Some(s) = transcribe_legacy(&path, &home)
                    && !db_ids.contains(&s.id)
                {
                    sessions.push(s);
                }
            }
        }

        if sessions.is_empty() {
            anyhow::bail!("hermes: no readable store in {}", root.display());
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec![
            "hermes".to_string(),
            "--resume".to_string(),
            session.id.clone(),
        ];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }

    fn content_match(&self, session: &Session, pat: &SearchPattern) -> Option<String> {
        if let Some(path) = session.extra.get("path") {
            return legacy_content_match(Path::new(path), pat);
        }
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
                "SELECT content FROM messages \
                 WHERE session_id = ?1 AND content LIKE ?2 ORDER BY timestamp, id",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session.id, like], |r| {
                r.get::<_, Option<String>>(0)
            })
            .ok()?;
        for content in rows.flatten().flatten() {
            if let Some(m) = pat.find(&content) {
                return Some(super::snippet(&content, m.range()));
            }
        }
        None
    }

    fn render(&self, session: &Session) -> Option<String> {
        if let Some(path) = session.extra.get("path") {
            let v = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).ok()?)
                .ok()?;
            let msgs = v.get("messages")?.as_array()?;
            return render_turns(msgs.iter().filter_map(message_text));
        }
        let conn = self.conn().ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT role, content FROM messages \
                 WHERE session_id = ?1 AND content IS NOT NULL ORDER BY timestamp, id",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![session.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .ok()?;
        render_turns(rows.flatten())
    }
}

impl Hermes {
    fn conn(&self) -> Result<Connection> {
        let home = super::home_dir().context("hermes: home dir unavailable")?;
        let db = home.join(".hermes").join("state.db");
        Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("hermes: opening db {}", db.display()))
    }
}

/// Session rows from state.db. The projection adapts to the columns that
/// exist (the schema grew over versions — cwd, git_branch, tokens …).
fn collect_db(conn: &Connection, include_children: bool, home: &Path) -> Result<Vec<Session>> {
    let cols: HashSet<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
        stmt.query_map([], |r| r.get::<_, String>(1))?
            .flatten()
            .collect()
    };

    let updated_expr = ["last_activity_at", "ended_at", "started_at"]
        .iter()
        .filter(|c| cols.contains(**c))
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let updated_expr = if updated_expr.is_empty() {
        "0".to_string()
    } else {
        format!("CAST(coalesce({updated_expr}) * 1000 AS INTEGER)")
    };

    let mut filters: Vec<String> = Vec::new();
    for col in ["archived", "hidden", "auto_archived"] {
        if cols.contains(col) {
            filters.push(format!("{col} = 0"));
        }
    }
    if !include_children && cols.contains("parent_session_id") {
        filters.push("parent_session_id IS NULL".to_string());
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", filters.join(" AND "))
    };

    let opt = |name: &str| cols.contains(name).then(|| format!("NULLIF({name}, '')"));
    let select_extra = |name: &str| {
        if cols.contains(name) {
            format!(", {name}")
        } else {
            ", NULL".to_string()
        }
    };
    // id, title, cwd, updated_ms, then optional extras in fixed slot order
    let sql = format!(
        "SELECT id, {}, {}, {updated_expr}{}{}{}{}{}{}{}{} \
         FROM sessions {where_clause}",
        opt("title").unwrap_or_else(|| "''".to_string()),
        opt("cwd").unwrap_or_else(|| "NULL".to_string()),
        select_extra("model"),
        select_extra("git_branch"),
        select_extra("message_count"),
        select_extra("input_tokens"),
        select_extra("output_tokens"),
        select_extra("reasoning_tokens"),
        select_extra("estimated_cost_usd"),
        select_extra("parent_session_id"),
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<String>>(4)?,  // model
            r.get::<_, Option<String>>(5)?,  // git_branch
            r.get::<_, Option<i64>>(6)?,     // message_count
            r.get::<_, Option<i64>>(7)?,     // input_tokens
            r.get::<_, Option<i64>>(8)?,     // output_tokens
            r.get::<_, Option<i64>>(9)?,     // reasoning_tokens
            r.get::<_, Option<f64>>(10)?,    // estimated_cost_usd
            r.get::<_, Option<String>>(11)?, // parent_session_id
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let Ok((id, title, cwd, updated, model, branch, turns, tin, tout, treason, cost, parent)) =
            row
        else {
            continue; // skip malformed row
        };
        if !include_children && parent.is_some() {
            continue; // belt-and-braces for schemas without the WHERE filter
        }
        let mut extra = BTreeMap::new();
        if let Some(m) = model {
            extra.insert("model".to_string(), m);
        }
        if let Some(b) = branch {
            extra.insert("branch".to_string(), b);
        }
        if let Some(t) = turns {
            extra.insert("turnCount".to_string(), t.to_string());
        }
        if tin.is_some() || tout.is_some() || treason.is_some() {
            let parts: Vec<String> = [
                tin.filter(|v| *v > 0).map(|v| format!("in {v}")),
                tout.filter(|v| *v > 0).map(|v| format!("out {v}")),
                treason.filter(|v| *v > 0).map(|v| format!("reason {v}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !parts.is_empty() {
                extra.insert("tokens".to_string(), parts.join(" / "));
            }
        }
        if let Some(c) = cost.filter(|c| *c > 0.0) {
            extra.insert("totalCostUsd".to_string(), format!("{c:.4}"));
        }
        out.push(Session {
            id,
            harness: "hermes".to_string(),
            cwd: cwd.map(PathBuf::from).unwrap_or_else(|| home.to_path_buf()),
            title: title.unwrap_or_default(),
            updated_ms: updated,
            extra,
        });
    }
    Ok(out)
}

/// One legacy `session_<id>.json` file → Session. Whole-file parse: these
/// are single flat objects (tens of KB seen); a broken file is skipped.
fn transcribe_legacy(path: &Path, home: &Path) -> Option<Session> {
    let v = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).ok()?).ok()?;
    let id = v
        .get("session_id")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("session_"))
                .and_then(|n| n.strip_suffix(".json"))
                .map(str::to_string)
        })?;

    let mut extra = BTreeMap::new();
    extra.insert("path".to_string(), path.to_string_lossy().into_owned());
    if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
        extra.insert("model".to_string(), m.to_string());
    }
    let msgs = v.get("messages").and_then(|m| m.as_array());
    if let Some(n) = msgs.map(|m| m.len()) {
        extra.insert("turnCount".to_string(), n.to_string());
    }

    let title = msgs
        .and_then(|m| {
            m.iter().find_map(|msg| {
                let content = msg.get("content")?.as_str()?.trim();
                (msg.get("role").and_then(|r| r.as_str()) == Some("user")
                    && !content.is_empty()
                    && !content.starts_with('<'))
                .then(|| util::first_line_cut(content, 80))
            })
        })
        .unwrap_or_default();

    Some(Session {
        id,
        harness: "hermes".to_string(),
        cwd: home.to_path_buf(), // legacy files record no cwd
        title,
        updated_ms: util::mtime_ms(path),
        extra,
    })
}

/// `(role, text)` for a legacy message ({role, content-as-string}).
fn message_text(msg: &serde_json::Value) -> Option<(String, String)> {
    let role = msg.get("role")?.as_str()?.to_string();
    let text = msg.get("content")?.as_str()?.trim().to_string();
    (!text.is_empty()).then_some((role, text))
}

fn render_turns(turns: impl Iterator<Item = (String, String)>) -> Option<String> {
    let mut out = String::new();
    for (role, text) in turns {
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
    (!out.is_empty()).then_some(out)
}

/// Legacy content search: title + message bodies of the parsed file.
fn legacy_content_match(path: &Path, pat: &SearchPattern) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).ok()?).ok()?;
    let mut values: Vec<&str> = Vec::new();
    util::json_string_values(&v, &mut values); // strings only — keys never match
    for value in values {
        if let Some(m) = pat.find(value) {
            return Some(super::snippet(value, m.range()));
        }
    }
    None
}
