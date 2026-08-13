#!/usr/bin/env bash
set -euo pipefail

work_dir=$(mktemp -d "${RUNNER_TEMP:-/tmp}/noire-phase7-session.XXXXXX")
runtime_dir="$work_dir/runtime"
config_dir="$work_dir/config"
mkdir -p "$runtime_dir" "$config_dir"
chmod 700 "$runtime_dir"

daemon_pid=""
bus_pid=""
cleanup() {
    if [[ -n "$daemon_pid" ]]; then
        kill "$daemon_pid" 2>/dev/null || true
        wait "$daemon_pid" 2>/dev/null || true
    fi
    if [[ -n "$bus_pid" ]]; then
        kill "$bus_pid" 2>/dev/null || true
    fi
    rm -rf "$work_dir"
}
trap cleanup EXIT INT TERM

mapfile -t bus_info < <(dbus-daemon --session --fork --print-address=1 --print-pid=1)
test "${#bus_info[@]}" -eq 2
export DBUS_SESSION_BUS_ADDRESS="${bus_info[0]}"
bus_pid="${bus_info[1]}"
export XDG_RUNTIME_DIR="$runtime_dir"
export XDG_CONFIG_HOME="$config_dir"

cargo build --release --package noired --package noirectl --locked
target/release/noired >"$work_dir/noired.log" 2>&1 &
daemon_pid=$!

for _attempt in $(seq 1 100); do
    if target/release/noirectl --json status >"$work_dir/status.json" 2>/dev/null; then
        break
    fi
    kill -0 "$daemon_pid"
    sleep 0.02
done
target/release/noirectl --json status >/dev/null

for descriptor in "/proc/$daemon_pid/fd/"*; do
    socket=$(readlink "$descriptor" 2>/dev/null || true)
    [[ "$socket" == socket:\[*\] ]] || continue
    inode=${socket#socket:[}
    inode=${inode%]}
    for table in tcp tcp6 udp udp6; do
        if awk -v inode="$inode" 'NR > 1 && $10 == inode { found = 1 } END { exit !found }' \
            "/proc/$daemon_pid/net/$table"; then
            echo "normal operation opened an INET socket" >&2
            exit 1
        fi
    done
done

target/release/noirectl --json set strength 0.75 >/dev/null
target/release/noirectl --json set enabled false >/dev/null
target/release/noirectl --json diagnostics >"$work_dir/diagnostics.json"
if find "$work_dir" -type f \( -iname '*.wav' -o -iname '*.flac' -o -iname '*.raw' -o -iname '*.pcm' -o -iname '*.ogg' \) | grep -q .; then
    echo "normal operation persisted audio" >&2
    exit 1
fi
grep -q '"schema_version":1' "$work_dir/diagnostics.json"
grep -q '"privacy":' "$work_dir/diagnostics.json"
if grep -Eq '"(audio|environment|upload_url)"[[:space:]]*:' "$work_dir/diagnostics.json"; then
    echo "diagnostics exposed a forbidden payload field" >&2
    exit 1
fi

kill "$bus_pid"
bus_pid=""
for _attempt in $(seq 1 100); do
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        wait "$daemon_pid"
        daemon_pid=""
        break
    fi
    sleep 0.02
done
test -z "$daemon_pid"

echo "NOIRE_PHASE7_SESSION network_sockets=0 audio_files=0 diagnostics_privacy=pass session_logout=clean"
