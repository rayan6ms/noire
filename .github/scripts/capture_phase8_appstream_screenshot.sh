#!/usr/bin/env bash
set -euo pipefail

if [[ "${NOIRE_PHASE8_SCREENSHOT_VM:-0}" != "1" ]]; then
    echo "Refusing package installation without NOIRE_PHASE8_SCREENSHOT_VM=1" >&2
    exit 2
fi
if [[ "$#" != "2" ]]; then
    echo "usage: $0 <deb-package-dir> <output.png>" >&2
    exit 2
fi
if [[ "$(id -u)" != 0 ]]; then
    echo "The AppStream screenshot harness must run as root" >&2
    exit 2
fi
for tool in apt-get dbus-run-session runuser xvfb-run; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "The AppStream screenshot harness requires $tool" >&2
        exit 2
    }
done

package_dir="$(realpath "$1")"
output="$(realpath -m "$2")"
test_user="noire-screenshot-test"
work_dir="$(mktemp -d)"

cleanup() {
    status=$?
    if [[ "$status" != 0 ]]; then
        for log in "$work_dir"/*.log; do
            [[ -f "$log" ]] || continue
            echo "--- $(basename "$log") ---" >&2
            sed -n '1,160p' "$log" >&2
        done
    fi
    find "$work_dir" -depth -delete
    trap - EXIT
    exit "$status"
}
trap cleanup EXIT

[[ -d "$package_dir" ]] || {
    echo "Package directory does not exist: $package_dir" >&2
    exit 2
}
[[ "${output##*.}" == png ]] || {
    echo "Screenshot output must use a .png extension" >&2
    exit 2
}
mkdir -p "$(dirname "$output")"

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install --no-install-recommends --yes \
    "$package_dir"/noire-daemon_*.deb \
    "$package_dir"/noire-ui_*.deb \
    imagemagick \
    xdotool

for tool in identify import convert pw-cli pipewire wireplumber; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "Installed screenshot environment is missing $tool" >&2
        exit 1
    }
done

useradd --create-home --shell /bin/bash "$test_user"
test_home="$(getent passwd "$test_user" | cut -d: -f6)"
runtime_dir="$work_dir/runtime"
mkdir -p "$runtime_dir"
chown -R "$test_user:$test_user" "$work_dir"
chmod 0700 "$work_dir" "$runtime_dir"

# The script and its variables intentionally expand in the inner user shell.
# shellcheck disable=SC2016
runuser -u "$test_user" -- env \
    HOME="$test_home" \
    XDG_RUNTIME_DIR="$runtime_dir" \
    dbus-run-session -- bash -eu -o pipefail -c '
        work_dir="$1"
        pipewire >"$work_dir/pipewire.log" 2>&1 &
        pipewire_pid=$!
        wireplumber >"$work_dir/wireplumber.log" 2>&1 &
        wireplumber_pid=$!
        /usr/bin/noired >"$work_dir/daemon.log" 2>&1 &
        daemon_pid=$!
        cleanup_session() {
            for pid in "$daemon_pid" "$wireplumber_pid" "$pipewire_pid"; do
                kill "$pid" >/dev/null 2>&1 || true
            done
            wait "$daemon_pid" >/dev/null 2>&1 || true
            wait "$wireplumber_pid" >/dev/null 2>&1 || true
            wait "$pipewire_pid" >/dev/null 2>&1 || true
        }
        trap cleanup_session EXIT

        for _attempt in $(seq 1 100); do
            if pw-cli info 0 >/dev/null 2>&1 && \
                /usr/bin/noirectl --json status >/dev/null 2>&1; then
                break
            fi
            sleep 0.05
        done
        pw-cli info 0 >/dev/null
        /usr/bin/noirectl --json status >/dev/null

        pw-cli create-node adapter \
            "{ factory.name=support.null-audio-sink node.name=noire.screenshot.source node.description=\"Studio Microphone\" media.class=Audio/Source object.linger=true audio.position=[ MONO ] }" \
            >"$work_dir/source.log"
        for _attempt in $(seq 1 100); do
            if /usr/bin/noirectl --json devices 2>/dev/null | \
                grep -F "noire.screenshot.source" >/dev/null; then
                break
            fi
            sleep 0.05
        done
        /usr/bin/noirectl --json set input noire.screenshot.source >/dev/null
        /usr/bin/noirectl --json start >/dev/null
        for _attempt in $(seq 1 100); do
            if /usr/bin/noirectl --json status 2>/dev/null | \
                grep -F "\"state\":\"running\"" >/dev/null; then
                break
            fi
            sleep 0.05
        done
        /usr/bin/noirectl --json status | grep -F "\"state\":\"running\"" >/dev/null

        GTK_A11Y=none xvfb-run --auto-servernum \
            --server-args="-screen 0 1280x720x24 -nolisten tcp" \
            bash -eu -o pipefail -c "
                GSK_RENDERER=cairo /usr/bin/noire >\"$work_dir/ui.log\" 2>&1 &
                ui_pid=\$!
                cleanup_ui() {
                    kill \"\$ui_pid\" >/dev/null 2>&1 || true
                    wait \"\$ui_pid\" >/dev/null 2>&1 || true
                }
                trap cleanup_ui EXIT
                window_id=\"\"
                for _attempt in \$(seq 1 100); do
                    window_id=\$(xdotool search --onlyvisible --name '^Noire$' 2>/dev/null | head -n 1 || true)
                    [[ -n \"\$window_id\" ]] && break
                    sleep 0.05
                done
                [[ -n \"\$window_id\" ]]
                xdotool windowsize --sync \"\$window_id\" 640 640
                sleep 0.75
                import -window \"\$window_id\" \
                    -define png:exclude-chunk=date,time \
                    \"$work_dir/window.png\"
            "
    ' bash "$work_dir"

convert "$work_dir/window.png" \
    -bordercolor '#d8d8d8' -border 1 \
    -background '#f6f5f4' -gravity center -extent 1280x720 \
    -strip -define png:exclude-chunk=date,time \
    "$work_dir/appstream.png"

dimensions="$(identify -format '%wx%h' "$work_dir/appstream.png")"
[[ "$dimensions" == 1280x720 ]]
identify -format '%m' "$work_dir/appstream.png" | grep -Fx PNG >/dev/null
install -m 0644 "$work_dir/appstream.png" "$output"

echo "NOIRE_PHASE8_APPSTREAM_SCREENSHOT package=deb state=running dimensions=$dimensions capture=actual-window"
