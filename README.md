# hxr — one picker for every AI coding CLI

[![CI](https://github.com/stondo/hxr/actions/workflows/ci.yml/badge.svg)](https://github.com/stondo/hxr/actions/workflows/ci.yml)

Every coding-agent CLI keeps its own session history in its own format, in its
own place. `hxr` reads them all and gives you **one fzf picker** to find a past
session — across every harness you have installed — and resume it in the right
directory with the right command.

```
[all]  opencode  claude  codex  pi  omp  qwen  kimi  reasonix
tab next · shift-tab prev · enter resume · esc cancel
──────────────────────────────────────────────────────────────
  12 min ago   [oc] Adopt froggeric v22.5…   ~/PARA/Projects/local-ai
  3 hours ago  [cx] Refine language plan     ~/PARA/Projects/func-lang
  1 day ago    [cl] Plan the social platform ~/PARA/Projects/local-ai
```

- **One merged list**, newest first, with 2-char harness tags — or per-harness
  tabs cycled with **Tab / Shift+Tab** (only installed harnesses appear)
- **Preview pane**: first user message, model, git branch, turns, cost, token totals (codex, kimi)
- **`hxr show`**: render a session transcript read-only through `$PAGER`
- **Right resume semantics** per harness: correct CLI flag, correct working
  directory (`claude -r <id>`, `codex resume <uuid>`, `pi --session <path>`,
  `omp -r <id>`, `qwen -r <id>`, `reasonix -r <path> --dir <ws>`, …)
- **Fast**: parallel read-only discovery (~900 sessions in ~50 ms), bounded
  I/O, never full-scans big transcripts for the list view
- **Read-only** by design; broken stores are skipped, never touched

## Requirements

- Linux, macOS or Windows 10+ (process replace on unix, spawn on Windows)
- [fzf](https://github.com/junegunn/fzf) — e.g. `dnf install fzf`,
  `brew install fzf`, `apt install fzf` (`winget install junegunn.fzf` on
  Windows)
- The harness CLIs you want listed (a harness only shows up when its binary is
  on `PATH` **and** it has at least one session)

## Install

```sh
cargo install hxr-picker        # installs the `hxr` command
brew install stondo/tap/hxr-picker   # macOS / Linuxbrew
# or grab a static binary from the releases page:
# https://github.com/stondo/hxr/releases (linux amd64/arm64, macOS amd64/arm64)
```

## Usage

```sh
hxr                  # pick a session (fzf), resume it in its directory
hxr                  #   Tab = next harness tab · Shift+Tab = previous
hxr                  #   Ctrl-G = search inside conversations for the typed
hxr                  #   query (content mode; header shows the pattern;
hxr                  #   Ctrl-G on an empty query returns to metadata mode)
hxr --harness codex  # start on a specific tab
hxr --since 7d       # only sessions newer than a duration (s/m/h/d/w)
hxr -G <pattern>     # content search: keep sessions whose conversation
                     #   bodies (or titles) match; snippets in the preview
hxr --show           # print the transcript of the picked session ($PAGER)
hxr --all            # include child/subagent sessions (opencode today)
hxr --detect         # show detected harnesses + session counts
hxr --list [name]    # dump TSV lines (id, harness, cwd, display) — no fzf needed
hxr --session <id-prefix> --print-cmd   # resolve without fzf, print resume cmd
hxr -- --model x     # args after -- pass through to the harness CLI
```

In the picker, typing fuzzy-filters what is on screen (instant). Ctrl-G
submits the typed query to the content engine: one deliberate scan
(~0.2s), sessions whose conversation bodies match replace the list, tab
cycling stays inside content mode, and the preview pane shows match
snippets. Patterns are Rust regex; note the in-picker bind cannot carry
apostrophes (use `-G` for those).

### Content search (`-G`)

`hxr -G "that error message"` scans conversation bodies live — every
harness store, read-only, no index to keep fresh (about 0.2s on ~1,000
sessions here; the largest corpus, opencode's sqlite, prefilters literals
in SQL). Patterns are Rust `regex`; plain text when there are no
metacharacters. Titles match too, matched snippets appear in the preview
pane, and results flow into the normal picker: Enter resumes. Composable
with `--harness` and `--since`.

`--list` is a stable plain-text contract: `hxr --list | grep -i <project> |
fzf` works too. Dead session directories are flagged with `⚠`.

## Supported harnesses

| Harness | Sessions read from | Resume |
|---|---|---|
| opencode | SQLite (`session_v2 ∪ session`) | `opencode -s <id>` |
| Claude Code | `~/.claude/projects/<munged>/<uuid>.jsonl` + `history.jsonl` | `claude -r <id>` |
| Codex | `~/.codex/state_*.sqlite` (`threads`) | `codex resume <uuid>` |
| pi | `~/.pi/agent/sessions/**/*.jsonl` | `pi --session <path>` |
| omp | `~/.omp/agent/history.db` | `omp -r <id>` |
| Qwen Code | `~/.qwen/projects/*/chats/*.jsonl` | `qwen -r <id>` |
| Kimi | `~/.kimi-code/session_index.jsonl` + wire event streams | `kimi -S <id>` |
| Copilot CLI | `~/.copilot/session-state/<uuid>/workspace.yaml` | `copilot -r <id>` |
| reasonix | `~/.reasonix/sessions/*.meta.json` | `reasonix -r <path> --dir <ws>` |

Format details, evidence and per-harness pitfalls live in
[ANALYSIS.md](ANALYSIS.md).

## Remote machines

`hxr -R deep -R perception` lists those machines' sessions too, live over
ssh — nothing is copied, nothing is synced; the sessions stay where they
live. Merged-view tags carry the origin (`[cx@deep]`), and Enter delegates
the resume to the remote's own hxr over `ssh -t` (so the right CLI starts
in the right directory on the right machine). Works with the picker, and
with `--list` for scripting. Requirements per remote: ssh access (key
auth; `~/.ssh/config` aliases work) and hxr >= 0.7.0 on its PATH. In
content mode (`-G` / Ctrl-G) remote sessions match by title; body search
runs on the machine you're sitting at.

## Drift checks

These CLIs update themselves and their storage formats drift. Two guards:

```sh
cargo test        # fixture tests: one synthetic store per harness, incl. sqlite schemas
scripts/smoke.sh  # live canary: detection, list shape, resume resolution, timing
```

Run `scripts/smoke.sh` after any harness CLI update; a broken adapter shows up
as a missing tab in `hxr --detect` and as a FAIL line here.

## Adding a harness

Adapters are small, isolated modules implementing the `Harness` trait
(`discover()` + `resume()`) in [`src/adapter/`](src/adapter/). Register it in
`registry()`, add a fixture block in `src/adapter/fixtures_test.rs`, a row in
the tables above — done. Each adapter's module docs describe its storage
format and the pitfalls encoded in it.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
