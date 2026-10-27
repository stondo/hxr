#!/usr/bin/env bash
# Build / enter the hxr harness lab (see lab/Containerfile).
#
#   lab/run.sh build   — build the image
#   lab/run.sh shell   — interactive bash; repo at /src, lab home at /root
#   lab/run.sh <cmd…>  — run one command in the lab instead of a shell
#
# The lab home is ./.lab/home on the host (gitignored) mounted as /root: every
# harness store the lab creates persists across container runs, on the host.
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE=localhost/hxr-lab:1

build() {
    podman build -t "$IMAGE" lab "$@"
}

run() {
    mkdir -p .lab/home
    local tty=()
    [ -t 0 ] && tty=(-t)
    # shellcheck disable=SC2086
    podman run --rm "${tty[@]}" --name hxr-lab \
        --security-opt label=disable \
        -v "$PWD:/src" \
        -v "$PWD/.lab/home:/root" \
        -e DEEP_BASE_URL -e DEEP_MODEL \
        "$IMAGE" "$@"
}

case "${1:-shell}" in
    build) shift; build "$@" ;;
    shell) run bash ;;
    *)     run "$@" ;;
esac
