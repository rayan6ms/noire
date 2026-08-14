#!/usr/bin/env bash
set -euo pipefail

if [[ "${NOIRE_PHASE9_DISPOSABLE_VM:-0}" != "1" ]]; then
    echo "Refusing service lifecycle changes without NOIRE_PHASE9_DISPOSABLE_VM=1" >&2
    exit 2
fi
if [[ "$(id -u)" != "0" ]]; then
    echo "The disposable-VM service harness must run as root" >&2
    exit 2
fi

daemon_binary="${NOIRE_PHASE9_DAEMON_BINARY:-}"
if [[ -z "$daemon_binary" || ! -x "$daemon_binary" ]]; then
    echo "NOIRE_PHASE9_DAEMON_BINARY must name an executable runtime-enabled noired" >&2
    exit 2
fi

unit="data/systemd/user/noire.service"
test_user="noirephase9"
if id "$test_user" >/dev/null 2>&1; then
    echo "Disposable fixture user $test_user already exists" >&2
    exit 2
fi

install -m 0755 "$daemon_binary" /usr/bin/noired
install -d /usr/lib/systemd/user
install -m 0644 "$unit" /usr/lib/systemd/user/noire.service
systemctl daemon-reload
useradd --create-home "$test_user"
test_uid="$(id -u "$test_user")"
loginctl enable-linger "$test_user"

for _attempt in $(seq 1 50); do
    [[ -S "/run/user/$test_uid/bus" ]] && break
    sleep 0.1
done
if [[ ! -S "/run/user/$test_uid/bus" ]]; then
    echo "Disposable user manager did not create its session bus" >&2
    exit 1
fi

userctl() {
    runuser -u "$test_user" -- env \
        XDG_RUNTIME_DIR="/run/user/$test_uid" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$test_uid/bus" \
        systemctl --user "$@"
}

userbus() {
    runuser -u "$test_user" -- env \
        XDG_RUNTIME_DIR="/run/user/$test_uid" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$test_uid/bus" \
        busctl --user "$@"
}

userctl daemon-reload
if userctl is-enabled --quiet noire.service; then
    echo "Fresh package unexpectedly enabled noire.service" >&2
    exit 1
fi
userctl enable noire.service >/dev/null

# Recreate the user manager to exercise default.target as a login would.
systemctl stop "user@$test_uid.service"
systemctl start "user@$test_uid.service"
for _attempt in $(seq 1 50); do
    if userctl is-active --quiet noire.service 2>/dev/null; then
        break
    fi
    sleep 0.1
done
userctl is-enabled --quiet noire.service
userctl is-active --quiet noire.service
userbus --no-pager status io.github.rayan6ms.Noire.Noire1 >/dev/null

main_pid="$(userctl show noire.service --property=MainPID --value)"
process_uid="$(ps -o uid= -p "$main_pid" | tr -d ' ')"
if [[ "$process_uid" != "$test_uid" ]]; then
    echo "User service ran as UID $process_uid instead of $test_uid" >&2
    exit 1
fi

# One unexpected failure must recover after RestartSec.
kill -KILL "$main_pid"
for _attempt in $(seq 1 50); do
    restarts="$(userctl show noire.service --property=NRestarts --value)"
    if userctl is-active --quiet noire.service && [[ "$restarts" -ge 1 ]]; then
        break
    fi
    sleep 0.1
done
userctl is-active --quiet noire.service
restarts="$(userctl show noire.service --property=NRestarts --value)"
if [[ "$restarts" -lt 1 ]]; then
    echo "noire.service did not restart after failure" >&2
    exit 1
fi

userctl stop noire.service
if userctl is-active --quiet noire.service; then
    echo "Explicit stop left noire.service active" >&2
    exit 1
fi
userctl disable noire.service >/dev/null
if userctl is-enabled --quiet noire.service; then
    echo "Explicit disable left noire.service enabled" >&2
    exit 1
fi

echo "NOIRE_PHASE9_UNIT static_verify=pass login=pass uid=$process_uid restart=$restarts stop=pass disable=pass"
