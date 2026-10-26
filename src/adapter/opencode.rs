//! opencode adapter — port of the original `ocr` python script.
//!
//! Sessions live in a SQLite db whose path is resolved at runtime via
//! `opencode debug paths db` (never hardcode it). Rows come from the union of
//! `session_v2` and `session` (parentless, unarchived), with `session` rows
//! deduplicated against `session_v2`. Resume: `opencode -s <id>` from the
//! session's own directory.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use super::Harness;
use crate::model::Session;

pub struct Opencode;

impl Harness for Opencode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn binary(&self) -> &'static str {
        "opencode"
    }

    fn discover(&self) -> Result<Vec<Session>> {
        let db = db_path()?;
        let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("opencode: opening db {}", db.display()))?;

        let has_v2 = table_exists(&conn, "session_v2")?;
        let has_v1 = table_exists(&conn, "session")?;
        if !has_v2 && !has_v1 {
            anyhow::bail!("opencode: neither session_v2 nor session table exists");
        }

        // Same projection as the original script; `slug` fills in when the
        // title is empty. The NOT IN filter only applies when both tables
        // exist (mirrors the UNION ALL minus ids already in session_v2).
        let parts: Vec<&str> = match (has_v2, has_v1) {
            (true, true) => vec![
                "SELECT id, directory, coalesce(nullif(title, ''), slug), time_updated \
                 FROM session_v2 WHERE parent_id IS NULL AND time_archived IS NULL",
                "SELECT id, directory, coalesce(nullif(title, ''), slug), time_updated \
                 FROM session WHERE parent_id IS NULL AND time_archived IS NULL \
                 AND id NOT IN (SELECT id FROM session_v2)",
            ],
            (true, false) => vec![
                "SELECT id, directory, coalesce(nullif(title, ''), slug), time_updated \
                 FROM session_v2 WHERE parent_id IS NULL AND time_archived IS NULL",
            ],
            (false, true) => vec![
                "SELECT id, directory, coalesce(nullif(title, ''), slug), time_updated \
                 FROM session WHERE parent_id IS NULL AND time_archived IS NULL",
            ],
            _ => unreachable!(),
        };

        let mut sessions = Vec::new();
        let mut errors: Vec<anyhow::Error> = Vec::new();
        for sql in parts {
            match collect(&conn, sql) {
                Ok(mut rows) => sessions.append(&mut rows),
                Err(e) => errors.push(e.context("opencode: query failed")),
            }
        }
        if sessions.is_empty() {
            for e in errors {
                eprintln!("hxr: {e:#}");
            }
            return Ok(sessions);
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
        Ok(sessions)
    }

    fn resume(&self, session: &Session, extra: &[String]) -> (Vec<String>, PathBuf) {
        let mut argv = vec!["opencode".to_string(), "-s".to_string(), session.id.clone()];
        argv.extend_from_slice(extra);
        (argv, session.cwd.clone())
    }
}

/// Resolve the db path via the CLI (single source of truth, survives moves).
/// Fast path: the XDG default location, falling back to `opencode debug paths db`
/// for non-standard setups.
fn db_path() -> Result<PathBuf> {
    if let Some(p) = xdg_default_db()
        && p.is_file()
    {
        return Ok(p);
    }
    let out = Command::new("opencode")
        .args(["debug", "paths", "db"])
        .output()
        .context("opencode: running `opencode debug paths db`")?;
    if !out.status.success() {
        anyhow::bail!(
            "opencode: `debug paths db` exited with {}",
            out.status.code().unwrap_or(-1)
        );
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() || path.contains('\n') {
        anyhow::bail!("opencode: unexpected `debug paths db` output: {path:?}");
    }
    Ok(PathBuf::from(path))
}

/// `$XDG_DATA_HOME/opencode/opencode.db` (or the `~/.local/share` default).
fn xdg_default_db() -> Option<PathBuf> {
    let base = match std::env::var_os("HXR_HOME") {
        // tests / sandboxes: everything lives under HXR_HOME, ignore real XDG
        Some(h) => PathBuf::from(h).join(".local/share"),
        None => match std::env::var_os("XDG_DATA_HOME") {
            Some(x) => PathBuf::from(x),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
        },
    };
    Some(base.join("opencode").join("opencode.db"))
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Tolerant row collector: individual broken rows are skipped.
fn collect(conn: &Connection, sql: &str) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let Ok((id, directory, title, updated)) = row else {
            continue; // skip malformed row
        };
        let cwd = PathBuf::from(&directory);
        let mut extra = BTreeMap::new();
        extra.insert("directory".to_string(), directory);
        out.push(Session {
            id,
            harness: "opencode".to_string(),
            cwd,
            title: title.unwrap_or_default(),
            updated_ms: updated,
            extra,
        });
    }
    Ok(out)
}
