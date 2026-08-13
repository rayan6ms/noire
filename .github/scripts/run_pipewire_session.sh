#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" && "${NOIRE_DBUS_SESSION_WRAPPED:-0}" != "1" ]]; then
    exec env NOIRE_DBUS_SESSION_WRAPPED=1 dbus-run-session -- "$0" "$@"
fi

log_dir="${NOIRE_PIPEWIRE_LOG_DIR:-${RUNNER_TEMP:-/tmp}/noire-pipewire-logs}"
mkdir -p "$log_dir"
runtime_dir=$(mktemp -d "${RUNNER_TEMP:-/tmp}/noire-pipewire-runtime.XXXXXX")
chmod 700 "$runtime_dir"
export XDG_RUNTIME_DIR="$runtime_dir"

pipewire >"$log_dir/pipewire-server.log" 2>&1 &
pipewire_pid=$!
wireplumber >"$log_dir/wireplumber.log" 2>&1 &
wireplumber_pid=$!
pipewire-pulse >"$log_dir/pipewire-pulse.log" 2>&1 &
pipewire_pulse_pid=$!

cleanup() {
    kill "$pipewire_pulse_pid" "$wireplumber_pid" "$pipewire_pid" 2>/dev/null || true
    wait "$pipewire_pulse_pid" "$wireplumber_pid" "$pipewire_pid" 2>/dev/null || true
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

for _attempt in $(seq 1 100); do
    if [[ -S "$runtime_dir/pulse/native" ]]; then
        break
    fi
    if ! kill -0 "$pipewire_pulse_pid" 2>/dev/null; then
        echo "pipewire-pulse exited before creating its compatibility socket" >&2
        exit 1
    fi
    sleep 0.05
done
test -S "$runtime_dir/pulse/native"

cargo test --release --package noire-pipewire --features native-test \
    --test native_session --test phase4_session --test phase5_session \
    --locked -- --ignored --nocapture --test-threads=1 \
    2>&1 | tee "$log_dir/native-session.log"

cargo test --release --package noired --features native-test \
    --test phase6_native --locked -- --ignored --nocapture --test-threads=1 \
    2>&1 | tee "$log_dir/phase6-native.log"

if grep -Eiq '(^|[^[:alpha:]])(xrun|underrun|overrun)([^[:alpha:]]|$)' \
    "$log_dir/pipewire-server.log" "$log_dir/pipewire-pulse.log" \
    "$log_dir/wireplumber.log"; then
    echo "The disposable PipeWire session reported an xrun" >&2
    exit 1
fi

kill "$pipewire_pulse_pid" "$wireplumber_pid" "$pipewire_pid" 2>/dev/null || true
wait "$pipewire_pulse_pid" "$wireplumber_pid" "$pipewire_pid" 2>/dev/null || true

cargo test --release --package noired --features native-test \
    --test phase7_recovery --locked -- --ignored --nocapture --test-threads=1 \
    2>&1 | tee "$log_dir/phase7-recovery.log"
