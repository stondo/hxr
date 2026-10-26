//! Presentation helpers: relative-time labels and the aligned TSV line format
//! fed to fzf. Mirrors the column math of the original `ocr` script:
//!
//! ```text
//! id \t harness \t cwd \t {when:<when_w}  {title:<title_w}  {short_dir}
//! ```
//!
//! fzf is started with `--delimiter=\t --with-nth=4` so only the formatted
//! display field is shown and searched.

use crate::model::Session;

/// Terminal width in columns (COLUMNS env, ioctl, then 80) minus the fzf
/// pointer/margin allowance of 4 — same as `ocr`.
pub fn usable_cols() -> usize {
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&c| c > 0)
        .or_else(|| {
            terminal_size::terminal_size()
                .map(|(w, _)| w.0 as usize)
                .filter(|&c| c > 0)
        })
        .unwrap_or(80);
    cols.saturating_sub(4)
}

/// Compact "n min ago" / "n hours ago" / local timestamp label.
pub fn when(updated_ms: i64, now: i64) -> String {
    let secs = now - updated_ms / 1000;
    if secs < 120 * 60 {
        format!("{} min ago", secs.max(0) / 60)
    } else if secs < 24 * 3600 {
        format!("{} hours ago", secs / 3600)
    } else {
        match jiff::Timestamp::from_millisecond(updated_ms) {
            Ok(ts) => ts
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string(),
            Err(_) => updated_ms.to_string(),
        }
    }
}

/// `~/PARA/...` shortened form of a directory.
pub fn shorten_dir(dir: &std::path::Path) -> String {
    let s = dir.to_string_lossy();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if !home.is_empty()
            && let Some(rest) = s.strip_prefix(&*home)
        {
            return format!("~{rest}");
        }
    }
    s.into_owned()
}

/// 2-char harness tag for the merged view.
pub fn tag(harness: &str) -> String {
    match harness {
        "opencode" => "oc",
        "claude" => "cl",
        "codex" => "cx",
        "pi" => "pi",
        "omp" => "om",
        "qwen" => "qw",
        "kimi" => "ki",
        "reasonix" => "rx",
        other => return other.chars().take(2).collect(),
    }
    .to_string()
}

/// Tab bar header: `[...]` marks the active tab; `active: None` = merged view.
pub fn tab_bar(names: &[String], active: Option<&str>) -> String {
    let mut parts = vec![match active {
        None => "[all]".to_string(),
        _ => "all".to_string(),
    }];
    for n in names {
        parts.push(if Some(n.as_str()) == active {
            format!("[{n}]")
        } else {
            n.clone()
        });
    }
    parts.join("  ")
}

/// Header lines for the fzf feed (padded into display field 4 so
/// `--with-nth=4` keeps showing them). These travel WITH the list through
/// `--header-lines`, which is how the tab bar survives reloads.
pub fn feed_header(names: &[String], active: Option<&str>) -> Vec<String> {
    let hint = if names.len() > 1 {
        "tab next · shift-tab prev · enter resume · esc cancel"
    } else {
        "enter resume · esc cancel"
    };
    vec![
        format!("\t\t\t{}", tab_bar(names, active)),
        format!("\t\t\t{hint}"),
    ]
}

/// Parse a `--since` duration like `30d`, `2w`, `48h`, `15m`, `90s` into ms.
pub fn parse_since(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 2 {
        return None;
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => return None,
    };
    n.checked_mul(mult)
}

/// Build the fzf input lines plus an (harness, id) keyed index for resolving
/// the selection back to a [`Session`].
///
/// `tags` prefixes 2-char harness tags to titles (merged view); per-harness
/// tabs leave them out (redundant with the tab bar). Dead cwds get a ⚠ suffix.
pub fn format_lines(sessions: &[Session], tags: bool) -> Vec<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let entries: Vec<(String, String, String)> = sessions
        .iter()
        .map(|s| {
            let mut title = s.title.split_whitespace().collect::<Vec<_>>().join(" ");
            if tags {
                title = format!("[{}] {title}", tag(&s.harness));
            }
            let short = shorten_dir(&s.cwd);
            let short = if s.cwd.is_dir() {
                short
            } else {
                format!("{short} ⚠")
            };
            (when(s.updated_ms, now), title, short)
        })
        .collect();

    let when_w = entries
        .iter()
        .map(|e| e.0.chars().count())
        .max()
        .unwrap_or(0);
    let dir_w = entries
        .iter()
        .map(|e| e.2.chars().count())
        .max()
        .unwrap_or(0);
    let max_title = entries
        .iter()
        .map(|e| e.1.chars().count())
        .max()
        .unwrap_or(0);
    let title_w = max_title.min(usable_cols().saturating_sub(when_w + dir_w).max(20));

    sessions
        .iter()
        .zip(&entries)
        .map(|(s, (w, title, short))| {
            let title_display: String = if title.chars().count() > title_w {
                let mut t: String = title.chars().take(title_w.saturating_sub(1)).collect();
                t.push('…');
                t
            } else {
                title.clone()
            };
            format!(
                "{}\t{}\t{}\t{w:<when_w$}  {title_display:<title_w$}  {short}",
                s.id,
                s.harness,
                s.cwd.to_string_lossy()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn when_boundaries() {
        let now = 1_700_000_000i64 * 1000;
        assert_eq!(when(now, now / 1000), "0 min ago");
        assert_eq!(when(now - 90 * 60 * 1000, now / 1000), "90 min ago");
        assert_eq!(when(now - 3 * 3600 * 1000, now / 1000), "3 hours ago");
        // >24h renders a local timestamp, just assert it parses as a date-ish string
        let d = when(now - 3 * 24 * 3600 * 1000, now / 1000);
        assert!(d.contains('-') && d.contains(':'), "{d}");
    }

    #[test]
    fn tags_and_tab_bar() {
        assert_eq!(tag("opencode"), "oc");
        assert_eq!(tag("newthing"), "ne");
        let names = vec!["opencode".to_string(), "claude".to_string()];
        assert_eq!(tab_bar(&names, None), "[all]  opencode  claude");
        assert_eq!(tab_bar(&names, Some("claude")), "all  opencode  [claude]");
    }

    #[test]
    fn since_parsing() {
        assert_eq!(parse_since("30d"), Some(30 * 86_400_000));
        assert_eq!(parse_since("2w"), Some(14 * 86_400_000));
        assert_eq!(parse_since("15m"), Some(900_000));
        assert_eq!(parse_since("90s"), Some(90_000));
        assert_eq!(parse_since("48h"), Some(48 * 3_600_000));
        assert_eq!(parse_since("bogus"), None);
        assert_eq!(parse_since("5x"), None);
    }

    #[test]
    fn merged_tags_and_dead_dir_marker() {
        // format_lines reads HOME/COLUMNS — serialize against fixture env tests
        let _guard = crate::adapter::util::ENV_LOCK.lock().unwrap();
        let mk = |harness: &str, cwd: &str| crate::model::Session {
            id: "x".into(),
            harness: harness.into(),
            cwd: cwd.into(),
            title: "a title".into(),
            updated_ms: 0,
            extra: Default::default(),
        };
        let lines = format_lines(
            &[mk("codex", "/tmp"), mk("qwen", "/nonexistent-hxr-t")],
            true,
        );
        assert!(lines[0].contains("[cx] a title"));
        assert!(lines[1].contains("[qw] a title"));
        assert!(lines[1].contains("⚠"));
        assert!(!lines[0].contains("⚠"));
        let plain = format_lines(&[mk("codex", "/tmp")], false);
        assert!(plain[0].contains("a title") && !plain[0].contains("[cx]"));
    }
}
