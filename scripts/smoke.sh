#!/usr/bin/env bash
# hxr live smoke test — drift canary against the real harness stores.
# Run after any harness CLI update (or any hxr change):
#   scripts/smoke.sh            (uses hxr from PATH, falls back to target/release)
# Exits non-zero on the first real failure. Never launches a harness TUI:
# resume commands are only resolved via --print-cmd.
set -u

HXR="$(command -v hxr || true)"
[ -z "$HXR" ] && [ -x "$(dirname "$0")/../target/release/hxr" ] && \
    HXR="$(dirname "$0")/../target/release/hxr"
if [ -z "$HXR" ]; then
    echo "FAIL: no hxr binary on PATH and no target/release/hxr build" >&2
    exit 1
fi
echo "using $HXR ($("$HXR" --version))"

fails=0
fail() { echo "FAIL: $*" >&2; fails=$((fails + 1)); }

# 1) detection: at least one harness, each line "<name>  <n> sessions"
mapfile -t detect < <("$HXR" --detect)
[ "${#detect[@]}" -ge 1 ] || fail "--detect returned nothing"
for line in "${detect[@]}"; do
    name="$(awk '{print $1}' <<<"$line")"
    n="$(awk '{print $2}' <<<"$line")"
    [[ "$name" =~ ^[a-z]+$ ]] || fail "odd harness name in --detect: $line"
    [ "$n" -gt 0 ] 2>/dev/null || fail "harness $name reported non-positive count: $n"

    # 2) scoped listing shape: >=1 line, 4 TSV fields, sane types
    newest="$("$HXR" --list "$name" | head -1)"
    [ -n "$newest" ] || { fail "--list $name empty"; continue; }
    IFS=$'\t' read -r id h cwd _ <<<"$newest"
    [ "$h" = "$name" ] || fail "--list $name: field2 is '$h'"
    case "$cwd" in /*) : ;; *) fail "--list $name: cwd not absolute: $cwd" ;; esac

    # 3) resume resolution (dry-run only)
    out="$("$HXR" --session "$id" --harness "$name" --print-cmd 2>&1)" || {
        fail "--session resolution failed for $name/$id: $out"; continue; }
    cmd="$(printf '%s\n' "$out" | grep '^cmd: ' | cut -d' ' -f2-)"
    case "$cmd" in
        "$name "*|*"$id"*) : ;;
        *) fail "$name resume cmd looks wrong: $cmd" ;;
    esac
done

# 4) merged list shape spot-check (2-char harness tag present on first line)
first_merged="$("$HXR" --list | head -1)"
[ -n "$first_merged" ] || fail "merged --list empty"
cut -f4 <<<"$first_merged" | grep -q '\[[a-z][a-z]\]' \
    || fail "merged --list first line carries no [tag]: $first_merged"

# 5) timing budget (soft): merged listing should stay well under 1s
t0=$(date +%s%N); "$HXR" --list > /dev/null; t1=$(date +%s%N)
ms=$(( (t1 - t0) / 1000000 ))
[ "$ms" -lt 1000 ] || fail "merged --list took ${ms}ms (>1000ms)"

echo
if [ "$fails" -eq 0 ]; then
    echo "OK: ${#detect[@]} harnesses, merged list ${ms}ms"
else
    echo "SMOKE FAILED: $fails failure(s)" >&2
    exit 1
fi
