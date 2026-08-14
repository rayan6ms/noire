#!/usr/bin/env bash
set -euo pipefail

if ! command -v xvfb-run >/dev/null || ! command -v dbus-run-session >/dev/null; then
    echo "Phase-8 UI smoke requires xvfb-run and dbus-run-session" >&2
    exit 2
fi

if ! command -v xgettext >/dev/null || ! command -v msgfmt >/dev/null; then
    echo "Phase-8 UI smoke requires GNU gettext tools" >&2
    exit 2
fi

pot_check=$(mktemp)
trap 'rm -f -- "$pot_check"' EXIT
po/update-pot.sh "$pot_check"
cmp po/noire.pot "$pot_check"
msgfmt --check-format --output-file=/dev/null "$pot_check"

cargo build --package noire-ui --features gtk-ui --locked
target_dir="${CARGO_TARGET_DIR:-target}"
binary="$target_dir/debug/noire"
if [[ ! -x "$binary" ]]; then
    echo "GTK UI binary was not produced at $binary" >&2
    exit 1
fi

env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE \
    -u DBUS_SESSION_BUS_ADDRESS \
    dbus-run-session -- \
        cargo test --package noire-ui --features gtk-ui --locked \
            client::tests::dbus_worker_converges_after_external_change_rejection_and_restart \
            -- --ignored --exact --nocapture

env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE \
    -u DBUS_SESSION_BUS_ADDRESS \
    dbus-run-session -- \
        cargo test --package noired --locked --test phase6_session \
            same_user_contract_rejects_stale_invalid_and_malformed_requests \
            -- --ignored --exact --nocapture

env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE \
    -u DBUS_SESSION_BUS_ADDRESS GTK_A11Y=none \
    dbus-run-session -- xvfb-run --auto-servernum \
        cargo test --package noire-ui --features gtk-ui --locked \
            app::tests::widget_state_matrix_tracks_daemon_truth_and_accessible_controls \
            -- --ignored --exact --nocapture

env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE \
    -u DBUS_SESSION_BUS_ADDRESS GTK_A11Y=none \
    dbus-run-session -- xvfb-run --auto-servernum \
        cargo test --package noire-ui --features gtk-ui --locked \
            app::tests::accessibility_tree_and_keyboard_paths_are_complete \
            -- --ignored --exact --nocapture

set +e
env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE \
    -u DBUS_SESSION_BUS_ADDRESS GTK_A11Y=none \
    dbus-run-session -- xvfb-run --auto-servernum \
        timeout --signal=TERM 3 "$binary"
status=$?
set -e

if [[ "$status" -ne 124 ]]; then
    echo "GTK UI exited unexpectedly without a daemon (status $status)" >&2
    exit 1
fi

echo "NOIRE_PHASE8_UI signals=pass meters=bounded-subscribed-pass reconnect=capped-pass diagnostics=pass help=pass about=pass i18n=pass accessibility=automated-pass daemon_absence=pass"
echo "Phase-8 GTK UI remained responsive without a daemon"
