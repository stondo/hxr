# hxr — harness session storage analysis

Verified against live stores on Linux on **2026-10-26** (hermes on
2026-10-27 against a real v0.21.5 install).
All harnesses were installed with real sessions when these formats were
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
| hermes | `~/.hermes/state.db` `sessions` table (current; WAL) ∪ legacy `~/.hermes/sessions/session_<id>.json` flat objects (44 files live) | `sessions.id` (`YYYYMMDD_HHMMSS_<hex>`); legacy `session_id` | `sessions.cwd` — only newer rows; legacy files record none → home fallback | `sessions.title`; legacy: first user `messages[]` entry | `coalesce(last_activity_at, ended_at, started_at)` (s); legacy file mtime | `hermes --resume <id>` — resolves id-or-title against state.db only |
| goose | `~/.local/share/goose/sessions/sessions.db` sqlite: `sessions` + `messages` (parts-array `content_json`) | `id` (`YYYYMMDD_N`) | `working_dir` | user-renamed `name` (`user_set_name`) else llm `description` | `updated_at` (UTC `CURRENT_TIMESTAMP` string) | `goose session --resume --session-id <id>` from working_dir |
| gemini | `~/.gemini/tmp/<project>/chats/session-*.jsonl` + `~/.gemini/projects.json` (cwd→project-name map) | header `sessionId` uuid; filename fallback (rewinds drop the header) | projects.json inverted over the parent dir name (`<base>-N` on collisions) → home fallback | first non-`<session_context>` user entry | file mtime (jsonl `$set.lastUpdated` is not worth a tail scan) | `gemini --session-file <path>` from the original cwd |
| aider | `<project>/.aider.chat.history.md` markdown (blocks per `# aider chat started`), `~/.aider/chat-history.md` fallback | `path#block` (opaque; no id resume in aider) | history file's directory | block's `#### ` prompt | file mtime (block timestamps are local naive) | `aider --restore-chat-history` in the project (continue-latest mapping, per issue #2) |
| crush | `~/.local/share/crush/projects.json` → per-project `<project>/.crush/crush.db` (`sessions` + `messages` with typed `parts` arrays) | `sessions.id` (uuid) | projects.json `path` | `title` unless "Untitled Session" → first user `text` part | `updated_at` (epoch **seconds**) | `crush -s <id>` from the project path |

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

### hermes
Two eras of storage, read as a union (live evidence: netcup, v0.21.5,
8 db rows + 44 legacy files, 7 overlapping):

- Current: `~/.hermes/state.db` (sqlite, WAL — concurrent readers fine while
  the gateway daemon runs). `sessions` table (62 columns): `title`, `cwd`
  (**only newer rows** — pre-May-2026 rows are NULL → resume dir falls back
  to home), `started_at`/`ended_at`/`last_activity_at` epoch seconds,
  `input_tokens`/`output_tokens`/`reasoning_tokens`/`estimated_cost_usd`,
  `git_branch`, `source` (`cli`, `whatsapp` gateway chats are resumable too),
  `archived`/`hidden`/`auto_archived` housekeeping flags (filtered), and
  `parent_session_id` for compression continuations (children hidden unless
  `--all`; `--resume` follows the chain to the live tip itself). Bodies: the
  `messages` table (`role`, plain-text `content`, `timestamp`) — there is an
  FTS5 index too, but hxr does its own regex matching over rows.
- Legacy: `~/.hermes/sessions/session_<id>.json` — one flat JSON object:
  `session_id` (= filename without `session_`/`.json`), `model`,
  ISO `session_start`/`last_updated`, OpenAI-shaped `messages[{role,content}]`.
  **No cwd anywhere in the file.** `sessions.json` in the same dir is an
  (empty) index — the glob must not match it.
- Resume: `hermes --resume <id-or-title>` — resolution (`_resolve_session_by_
  name_or_id` in `hermes_cli/main.py`) queries state.db **only**, so legacy
  file-only sessions are listed but their resume is best-effort. By default
  a resume also cds into the recorded working directory itself; hxr still
  spawns it from `session.cwd`.
- Timestamps in the db are epoch **seconds** (float) — ×1000 for hxr's ms.

### goose
- `~/.local/share/goose/sessions/sessions.db` (sqlite; respects
  `XDG_DATA_HOME`, falls back to `~/.config/goose/sessions/sessions.db` for
  pre-XDG layouts). Older goose kept jsonl session files there — the `--path`
  shim in `goose session --help` still parses ids from those filenames; only
  the sqlite store is read.
- `sessions` rows: `id` = `YYYYMMDD_N` (per-day counter), `working_dir`,
  `session_type` (`user` seen), token columns (`total_tokens` else
  input+output), `updated_at` as sqlite `CURRENT_TIMESTAMP` (UTC string).
  A row is created **before** the first model reply — error-only sessions
  are real and resumable.
- `messages` rows: `role`, `content_json` = anthropic-style parts array
  (`[{"type":"text","text":…}]`; errors as `{"type":"error",…}`), epoch
  **seconds** `created_timestamp`. Every turn carries an injected
  `<turn-context>` user part (cwd/time boilerplate) — skipped like claude's
  command-caveats.
- `user_set_name` is **BOOLEAN** (sqlite numeric) — a `nullif('')`-style
  text projection silently breaks row decoding (found live).
- Resume: `goose session --resume --session-id <id>` from `working_dir`.

### gemini-cli
- `~/.gemini/tmp/<project>/chats/session-<ts><hash>.jsonl`. Line 1 is a
  header (`sessionId` uuid, `projectHash`, ISO `startTime`/`lastUpdated`,
  `kind`), then entries `{id, timestamp, type: "user"|"assistant",
  content: [{text}]}` interleaved with `{"$set": {…}}` and `{"$rewindTo":
  …}` operator records on retried turns. **A rewind can drop the header
  entirely** — the filename is the reliable id fallback.
- No cwd in transcripts. `~/.gemini/projects.json` maps cwd → project dir
  name (basename; collisions get `-1` suffixes). Sessions in unmapped
  project dirs fall back to home.
- First user entry is always a `<session_context>` boilerplate dump —
  skipped like claude's command-caveats. Aborted sessions (prompt recorded,
  reply never completed) are real resume points and stay listed. Model
  turns are written as `type: "gemini"` with **plain-string** `content`
  (not an `assistant` parts array — verified on a completed turn).
- Resume: `--resume` only accepts `latest` or a per-project *index*, not a
  session id — `--session-file <path>` is the stable handle. Headless
  non-tty runs need `--skip-trust`.

### aider
- No session store — per-project markdown logs. `<project>/
  .aider.chat.history.md` accumulates chats delimited by
  `# aider chat started at <local ts>`; `#### <prompt>` lines are user
  turns, bare text is the assistant reply (reasoning inside
  `<thinking-content-…>` wrappers), `> `-prefixed lines are the UI log —
  `> Model:` and `> Tokens: N sent, M received.` carry useful metadata.
  `~/.aider/chat-history.md` is the outside-any-project fallback.
- Resume contract: aider cannot resume a specific session —
  `--restore-chat-history` reloads the *latest* chat of the project file.
  Every listed block resumes the same way (continue-latest mapping agreed
  in issue #2); block ids exist only for listing and `-G` scoping.
- Discovery: bounded (depth-6) home walk for the well-known filename,
  hidden dirs / node_modules / target skipped — aider ships no index.
  Recency is file mtime for every block (delimiter timestamps are local
  naive strings).

### crush
- v0.98 config is **bash**: `~/.config/crush/crushrc` runs at startup
  (`provider add <id> --type openai-compat --base-url … --api-key …`,
  `model add <provider>/<id> …`). The old `config.yaml` providers block and
  a bare `providers.json` entry are ignored — "No providers configured"
  means no crushrc/agent config. `providers.json` in the data dir is only
  the model catalog.
- Store is per-project sqlite: `~/.local/share/crush/projects.json`
  registers `{path, data_dir, last_accessed}` (a failed run registers the
  project and creates the db too); `<data_dir>/crush.db` (default
  `<project>/.crush/`) holds `sessions` (uuid, title — "Untitled Session"
  until the TUI lazily titles it, token/cost columns, `parent_session_id`
  children, epoch-**second** timestamps) and `messages` (role, `parts` =
  typed arrays: `text` / `reasoning` / `tool_call` / `tool_result` /
  `finish`; prose lives in `text` parts at `/data/text`).
- Headless: `crush run -m provider/model "<prompt>"` (no `-p`, and `-y`
  does not hoist onto subcommands). Resume: `crush -s <id>`.

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
