#!/bin/sh
set -eu

daemon=${1:?usage: run_portable_daemon_lifecycle_smoke.sh /path/to/noired}
case "$daemon" in
    /*) ;;
    *) daemon=$(CDPATH='' cd -- "$(dirname -- "$daemon")" && pwd)/$(basename -- "$daemon") ;;
esac
[ -x "$daemon" ]

work_dir=$(mktemp -d)
trap 'find "$work_dir" -depth -delete' EXIT HUP INT TERM

# The nested private-session shell expands this script body.
# shellcheck disable=SC2016
dbus-run-session -- sh -eu -c '
    work_dir=$1
    daemon_binary=$2
    sleep 0.4 &
    controller=$!
    XDG_CONFIG_HOME="$work_dir/config" \
        NOIRE_PORTABLE_CONTROLLER_PID=$controller \
        "$daemon_binary" >"$work_dir/daemon.log" 2>&1 &
    daemon_pid=$!
    wait "$controller"

    attempt=0
    while kill -0 "$daemon_pid" 2>/dev/null && [ "$attempt" -lt 30 ]; do
        sleep 0.1
        attempt=$((attempt + 1))
    done
    if kill -0 "$daemon_pid" 2>/dev/null; then
        kill -TERM "$daemon_pid" 2>/dev/null || true
        wait "$daemon_pid" 2>/dev/null || true
        echo "Portable daemon survived its controller" >&2
        exit 1
    fi
    wait "$daemon_pid"
' sh "$work_dir" "$daemon"

grep -F 'event="daemon.portable-controller-exited"' "$work_dir/daemon.log" >/dev/null
echo 'NOIRE_PORTABLE_DAEMON controller_lifetime=pass isolated_session=pass'
