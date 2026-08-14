#!/usr/bin/env bash
set -euo pipefail

if [[ "${NOIRE_PHASE9_DISPOSABLE_VM:-0}" != "1" ]]; then
    echo "Refusing packaged service changes without NOIRE_PHASE9_DISPOSABLE_VM=1" >&2
    exit 2
fi
if [[ "$#" != "2" ]]; then
    echo "usage: $0 <deb|rpm> <package-dir>" >&2
    exit 2
fi
if [[ "$(id -u)" != "0" ]]; then
    echo "The packaged service harness must run as root" >&2
    exit 2
fi
if ! systemctl is-system-running --wait >/dev/null 2>&1; then
    state="$(systemctl is-system-running 2>/dev/null || true)"
    if [[ "$state" != "degraded" ]]; then
        echo "A running disposable systemd system manager is required" >&2
        exit 2
    fi
fi

family="$1"
package_dir="$(realpath "$2")"
test_user="noire-package-service-test"
work_dir="$(mktemp -d)"
installed=false
test_uid=""

cleanup() {
    if [[ -n "$test_uid" ]]; then
        systemctl stop "user@$test_uid.service" >/dev/null 2>&1 || true
        loginctl disable-linger "$test_user" >/dev/null 2>&1 || true
    fi
    if [[ "$installed" == true ]]; then
        case "$family" in
            deb) apt-get remove --yes noire-daemon >/dev/null 2>&1 || true ;;
            rpm)
                dnf remove --assumeyes \
                    --setopt=clean_requirements_on_remove=False \
                    noire-daemon >/dev/null 2>&1 || true
                ;;
        esac
    fi
    find "$work_dir" -depth -delete
}
trap cleanup EXIT

[[ -d "$package_dir" ]] || {
    echo "Package directory does not exist: $package_dir" >&2
    exit 2
}
if id "$test_user" >/dev/null 2>&1; then
    echo "Disposable fixture user $test_user already exists" >&2
    exit 2
fi

useradd --create-home --shell /bin/bash "$test_user"
test_uid="$(id -u "$test_user")"
test_gid="$(id -g "$test_user")"
test_home="$(getent passwd "$test_user" | cut -d: -f6)"
install -d -m 0700 -o "$test_uid" -g "$test_gid" "$test_home/.config"
install -d -m 0700 -o "$test_uid" -g "$test_gid" "$test_home/.config/noire"
cat >"$work_dir/config.toml" <<'EOF'
schema_version = 1
active = false
launch_at_login = false
# packaged-service-preservation-marker
EOF
install -m 0600 -o "$test_uid" -g "$test_gid" \
    "$work_dir/config.toml" "$test_home/.config/noire/config.toml"
config_before_install="$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)"

case "$family" in
    deb)
        command -v apt-get >/dev/null 2>&1 || exit 2
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        apt-get install --no-install-recommends --yes "$package_dir"/noire-daemon_*.deb
        package_owns() { dpkg-query -S "$1" | grep -Eq '^noire-daemon: '; }
        remove_package() { apt-get remove --yes noire-daemon; }
        ;;
    rpm)
        command -v dnf >/dev/null 2>&1 || exit 2
        dnf install --assumeyes --setopt=install_weak_deps=False \
            "$package_dir"/noire-daemon-*.rpm
        package_owns() { [[ "$(rpm -qf --qf '%{NAME}\n' "$1")" == "noire-daemon" ]]; }
        remove_package() {
            dnf remove --assumeyes \
                --setopt=clean_requirements_on_remove=False noire-daemon
        }
        ;;
    *)
        echo "Unsupported package family: $family" >&2
        exit 2
        ;;
esac
installed=true

for path in \
    /usr/bin/noired \
    /usr/bin/noirectl \
    /usr/lib/systemd/user/noire.service \
    /usr/share/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service; do
    [[ -e "$path" ]] || {
        echo "Installed daemon package is missing $path" >&2
        exit 1
    }
    package_owns "$path" || {
        echo "noire-daemon does not own $path" >&2
        exit 1
    }
done
[[ "$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)" == \
    "$config_before_install" ]] || {
    echo "Package install changed pre-existing user configuration" >&2
    exit 1
}

systemctl daemon-reload
loginctl enable-linger "$test_user"
systemctl start "user-runtime-dir@$test_uid.service"
systemctl start "user@$test_uid.service"

wait_for_bus() {
    for _attempt in $(seq 1 100); do
        [[ -S "/run/user/$test_uid/bus" ]] && return
        sleep 0.1
    done
    echo "Disposable user manager did not create its session bus" >&2
    exit 1
}

run_as_user() {
    runuser -u "$test_user" -- env \
        HOME="$test_home" \
        XDG_RUNTIME_DIR="/run/user/$test_uid" \
        PIPEWIRE_RUNTIME_DIR="/run/user/$test_uid" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$test_uid/bus" \
        "$@"
}

userctl() {
    run_as_user systemctl --user "$@"
}

process_count() {
    { pgrep -u "$test_uid" -x noired 2>/dev/null || true; } | wc -l
}

noire_node_count() {
    run_as_user pw-cli list-objects Node 2>/dev/null |
        grep -Fc 'node.name = "io.github.rayan6ms.Noire.Microphone"' || true
}

wait_for_bus
userctl daemon-reload
if userctl is-enabled --quiet noire.service; then
    echo "Installed package unexpectedly enabled noire.service" >&2
    exit 1
fi
if userctl is-active --quiet noire.service; then
    echo "Installed package unexpectedly started noire.service" >&2
    exit 1
fi

# Concurrent first clients must converge on systemd/D-Bus activation of exactly
# one daemon. This is the installed-metadata race that static unit checks cannot
# establish.
client_pids=()
for client in $(seq 1 8); do
    run_as_user timeout 10 /usr/bin/noirectl --json status \
        >"$work_dir/client-$client.json" \
        2>"$work_dir/client-$client.err" &
    client_pids+=("$!")
done
for client_pid in "${client_pids[@]}"; do
    wait "$client_pid"
done
for client in $(seq 1 8); do
    grep -F '"schema_version":1' "$work_dir/client-$client.json" >/dev/null
    [[ ! -s "$work_dir/client-$client.err" ]]
done
userctl is-active --quiet noire.service
[[ "$(process_count)" == "1" ]] || {
    echo "Concurrent D-Bus activation did not converge on exactly one daemon" >&2
    exit 1
}

activation_pid="$(userctl show noire.service --property=MainPID --value)"
activation_uid="$(ps -o uid= -p "$activation_pid" | tr -d ' ')"
[[ "$activation_uid" == "$test_uid" ]] || {
    echo "Activated service ran as UID $activation_uid instead of $test_uid" >&2
    exit 1
}

run_as_user /usr/bin/noirectl --json set launch-at-login true \
    >"$work_dir/enable.json"
userctl is-enabled --quiet noire.service

# Recreate the manager to exercise default.target as a login would.
systemctl stop "user@$test_uid.service"
systemctl start "user-runtime-dir@$test_uid.service"
systemctl start "user@$test_uid.service"
wait_for_bus
for _attempt in $(seq 1 100); do
    if userctl is-active --quiet noire.service 2>/dev/null; then
        break
    fi
    sleep 0.1
done
userctl is-enabled --quiet noire.service
userctl is-active --quiet noire.service
[[ "$(process_count)" == "1" ]] || {
    echo "Login activation did not converge on exactly one daemon" >&2
    exit 1
}

userctl start pipewire.service wireplumber.service
userctl is-active --quiet pipewire.service
userctl is-active --quiet wireplumber.service
run_as_user pw-cli create-node adapter \
    '{ factory.name=support.null-audio-sink node.name=noire.test.source node.description="Noire Test Source" media.class=Audio/Source object.linger=true audio.position=[ MONO ] }'
for _attempt in $(seq 1 50); do
    if run_as_user /usr/bin/noirectl --json devices 2>/dev/null |
        grep -F '"stable_id":"noire.test.source"' >/dev/null; then
        break
    fi
    sleep 0.1
done
run_as_user /usr/bin/noirectl --json devices |
    grep -F '"stable_id":"noire.test.source"' >/dev/null
run_as_user /usr/bin/noirectl --json set input noire.test.source \
    >"$work_dir/select-input.json"
run_as_user /usr/bin/noirectl --json start >"$work_dir/start.json"
for _attempt in $(seq 1 50); do
    [[ "$(noire_node_count)" == "1" ]] && break
    sleep 0.1
done
[[ "$(noire_node_count)" == "1" ]] || {
    echo "Active packaged daemon did not publish exactly one Noire source" >&2
    exit 1
}

run_as_user /usr/bin/noirectl --json stop >"$work_dir/stop.json"
for _attempt in $(seq 1 50); do
    [[ "$(noire_node_count)" == "0" ]] && break
    sleep 0.1
done
[[ "$(noire_node_count)" == "0" ]] || {
    echo "Explicit stop left a stale Noire PipeWire source" >&2
    exit 1
}
run_as_user /usr/bin/noirectl --json set launch-at-login false \
    >"$work_dir/disable.json"
userctl is-enabled --quiet noire.service && {
    echo "Installed CLI did not disable launch at login" >&2
    exit 1
}
userctl stop noire.service
[[ "$(process_count)" == "0" ]] || {
    echo "Explicit stop left a packaged daemon process running" >&2
    exit 1
}

# A package manager cannot safely inspect every user's home before a downgrade.
# The installed older daemon is therefore the enforcement boundary: it must load
# safe defaults, expose the incompatibility, reject mutation, and never rewrite
# a config created by a newer schema.
cat >"$work_dir/future-config.toml" <<'EOF'
schema_version = 99
future_only = "phase9-downgrade-refusal"
EOF
install -m 0600 -o "$test_uid" -g "$test_gid" \
    "$work_dir/future-config.toml" "$test_home/.config/noire/config.toml"
future_config_hash="$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)"

run_as_user timeout 10 /usr/bin/noirectl --json status \
    >"$work_dir/future-status.json"
grep -F '"active":false' "$work_dir/future-status.json" >/dev/null
grep -F '"has_error":true' "$work_dir/future-status.json" >/dev/null
grep -F '"code":"config-newer-schema"' "$work_dir/future-status.json" >/dev/null
grep -F 'newer incompatible daemon' "$work_dir/future-status.json" >/dev/null
[[ "$(noire_node_count)" == "0" ]] || {
    echo "Newer-schema safe-default startup published a Noire source" >&2
    exit 1
}

if run_as_user timeout 10 /usr/bin/noirectl --json set strength 0.25 \
    >"$work_dir/future-unexpected.json" 2>"$work_dir/future-rejected.json"; then
    echo "Packaged daemon accepted a mutation against a newer config schema" >&2
    exit 1
fi
grep -F '"schema_version":1' "$work_dir/future-rejected.json" >/dev/null
grep -F '"code":"persistence"' "$work_dir/future-rejected.json" >/dev/null
grep -F 'read-only' "$work_dir/future-rejected.json" >/dev/null
[[ "$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)" == \
    "$future_config_hash" ]] || {
    echo "Packaged daemon rewrote a newer config schema" >&2
    exit 1
}
userctl stop noire.service
[[ "$(process_count)" == "0" ]] || {
    echo "Newer-schema test left a packaged daemon process running" >&2
    exit 1
}

config_before_remove="$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)"
remove_package
installed=false
[[ ! -e /usr/bin/noired ]] && [[ ! -e /usr/bin/noirectl ]]
[[ "$(process_count)" == "0" ]]
[[ "$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)" == \
    "$config_before_remove" ]] || {
    echo "Package removal changed user configuration" >&2
    exit 1
}

echo "NOIRE_PHASE9_PACKAGED_SERVICE family=$family ownership=pass activation_clients=8 process_count=1 login=pass graph_start=1 graph_stop=0 newer_schema=read-only downgrade_refusal=pass uninstall=pass config=preserved"
