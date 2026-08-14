#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

for tool in cargo dbus-run-session gdbus timeout weston xvfb-run; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "Phase-8 accessibility preflight requires $tool" >&2
        exit 2
    }
done

work_dir="$(mktemp -d)"
weston_pid=""
app_pid=""
cleanup() {
    status=$?
    for pid in "$app_pid" "$weston_pid"; do
        [[ -n "$pid" ]] || continue
        kill "$pid" >/dev/null 2>&1 || true
        wait "$pid" >/dev/null 2>&1 || true
    done
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

cargo build --package noire-ui --features gtk-ui --locked
target_dir="${CARGO_TARGET_DIR:-target}"
binary="$target_dir/debug/noire"
[[ -x "$binary" ]] || {
    echo "GTK UI binary was not produced at $binary" >&2
    exit 1
}

env -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE -u DBUS_SESSION_BUS_ADDRESS \
    GDK_BACKEND=x11 \
    GDK_SCALE=2 \
    GSK_RENDERER=cairo \
    GTK_A11Y=none \
    GTK_THEME=HighContrast \
    NOIRE_PHASE8_GDK_BACKEND=x11 \
    dbus-run-session -- xvfb-run --auto-servernum \
        cargo test --package noire-ui --features gtk-ui --locked \
            app::tests::rtl_high_contrast_scaled_layout_remains_operable \
            -- --ignored --exact --nocapture

# Activate GTK's AT-SPI bridge on an isolated bus and prove that the application
# remains alive while the accessibility bus publishes an address. Human speech
# output and desktop-specific navigation are deliberately left to the review.
# The inner variables intentionally expand in the isolated session shell.
# shellcheck disable=SC2016
env -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE -u DBUS_SESSION_BUS_ADDRESS \
    GDK_BACKEND=x11 \
    GDK_SCALE=2 \
    GSK_RENDERER=cairo \
    GTK_A11Y=atspi \
    GTK_THEME=HighContrast \
    dbus-run-session -- xvfb-run --auto-servernum bash -eu -o pipefail -c '
        binary="$1"
        "$binary" >"$2/atspi-ui.log" 2>&1 &
        app_pid=$!
        cleanup_app() {
            kill "$app_pid" >/dev/null 2>&1 || true
            wait "$app_pid" >/dev/null 2>&1 || true
        }
        trap cleanup_app EXIT
        for _attempt in $(seq 1 50); do
            if kill -0 "$app_pid" 2>/dev/null && \
                gdbus call --session \
                    --dest org.a11y.Bus \
                    --object-path /org/a11y/bus \
                    --method org.a11y.Bus.GetAddress 2>/dev/null |
                    grep -q "unix:"; then
                echo "NOIRE_PHASE8_ATSPI bridge=pass app_alive=pass"
                exit 0
            fi
            sleep 0.1
        done
        echo "GTK application did not connect to a live AT-SPI bus" >&2
        exit 1
    ' bash "$binary" "$work_dir"

runtime_dir="$work_dir/wayland-runtime"
mkdir -p "$runtime_dir"
chmod 0700 "$runtime_dir"
XDG_RUNTIME_DIR="$runtime_dir" \
    weston --backend=headless-backend.so --socket=noire-wayland \
        --idle-time=0 --renderer=pixman --scale=2 \
        --log="$work_dir/weston.log" &
weston_pid=$!
for _attempt in $(seq 1 50); do
    [[ -S "$runtime_dir/noire-wayland" ]] && break
    kill -0 "$weston_pid" 2>/dev/null || {
        echo "Headless Weston exited before publishing its Wayland socket" >&2
        exit 1
    }
    sleep 0.1
done
[[ -S "$runtime_dir/noire-wayland" ]] || {
    echo "Headless Weston did not publish its Wayland socket" >&2
    exit 1
}

env -u DISPLAY -u XDG_SESSION_TYPE -u DBUS_SESSION_BUS_ADDRESS \
    XDG_RUNTIME_DIR="$runtime_dir" \
    WAYLAND_DISPLAY=noire-wayland \
    GDK_BACKEND=wayland \
    GDK_SCALE=2 \
    GSK_RENDERER=cairo \
    GTK_A11Y=none \
    GTK_THEME=HighContrast \
    NOIRE_PHASE8_GDK_BACKEND=wayland \
    dbus-run-session -- \
        cargo test --package noire-ui --features gtk-ui --locked \
            app::tests::rtl_high_contrast_scaled_layout_remains_operable \
            -- --ignored --exact --nocapture

echo "NOIRE_PHASE8_ACCESSIBILITY_PREFLIGHT x11=pass wayland=pass atspi=bridge-pass high_contrast=pass scale_200=pass rtl=pass focus=pass"
