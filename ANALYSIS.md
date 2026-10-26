# hxr — harness session storage analysis

Verified against live stores on Linux on **2026-10-26**.
All seven harnesses were installed with real sessions when these formats were
captured. Paths below use `~` for the user's home directory.

Total corpus: **~600 sessions**, 3 SQLite DBs, ~350 JSONL files needing first-line reads.

## Summary matrix

| Harness | Session source | id | cwd | title | recency | Resume |
|---|---|---|---|---|---|---|
| opencode | SQLite via `opencode debug paths db`; `session_v2` ∪ `session` (parentless, unarchived) | `id` | `directory` | `coalesce(nullif(title,''), slug)` | `time_updated` (ms) | `opencode -s <id>` from `directory` |
| claude | `~/.claude/projects/<munged-cwd>/<session-uuid>.jsonl` (173 files) | filename | demunge project dir (`-` → `/`) | last `ai-title` entry (`aiTitle`) in transcript tail; fallback first user msg; fallback `history.jsonl` first `display` | last `history.jsonl` ts (ms) / file mtime | `claude -r <id>` from original cwd |
| codex | `~/.codex/state_5.sqlite` → `threads` table (238 rollouts indexed) | `id` | `cwd` col | `title` col; fallback `first_user_message` | `updated_at_ms` / `recency_at` | `codex resume <uuid>` — works from any dir |
| pi | `~/.pi/agent/sessions/<munged-cwd>/<ts>_<uuid>.jsonl` (65 files) | line-1 `id` | line-1 `cwd` | first user `message` in file | file mtime | `pi --session <path\|id>` from session cwd |
| omp | `~/.omp/agent/history.db`: `session_titles` ⋈ `history` (11 titled sessions) | `session_id` | latest `history.cwd` for session | `session_titles.title` | `session_titles.updated_at` / `history.created_at` (s) | `omp -r <id-prefix\|path>` from session cwd |
| qwen | `~/.qwen/projects/<munged-cwd>/chats/<uuid>.jsonl` (106) + sibling `.runtime.json`; `qwen sessions list --json` exists as alternative index | chats filename / line-1 `sessionId` | line-1 `cwd` / runtime `work_dir` | first user msg in jsonl | file mtime / runtime `started_at` | `qwen -r <id>` from original cwd |
| kimi | `~/.kimi-code/session_index.jsonl` (`sessionId`/`sessionDir`/`workDir`) + `<sessionDir>/agents/main/wire.jsonl` event streams (192) | index `sessionId` (`session_<uuid>`) | index `workDir` | first `turn.prompt` entry's `input[].text`; model from first `llm.request` | wire.jsonl mtime | `kimi -S <id>` from workDir |
| reasonix | `~/.reasonix/sessions/<name>.jsonl` + sibling `.meta.json` (7) | file path (session "name", e.g. `code-local-ai`) | `meta.workspace` | `meta.summary` (may be long — truncate) | file mtime | `reasonix -r <path> --dir <workspace>` |

## Per-harness evidence and details

### opencode (reference — current `ocr` script)
- DB path: `opencode debug paths db` (do not hardcode).
- Listing query (unchanged from the current script): union of `session_v2` and
  `session` where `parent_id IS NULL AND time_archived IS NULL`, minus ids
  already in `session_v2`.
- Open read-only: `file:<db>?mode=ro`.

### claude code
- Project dirs: `~/.claude/projects/-home-stondo-PARA-Projects-<proj>/` — path
  with `/` → `-` and leading `-`. Demunge by replacing `-` → `/` after dropping
  the leading `-` **only on the known prefix shape**; safer: use `history.jsonl`
  `project` field which contains the *unmunged* absolute path.
- `~/.claude/history.jsonl`: one line per user prompt:
  `{"display","pastedContents","timestamp"(ms),"project"(abs path),"sessionId"}`.
  Build per-session: title = first `display` (skip `/slash-commands`), cwd =
  `project`, updated = max `timestamp`.
- Session transcripts `<uuid>.jsonl` are one file per session — id = filename.
  Skip non-`*.jsonl` entries (e.g. `memory/` dirs).
- Resume: `claude -r <sessionId>`; run from the session's original cwd so
  project context/CLAUDE.md load correctly.
- Note: `claude --bg`/`attach` background sessions exist; out of scope v1.

### codex
- Primary index: `~/.codex/state_5.sqlite`, table `threads`. Verified columns:
  `id, rollout_path, created_at, updated_at, source, model_provider, cwd, title,
  sandbox_policy, approval_mode, tokens_used, has_user_event, archived,
  archived_at, git_sha, git_branch, git_origin_url, cli_version,
  first_user_message, agent_nickname, agent_role, memory_mode, model,
  reasoning_effort, agent_path, created_at_ms, updated_at_ms, thread_source,
  preview, recency_at`.
  Filter: `archived = 0` (and optionally `has_user_event = 1` to drop
  never-used threads).
- Fallbacks: `~/.codex/session_index.jsonl`
  (`{"id","thread_name","updated_at"}` — no cwd) and rollout first lines
  `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (line 1 `session_meta` has
  `payload.id` + `payload.cwd`).
- The `threads` row payload can be huge (embedded conversation text) — **never
  `SELECT *`**; select explicit columns only.
- Resume: `codex resume <uuid>` restores the recorded cwd itself; no cd needed.

### pi (pi-coding-agent)
- `~/.pi/agent/sessions/<munged-cwd>/<ISO-ts>_<uuid>.jsonl`; line 1:
  `{"type":"session","version":3,"id","timestamp","cwd"}`.
- No title records — derive from first `{"type":"message","message":{"role":"user"}}` entry.
- Resume: `pi --session <path|id>` (partial UUID accepted); passing the **full
  path** is unambiguous across projects. Run from session cwd.
- `--name` display names exist (`-n`) but are not persisted per session in v3
  files in a way we found — treat first-user-message as title.

### omp (oh-my-pi)
- `~/.omp/agent/history.db` (SQLite, WAL):
  - `session_titles(session_id PK, title, updated_at)` — 11 rows here.
  - `history(id, prompt UNIQUE, created_at, cwd, session_id, use_count)` — has FTS trigger; open read-only.
- Join: title + updated from `session_titles`; cwd = `history.cwd` of the
  session's latest row.
- Session files live in `~/.omp/agent/sessions/<munged-ish>/<ts>_<uuid>/` —
  **dir names are unreliable munges** (e.g. `-PARA-Projects-dots`, missing the
  home prefix — do NOT demunge). Inner `*.jsonl` line 1 is
  `{"type":"title","title",...}` followed by `{"type":"session","id",...}` with
  cwd — usable as fallback/verification. Per-agent transcripts
  (e.g. `K3-Design.jsonl`) live in the same folder; skip non-main ones or map by
  inner session id.
- Resume: `omp -r <session-id-prefix | path>`; also `--restore` for
  interrupted-turn recovery (nice future hook). Run from session cwd.

### qwen code
- Real history: `~/.qwen/projects/<munged-cwd>/chats/<uuid>.jsonl` (106 files) +
  sibling `<uuid>.runtime.json` (`{pid, session_id, work_dir, hostname,
  started_at, qwen_version}` — live-process locks; treat presence as "possibly
  running" hint).
- jsonl line 1: `{"uuid","parentUuid","sessionId","timestamp","type":"user","cwd","version","gitBranch"}`.
- `~/.qwen/sessions/<pid>.json` are **runtime pid locks, not history — ignore
  this dir** (pitfall: it looks like the obvious place).
- `qwen sessions list --json [--limit N]` exists — evaluate output shape during
  implementation; if it includes id/cwd/title/updated it can replace raw
  scanning; otherwise scan files.
- Resume: `qwen -r <sessionId>` from the original cwd. Requires
  `--chat-recording` to have been on (default).

### reasonix
- `~/.reasonix/sessions/<name>.jsonl` + `<name>.events.jsonl` + `<name>.meta.json`.
  Names are stable session handles like `code-local-ai` (project-scoped);
  archived copies get `__archive_<ts>` suffixes — skip those.
- `.meta.json`: `{"summary","workspace","totalCostUsd","turnCount",...}` —
  title + cwd + juicy preview data.
- `.events.jsonl` appends over time (multiple `session.opened` lines) — use file mtime for recency.
- Resume: `reasonix -r <path-to-jsonl> --dir <workspace>` (verified flags:
  `-r, --resume string` = "session file path, session ID, or machine session
  ID"). `reasonix session list --json` lists only *machine* sessions (empty
  here) — not usable as index.

## Detection rule ("installed and has sessions")

A harness tab is shown iff **all** hold:
1. binary resolvable on PATH (`opencode`, `claude`, `codex`, `pi`, `omp`, `qwen`, `reasonix`)
2. its storage root exists
3. its adapter returns ≥ 1 session

Never render an empty tab; a missing tab must not be an error.

## Edge cases & pitfalls (from live data)

- **omp dir munging is broken-by-design** (no home prefix) → never derive cwd
  from omp session dir names; use `history.db` or inner jsonl.
- **qwen `sessions/` is a decoy** (pid locks); history is under `projects/*/chats/`.
- **codex `threads` rows embed full conversations** → explicit column selects only.
- **claude `history.jsonl`** misses sessions whose prompts were never typed
  interactively → fallback title: first user message of the transcript; mtime fallback.
- **reasonix archives** get suffixed names — filter `__archive_*`.
- Dead cwds: show a ⚠ marker, still allow resume (fall back to `$HOME` or
  refuse with a clear message — decide in UX pass).
- opencode db schema drift: keep the `session_v2 ∪ session` union tolerant
  (missing table → skip that half).

## Python vs Rust evaluation

Measured scale: ~600 sessions; work = 3 sqlite reads + ~350 first-line reads + JSON decode of small headers.

| | Python 3 (port of `ocr`) | Rust (release, this plan) |
|---|---|---|
| cold start | ~30–50 ms interpreter | ~3–5 ms |
| full scan | ~150–400 ms serial | ~10–30 ms (rayon/parallel) |
| fzf tab switch (re-exec lister via `reload`) | 200–500 ms — feels sluggish | 10–40 ms — instant |
| deployment | needs python3 on host | one static binary (musl) in `~/.local/bin` |
| deps | stdlib only | clap, rusqlite(bundled), serde, serde_json, jiff, rayon, anyhow |

**Decision: Rust.** The chosen tab UX (fzf `--bind …reload(hxr --list <h>)`)
re-executes the lister on every tab switch — exactly the hot path where the
Python port would feel laggy and Rust is imperceptible. Scope is small
(~800–1200 LOC); `rusqlite` + `serde_json` cover every storage format found.
