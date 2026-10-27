#!/usr/bin/env bash
# Generate a real session corpus for every lab harness, driving each CLI
# headlessly against the LAN OpenAI-compatible endpoint.
#
# Run INSIDE the lab container (lab/run.sh shell, then lab/gen-corpus.sh) —
# or directly: lab/run.sh bash /src/lab/gen-corpus.sh
#
# Resulting stores land in /root (= .lab/home on the host): one or two real
# sessions per harness per project, in three project cwds so cwd-based
# layouts get exercised. Idempotent-ish: each run adds fresh sessions.
set -uo pipefail

: "${DEEP_BASE_URL:?point DEEP_BASE_URL at an OpenAI-compatible /v1 endpoint}"
: "${DEEP_MODEL:?set DEEP_MODEL to the model id served there}"
KEY=sk-lab-none
PROJ=/tmp/lab-projects
LOG=/tmp/gen-corpus.log

step() { printf '\n\033[1;35m== %s ==\033[0m\n' "$*" | tee -a "$LOG"; }
note() { printf '   %s\n' "$*" | tee -a "$LOG"; }
fail() { printf '\033[0;31m   FAILED: %s\033[0m\n' "$*" | tee -a "$LOG"; }

mkdir -p "$PROJ"/{alpha,beta,gamma}
: > "$LOG"

step "endpoint probe"
if curl -sf --max-time 5 "$DEEP_BASE_URL/models" > /dev/null; then
    note "reachable: $DEEP_BASE_URL (model $DEEP_MODEL)"
else
    fail "cannot reach $DEEP_BASE_URL — check the LAN/VPN from inside the container"
    exit 1
fi

# --- aider ---------------------------------------------------------------
step "aider"
aider_make_history() { # $1 = project dir, $2 = prompt
    (cd "$1" && OPENAI_API_KEY=$KEY OPENAI_API_BASE="$DEEP_BASE_URL" \
        aider --model "openai/$DEEP_MODEL" \
              --openai-api-base "$DEEP_BASE_URL" \
              --message "$2" \
              --yes-always --no-git --no-auto-commits --no-check-update \
              --no-show-model-warnings --no-linting --no-auto-test \
              2>&1 | tail -5)
}
if command -v aider >/dev/null; then
    aider_make_history "$PROJ/alpha" 'Create a file hello.py that prints lab-alpha' || fail aider
    aider_make_history "$PROJ/alpha" 'Change the greeting to say lab-alpha-2'      || fail aider
    aider_make_history "$PROJ/beta"  'Add a rust TODO list to notes.md'            || fail aider
else fail "aider not installed"; fi

# --- gemini-cli ----------------------------------------------------------
step "gemini-cli"
gemini_run() { # $1 = project dir, $2 = prompt
    (cd "$1" && OPENAI_API_KEY=$KEY OPENAI_BASE_URL="$DEEP_BASE_URL" \
        gemini -m "openai/$DEEP_MODEL" -p "$2" 2>&1 | tail -5)
}
if command -v gemini >/dev/null; then
    gemini_run "$PROJ/alpha" 'Write a one-line summary of this project to gemini.txt' || fail gemini
    gemini_run "$PROJ/beta"  'List the files here in listing.txt'                     || fail gemini
    gemini_run "$PROJ/gamma" 'Suggest a name for a yaml config demo project'          || fail gemini
else fail "gemini not installed"; fi

# --- goose ---------------------------------------------------------------
step "goose"
mkdir -p "$HOME/.config/goose"
cat > "$HOME/.config/goose/config.yaml" <<EOF
GOOSE_PROVIDER: openai
GOOSE_MODEL: $DEEP_MODEL
OPENAI_API_KEY: $KEY
OPENAI_BASE_HOST: $DEEP_BASE_URL/
EOF
goose_run() { # $1 = project dir, $2 = prompt
    (cd "$1" && goose run -t "$2" 2>&1 | tail -5)
}
if command -v goose >/dev/null; then
    goose_run "$PROJ/alpha" 'Create notes.md with the text goose-alpha' || fail goose
    goose_run "$PROJ/beta"  'Explain in one line what a Makefile does'  || fail goose
else fail "goose not installed"; fi

# --- crush ---------------------------------------------------------------
step "crush"
mkdir -p "$HOME/.config/crush"
cat > "$HOME/.config/crush/config.yaml" <<EOF
default_provider: deep
providers:
  deep:
    name: deep
    baseURL: $DEEP_BASE_URL
    apiKey: $KEY
    models:
      - id: $DEEP_MODEL
        name: DeepSeek V4 Flash
EOF
crush_run() { # $1 = project dir, $2 = prompt
    (cd "$1" && CRUSH_DISABLE_UPDATE=1 timeout 120 \
        crush -m "deep/$DEEP_MODEL" -p "$2" 2>&1 | tail -5)
}
if command -v crush >/dev/null; then
    crush_run "$PROJ/alpha" 'Add a heading to crush.md saying crush-alpha' || fail crush
    crush_run "$PROJ/gamma" 'One line: why is yaml indentation significant' || fail crush
else fail "crush not installed"; fi

# --- summary --------------------------------------------------------------
step "created stores (this run's footprint in /root)"
find /root -type f -newer "$LOG" 2>/dev/null \
    | grep -Ev '/(\.npm|\.cache|\.cargo|node_modules|\.config/goose/config.yaml|\.config/crush/config.yaml)' \
    | sort | tee -a "$LOG"

step "harness store roots on disk now"
for d in .gemini .aider .config/goose .config/crush .local/share/crush .goose; do
    [ -e "/root/$d" ] && note "$d: $(find /root/$d -type f | wc -l) files"
done
