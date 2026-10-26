//! hxr — one fzf picker for every coding-agent harness session.

mod adapter;
mod model;
mod ui;

use anyhow::{Context, Result, bail};
use clap::Parser;
use model::Session;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use adapter::{Harness, registry};

#[derive(Parser)]
#[command(
    name = "hxr",
    version,
    about = "Pick a session from any coding-agent harness and resume it"
)]
struct Cli {
    /// Start on (or lock to) one harness
    #[arg(long)]
    harness: Option<String>,

    /// Print the TSV session lines instead of opening the picker
    /// (optionally scoped to one harness, or "all")
    #[arg(long, num_args = 0..=1, default_missing_value = "all")]
    list: Option<String>,

    /// Print detected harnesses and session counts
    #[arg(long)]
    detect: bool,

    /// Skip fzf: resolve a session by id prefix directly
    #[arg(long)]
    session: Option<String>,

    /// Only show sessions newer than this (e.g. 30d, 2w, 48h, 15m)
    #[arg(long)]
    since: Option<String>,

    /// Content search: keep only sessions whose conversation bodies match
    /// this pattern (Rust regex; plain text when no metacharacters)
    #[arg(short = 'G', long = "grep")]
    grep: Option<String>,

    /// Print the session transcript instead of resuming it
    #[arg(long)]
    show: bool,

    /// Also list child/subagent sessions (opencode today)
    #[arg(long)]
    all: bool,

    /// Internal: advance the tab cycle in the state file and print the feed
    /// (hidden; used by fzf reload bindings).
    #[arg(long, hide = true)]
    cycle: Option<String>,

    /// Internal: set the in-picker content-search pattern (empty clears) and
    /// print the feed (hidden; the ctrl-g binding).
    #[arg(long, hide = true)]
    search: Option<String>,

    /// Internal: per-picker tab-state file (hidden).
    #[arg(long, hide = true)]
    state: Option<String>,

    /// Internal: render the preview pane for <HARNESS> <ID> (hidden; the
    /// fzf --preview command).
    #[arg(long, hide = true, num_args = 2, value_names = ["HARNESS", "ID"])]
    preview: Option<Vec<String>>,

    /// Print the resume command instead of executing it
    #[arg(long)]
    print_cmd: bool,

    /// Everything after -- is passed through to the harness CLI
    #[arg(last = true)]
    extra: Vec<String>,
}

/// A harness that passed the detection rule, with its sessions in memory.
struct Detected {
    harness: Box<dyn Harness>,
    sessions: Vec<Session>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // --detect: report every harness, regardless of scope
    if cli.detect {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for d in discover(None, cli.all) {
            if writeln!(
                out,
                "{:<10} {:>5} sessions",
                d.harness.name(),
                d.sessions.len()
            )
            .is_err()
            {
                break; // SIGPIPE-quiet under `| head`
            }
        }
        return Ok(());
    }

    let since_ms = cli
        .since
        .as_deref()
        .map(ui::parse_since)
        .map(|d| d.context("hxr: bad --since value (try 30d, 2w, 48h, 15m, 90s)"));

    // Internal preview mode: fzf highlights a line, we render its detail pane.
    if let Some(args) = &cli.preview
        && args.len() == 2
    {
        let (harness, id) = (&args[0], &args[1]);
        let detected = discover(Some(harness), false);
        match detected
            .first()
            .and_then(|d| d.sessions.iter().find(|s| &s.id == id))
            .cloned()
        {
            Some(mut session) => {
                let live_grep = cli
                    .state
                    .as_deref()
                    .and_then(|p| {
                        let (_, _, grep) = read_state(p);
                        nonempty(&grep).map(str::to_string)
                    })
                    .or_else(|| std::env::var("HXR_GREP").ok())
                    .and_then(|g| nonempty(&g).map(str::to_string));
                if let Some(pat) = live_grep
                    && let Ok(p) = adapter::SearchPattern::new(&pat)
                    && let Some(h) = detected.first().map(|d| d.harness.as_ref())
                    && let Some(snip) = session_hit(h, &session, &p)
                {
                    session.extra.insert("match".to_string(), snip);
                }
                if let Some(h) = detected.first().map(|d| d.harness.as_ref())
                    && let Some(st) = h.stats(&session)
                {
                    session.extra.extend(st);
                }
                print!("{}", render_preview(&session));
            }
            None => println!("(no {harness} session {id})"),
        }
        return Ok(());
    }

    // Internal feed mode: fzf's tab/btab reload runs us with --cycle/--state.
    // We advance the cycle position, then print header lines (tab bar, travels
    // with the list via --header-lines) + the target tab's session lines.
    if let Some(state_path) = &cli.state {
        let (current, names, cur_grep) = read_state(state_path);
        let (target, grep) = if let Some(dir) = cli.cycle.as_deref() {
            let target = cycle_target(&current, &names, dir);
            write_state(std::path::Path::new(state_path), &target, &names, &cur_grep);
            (target, nonempty(&cur_grep).map(str::to_string))
        } else if let Some(pat) = cli.search.as_deref() {
            write_state(std::path::Path::new(state_path), &current, &names, pat);
            (current, nonempty(pat).map(str::to_string))
        } else {
            return Ok(());
        };
        let tab = tab_of(&target);
        emit_feed(tab, &names, grep.as_deref(), cli.since.as_deref(), cli.all)?;
        return Ok(());
    }

    // --list [name]: scoped listing (this is also the fzf tab reload source).
    if cli.list.is_some() {
        let scope: Option<String> = cli
            .harness
            .clone()
            .or_else(|| cli.list.clone().filter(|l| !l.eq_ignore_ascii_case("all")));
        let detected = discover(scope.as_deref(), cli.all);
        check_detected(&detected, scope.as_deref())?;
        let mut sessions = flatten(&detected);
        apply_since(&mut sessions, since_ms)?;
        if let Some(pat) = &cli.grep {
            apply_grep(&mut sessions, &detected, pat)?;
        }
        // Merged lists carry 2-char harness tags so alt-0 stays navigable.
        let tags = scope.is_none();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for line in ui::format_lines(&sessions, tags) {
            if writeln!(out, "{line}").is_err() {
                break; // SIGPIPE-quiet under `| head`
            }
        }
        return Ok(());
    }

    // The interactive picker always discovers every harness (tab switches may
    // land anywhere); only --session resolution may stay scoped.
    let (detected, initial_tab): (Vec<Detected>, Option<String>) = if cli.session.is_some() {
        let scope = cli.harness.as_ref().map(|h| h.to_lowercase());
        let detected = discover(scope.as_deref(), cli.all);
        check_detected(&detected, scope.as_deref())?;
        (detected, None)
    } else {
        let detected = discover(None, cli.all);
        check_detected(&detected, None)?;
        let tab = cli.harness.as_ref().map(|h| h.to_lowercase());
        (detected, tab)
    };

    let mut sessions = flatten(&detected);
    apply_since(&mut sessions, since_ms)?;
    if let Some(pat) = &cli.grep {
        apply_grep(&mut sessions, &detected, pat)?;
    }
    if sessions.is_empty() {
        let msg = match (&cli.grep, &cli.since) {
            (Some(_), _) => "hxr: no sessions match the --grep pattern",
            (None, Some(_)) => "hxr: no sessions within the --since window",
            (None, None) => "hxr: no sessions found",
        };
        bail!("{msg}");
    }
    if let Some(tab) = &initial_tab
        && !sessions.iter().any(|s| &s.harness == tab)
    {
        bail!("hxr: harness not detected: {tab}");
    }

    // -G: propagate the pattern to fzf child processes (--preview) so the
    // match snippet can be re-derived per highlight. Single-threaded here.
    if let Some(pat) = &cli.grep {
        // Safety: no worker threads are running at this point.
        unsafe {
            std::env::set_var("HXR_GREP", pat);
        }
    }
    let picked = if let Some(prefix) = &cli.session {
        resolve_by_prefix(&sessions, prefix)?
    } else {
        let names: Vec<String> = detected
            .iter()
            .map(|d| d.harness.name().to_string())
            .collect();
        pick_with_fzf(
            &sessions,
            &names,
            initial_tab.as_deref(),
            cli.since.as_deref(),
            cli.grep.as_deref(),
        )?
    };

    let Some(session) = picked else {
        return Ok(()); // ESC / empty selection: exit quietly, like ocr
    };

    let h = detected
        .iter()
        .find(|d| d.harness.name() == session.harness)
        .map(|d| &d.harness)
        .context("hxr: selected harness disappeared")?;

    if cli.show {
        let text = h.render(&session).unwrap_or_else(|| {
            format!(
                "(no transcript renderer for {} yet)\n\nhxr show metadata:\n{}",
                session.harness,
                render_preview(&session)
            )
        });
        page(text)?;
        return Ok(());
    }

    let (argv, cwd) = h.resume(&session, &cli.extra);

    if cli.print_cmd {
        println!("cwd: {}", cwd.display());
        println!("cmd: {}", argv.join(" "));
        return Ok(());
    }
    if !cwd.is_dir() {
        bail!("hxr: directory no longer exists: {}", cwd.display());
    }
    // Safety: no worker threads are running at this point.
    unsafe {
        std::env::remove_var("HXR_GREP");
    }
    let err = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&cwd)
        .exec();
    bail!("hxr: exec {}: {err}", argv[0]);
}

fn check_detected(detected: &[Detected], scope: Option<&str>) -> Result<()> {
    match (scope, detected.is_empty()) {
        (Some(name), true) => bail!("hxr: harness not detected: {name}"),
        (None, true) => bail!("hxr: no sessions found"),
        _ => Ok(()),
    }
}

/// Flatten in tab order, newest first overall (matches ocr ordering).
fn flatten(detected: &[Detected]) -> Vec<Session> {
    let mut sessions: Vec<Session> = detected.iter().flat_map(|d| d.sessions.clone()).collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
    sessions
}

fn apply_since(sessions: &mut Vec<Session>, since_ms: Option<anyhow::Result<i64>>) -> Result<()> {
    if let Some(dur) = since_ms {
        let dur = dur?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(i64::MAX);
        sessions.retain(|s| s.updated_ms >= now - dur);
    }
    Ok(())
}

/// Content-search filter (`-G`): keep sessions whose bodies match, attaching
/// the first matching snippet as `extra["match"]`. Titles match too, so the
/// filter is a superset of the default fuzzy title search.
fn apply_grep(sessions: &mut Vec<Session>, detected: &[Detected], pattern: &str) -> Result<()> {
    use rayon::prelude::*;
    let pat = adapter::SearchPattern::new(pattern)?;
    let hits: Vec<Option<String>> = sessions
        .par_iter()
        .map(|s| {
            detected
                .iter()
                .find(|d| d.harness.name() == s.harness)
                .and_then(|d| session_hit(d.harness.as_ref(), s, &pat))
        })
        .collect();
    let mut kept = Vec::with_capacity(sessions.len());
    for (session, hit) in sessions.drain(..).zip(hits) {
        if let Some(snippet) = hit {
            let mut s = session;
            s.extra.insert("match".to_string(), snippet);
            kept.push(s);
        }
    }
    *sessions = kept;
    Ok(())
}

/// Full -G hit cascade for one session: title, then first user message,
/// then conversation bodies via the adapter.
fn session_hit(
    h: &dyn adapter::Harness,
    s: &Session,
    pat: &adapter::SearchPattern,
) -> Option<String> {
    if pat.find(&s.title).is_some() {
        return Some(format!("(title) {}", s.title));
    }
    if let Some(msg) = s.extra.get("first_user_message")
        && let Some(m) = pat.find(msg)
    {
        return Some(adapter::snippet(msg, m.range()));
    }
    h.content_match(s, pat)
}

/// Detection rule: binary on PATH and discover() yields >= 1 session.
/// `scope` limits discovery to a single harness (used by --harness/--list);
/// adapters still run concurrently and in registry (tab) order.
fn discover(scope: Option<&str>, include_all: bool) -> Vec<Detected> {
    use rayon::prelude::*;
    registry(include_all)
        .into_par_iter()
        .filter(|h| scope.is_none_or(|s| h.name() == s.to_lowercase()))
        .filter_map(|harness| {
            if !adapter::on_path(harness.binary()) {
                return None;
            }
            match harness.discover() {
                Ok(sessions) if !sessions.is_empty() => Some(Detected { harness, sessions }),
                Ok(_) => None,
                Err(e) => {
                    eprintln!("hxr: skipping {}: {e:#}", harness.name());
                    None
                }
            }
        })
        .collect()
}

/// Single-shot fzf with tab cycling. `tab`/`shift-tab` reload from this same
/// binary (`--cycle next|prev --state <file>`): the subprocess advances the
/// cycle position and prints the new feed — header lines (tab bar) included,
/// which `--header-lines=2` pins above the list. Returns the chosen Session
/// or None on cancel.
fn pick_with_fzf(
    sessions: &[Session],
    names: &[String],
    initial_tab: Option<&str>,
    since: Option<&str>,
    grep: Option<&str>,
) -> Result<Option<Session>> {
    let state = state_path();
    if let Some(dir) = state.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let initial = initial_tab.unwrap_or("all").to_string();
    // the CLI -G pattern seeds the state so tab cycling and the preview pane
    // stay in content mode for the whole session
    write_state(&state, &initial, names, grep.unwrap_or(""));

    let feed: Vec<Session> = match initial_tab {
        Some(tab) => sessions
            .iter()
            .filter(|s| s.harness == tab)
            .cloned()
            .collect(),
        None => sessions.to_vec(),
    };
    let mut input = ui::feed_header(names, initial_tab, grep).join("\n");
    input.push('\n');
    input.push_str(&ui::format_lines(&feed, initial_tab.is_none()).join("\n"));

    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "hxr".to_string());
    let mut child = Command::new("fzf")
        .args(fzf_args(&exe, &state.to_string_lossy(), since))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("hxr: fzf not found — install it (https://github.com/junegunn/fzf)")?;
    {
        let mut stdin = child.stdin.take().context("hxr: fzf stdin")?;
        // Broken pipe here means fzf exited early (e.g. user cancelled while
        // the feed was still writing) — treat as cancel, not an error.
        if stdin.write_all(input.as_bytes()).is_err() {
            let _ = child.wait();
            let _ = std::fs::remove_file(&state);
            return Ok(None);
        }
    } // dropped => EOF so fzf can render
    let out = child.wait_with_output()?;
    let _ = std::fs::remove_file(&state);
    if !out.status.success() {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim_end_matches('\n');
    if line.is_empty() {
        return Ok(None);
    }
    let mut fields = line.splitn(3, '\t');
    let (id, harness) = (fields.next(), fields.next());
    Ok(match (id, harness) {
        (Some(id), Some(harness)) => sessions
            .iter()
            .find(|s| s.id == id && s.harness == harness)
            .cloned(),
        _ => None,
    })
}

/// Build the fzf argv. Tab cycling is delegated to reload subprocesses that
/// re-emit the header lines with the list, so no dynamic change-* binds are
/// needed. Bind command text must avoid commas (fzf's bind separator).
fn fzf_args(exe: &str, state: &str, since: Option<&str>) -> Vec<String> {
    let cycle_cmd = |dir: &str| {
        let mut c = format!("\"{exe}\" --cycle {dir} --state \"{state}\"");
        if let Some(s) = since {
            c.push_str(&format!(" --since {s}"));
        }
        c
    };
    let mut search_cmd = format!("\"{exe}\" --search '{{q}}' --state \"{state}\"");
    if let Some(s) = since {
        search_cmd.push_str(&format!(" --since {s}"));
    }
    vec![
        "--delimiter=\t".to_string(),
        "--with-nth=4".to_string(),
        "--header-lines=2".to_string(),
        "--height=80%".to_string(),
        "--reverse".to_string(),
        "--tiebreak=index".to_string(),
        "--prompt=hxr> ".to_string(),
        format!("--preview=\"{exe}\" --preview {{2}} {{1}} --state \"{state}\""),
        "--preview-window=right:55%:wrap".to_string(),
        format!("--bind=tab:reload({})", cycle_cmd("next")),
        format!("--bind=btab:reload({})", cycle_cmd("prev")),
        format!("--bind=ctrl-g:reload({search_cmd})"),
    ]
}

/// Per-picker tab state file (unique per process, removed on exit).
fn state_path() -> std::path::PathBuf {
    std::env::temp_dir()
        .join("opencode")
        .join(format!("hxr-{}.state", std::process::id()))
}

/// Render the fzf preview pane for one session.
fn render_preview(s: &Session) -> String {
    use std::fmt::Write as _;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mut out = String::new();
    let title = if s.title.is_empty() {
        "(untitled)".to_string()
    } else {
        cap(&s.title, 2, 160)
    };
    let _ = writeln!(out, "{title}");
    let _ = writeln!(out, "──────────────────────────────────");
    let _ = writeln!(out, "harness   {}", s.harness);
    let _ = writeln!(out, "id        {}", s.id);
    let mark = if s.cwd.is_dir() { "" } else { "   ⚠ missing" };
    let _ = writeln!(out, "dir       {}{}", s.cwd.display(), mark);
    let abs = jiff::Timestamp::from_millisecond(s.updated_ms)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| s.updated_ms.to_string());
    let _ = writeln!(
        out,
        "updated   {abs} ({})",
        ui::when(s.updated_ms, now / 1000)
    );
    for (label, key) in [
        ("branch", "branch"),
        ("model", "model"),
        ("turns", "turnCount"),
        ("cost", "totalCostUsd"),
        ("tokens", "tokens"),
    ] {
        if let Some(v) = s.extra.get(key) {
            let _ = writeln!(out, "{label:<9} {v}");
        }
    }
    if let Some(m) = s.extra.get("match") {
        let _ = writeln!(out, "──────────────────────────────────");
        let _ = writeln!(out, "match:");
        let _ = writeln!(out, "{}", cap(m, 6, 600));
    }
    if let Some(msg) = first_message(s) {
        let _ = writeln!(out, "──────────────────────────────────");
        let _ = writeln!(out, "first message:");
        let _ = writeln!(out, "{}", cap(&msg, 30, 1500));
    }
    out
}

/// First user message: from extra when the adapter captured it, else bounded
/// head read of the transcript path.
fn first_message(s: &Session) -> Option<String> {
    if let Some(m) = s
        .extra
        .get("first_user_message")
        .or_else(|| s.extra.get("first_prompt"))
    {
        return Some(m.clone());
    }
    let path = s.extra.get("path")?;
    let tag = match s.harness.as_str() {
        "pi" => "message",
        _ => "user",
    };
    let head = adapter::util::read_jsonl_head(std::path::Path::new(path), 64 * 1024);
    adapter::util::first_user_text(&head, tag)
}

/// Print the picker feed for `tab` (None = merged) honoring the in-picker
/// content-search pattern: 2 header lines + formatted session lines.
fn emit_feed(
    tab: Option<&str>,
    names: &[String],
    grep: Option<&str>,
    since: Option<&str>,
    include_all: bool,
) -> Result<()> {
    let detected = discover(tab, include_all);
    let mut sessions = flatten(&detected);
    if let Some(s) = since {
        let ms =
            ui::parse_since(s).context("hxr: bad --since value (try 30d, 2w, 48h, 15m, 90s)")?;
        apply_since(&mut sessions, Some(Ok(ms)))?;
    }
    if let Some(pat) = grep {
        apply_grep(&mut sessions, &detected, pat)?;
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in ui::feed_header(names, tab, grep)
        .into_iter()
        .chain(ui::format_lines(&sessions, tab.is_none()))
    {
        if writeln!(out, "{line}").is_err() {
            break;
        }
    }
    Ok(())
}

/// Write `text` through $PAGER when stdout is a terminal, else plain print.
fn page(text: String) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        print!("{text}");
        return Ok(());
    }
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&pager)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("hxr: starting pager {pager:?}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait()?;
    Ok(())
}

/// Cap text to at most `lines` lines / `chars` characters, ellipsized.
fn cap(s: &str, lines: usize, chars: usize) -> String {
    let mut out: String = s.lines().take(lines).collect::<Vec<_>>().join("\n");
    if out.chars().count() > chars {
        out = out.chars().take(chars - 1).collect::<String>() + "…";
    }
    if s.lines().count() > lines || s.chars().count() > chars {
        out.push_str("\n…");
    }
    out.trim_end().to_string()
}

/// Line 1: active tab ("all" or harness name). Line 2: tab names.
/// Line 3: content-search pattern (empty = metadata mode).
fn write_state(path: &std::path::Path, current: &str, names: &[String], grep: &str) {
    let _ = std::fs::write(path, format!("{current}\n{}\n{grep}", names.join(" ")));
}

fn read_state(path: &str) -> (String, Vec<String>, String) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return ("all".to_string(), Vec::new(), String::new());
    };
    let mut lines = content.lines();
    let current = lines.next().unwrap_or("all").trim().to_string();
    let names = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let grep = lines.next().unwrap_or("").trim().to_string();
    (current, names, grep)
}

fn tab_of(name: &str) -> Option<&str> {
    (name != "all").then_some(name)
}

fn nonempty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// Pure cycle math over `["all", names...]`; `dir` is "next" or "prev".
fn cycle_target(current: &str, names: &[String], dir: &str) -> String {
    let mut seq: Vec<&str> = vec!["all"];
    seq.extend(names.iter().map(String::as_str));
    let idx = seq.iter().position(|t| *t == current).unwrap_or(0);
    let n = seq.len();
    let step = if dir == "prev" { n - 1 } else { 1 };
    seq[(idx + step) % n].to_string()
}

/// Resolve `--session <prefix>` against the visible sessions.
fn resolve_by_prefix(sessions: &[Session], prefix: &str) -> Result<Option<Session>> {
    let matches: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.id.starts_with(prefix))
        .collect();
    match matches.len() {
        0 => bail!("hxr: no session matches prefix {prefix}"),
        1 => Ok(Some(matches[0].clone())),
        _ => {
            let ids: Vec<&str> = matches.iter().map(|s| s.id.as_str()).collect();
            bail!("hxr: ambiguous prefix {prefix}: {}", ids.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cycle_target;

    #[test]
    fn cycle_wraps_both_ways() {
        let names: Vec<String> = ["opencode", "claude", "codex"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(cycle_target("all", &names, "next"), "opencode");
        assert_eq!(cycle_target("opencode", &names, "next"), "claude");
        assert_eq!(cycle_target("codex", &names, "next"), "all"); // wrap
        assert_eq!(cycle_target("all", &names, "prev"), "codex"); // wrap back
        assert_eq!(cycle_target("claude", &names, "prev"), "opencode");
    }
}
