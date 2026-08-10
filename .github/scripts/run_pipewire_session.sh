#!/usr/bin/env bash
set -euo pipefail

log_dir="${NOIRE_PIPEWIRE_LOG_DIR:-${RUNNER_TEMP:-/tmp}/noire-pipewire-logs}"
mkdir -p "$log_dir"
runtime_dir=$(mktemp -d "${RUNNER_TEMP:-/tmp}/noire-pipewire-runtime.XXXXXX")
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR="$runtime_dir"

pipewire >"$log_dir/pipewire-server.log" 2>&1 &
pipewire_pid=$!
wireplumber >"$log_dir/wireplumber.log" 2>&1 &
wireplumber_pid=$!

cleanup() {
    kill "$wireplumber_pid" "$pipewire_pid" 2>/dev/null || true
    wait "$wireplumber_pid" "$pipewire_pid" 2>/dev/null || true
    rmdir "$runtime_dir" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

for _attempt in $(seq 1 100); do
    if [[ -S "$runtime_dir/pipewire-0" ]]; then
        break
    fi
    if ! kill -0 "$pipewire_pid" 2>/dev/null; then
        echo "PipeWire exited before creating its native socket" >&2
        exit 1
    fi
    sleep 0.05
done
test -S "$runtime_dir/pipewire-0"

cargo test --package noire-pipewire --features native-test \
    --test native_session --locked -- --ignored --nocapture \
    2>&1 | tee "$log_dir/native-session.log"

if grep -Eiq '(^|[^[:alpha:]])(xrun|underrun|overrun)([^[:alpha:]]|$)' \
    "$log_dir/pipewire-server.log" "$log_dir/wireplumber.log"; then
    echo "The disposable PipeWire session reported an xrun" >&2
    exit 1
fi
