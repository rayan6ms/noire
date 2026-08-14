#!/usr/bin/env bash
set -euo pipefail

if [[ "${NOIRE_PHASE8_DISPOSABLE_VM:-0}" != "1" ]]; then
    echo "Refusing package installation without NOIRE_PHASE8_DISPOSABLE_VM=1" >&2
    exit 2
fi
if [[ "$#" != "2" ]]; then
    echo "usage: $0 <deb|rpm> <package-dir>" >&2
    exit 2
fi
if [[ "$(id -u)" != "0" ]]; then
    echo "The packaged UI harness must run as root" >&2
    exit 2
fi
for tool in dbus-run-session dbus-send xvfb-run timeout ldd; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "The packaged UI harness requires $tool" >&2
        exit 2
    }
done

family="$1"
package_dir="$(realpath "$2")"
test_user="noire-package-ui-test"
work_dir="$(mktemp -d)"
installed_daemon=false
installed_ui=false

cleanup() {
    status=$?
    if [[ "$status" != 0 ]]; then
        for log in "$work_dir"/*; do
            [[ -f "$log" ]] || continue
            echo "--- $(basename "$log") ---" >&2
            sed -n '1,160p' "$log" >&2
        done
    fi
    if [[ "$installed_ui" == true ]]; then
        remove_ui >/dev/null 2>&1 || true
    fi
    if [[ "$installed_daemon" == true ]]; then
        remove_daemon >/dev/null 2>&1 || true
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
if id "$test_user" >/dev/null 2>&1; then
    echo "Disposable fixture user $test_user already exists" >&2
    exit 2
fi
useradd --create-home --shell /bin/bash "$test_user"
test_home="$(getent passwd "$test_user" | cut -d: -f6)"
chown "$test_user:$test_user" "$work_dir"
chmod 0700 "$work_dir"

case "$family" in
    deb)
        command -v apt-get >/dev/null 2>&1 || exit 2
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        install_daemon() {
            apt-get install --no-install-recommends --yes \
                "$package_dir"/noire-daemon_*.deb
        }
        install_ui() {
            apt-get install --no-install-recommends --yes \
                "$package_dir"/noire-ui_*.deb
        }
        remove_ui() { apt-get remove --yes noire-ui; }
        remove_daemon() { apt-get remove --yes noire-daemon; }
        gtk_installed() {
            dpkg-query -W -f='${db:Status-Abbrev}\n' libgtk-4-1 2>/dev/null |
                grep -q '^ii '
        }
        gtk_version() { dpkg-query -W -f='${Version}\n' libgtk-4-1; }
        gtk_meets_floor() {
            dpkg --compare-versions "$(gtk_version)" ge 4.10
        }
        package_owns_ui() {
            dpkg-query -S /usr/bin/noire | grep -Eq '^noire-ui: '
        }
        ;;
    rpm)
        command -v dnf >/dev/null 2>&1 || exit 2
        install_daemon() {
            dnf install --assumeyes --setopt=install_weak_deps=False \
                "$package_dir"/noire-daemon-*.rpm
        }
        install_ui() {
            dnf install --assumeyes --setopt=install_weak_deps=False \
                "$package_dir"/noire-ui-*.rpm
        }
        remove_ui() {
            dnf remove --assumeyes --setopt=clean_requirements_on_remove=False noire-ui
        }
        remove_daemon() {
            dnf remove --assumeyes --setopt=clean_requirements_on_remove=False noire-daemon
        }
        gtk_installed() { rpm -q gtk4 >/dev/null 2>&1; }
        gtk_version() { rpm -q --qf '%{VERSION}\n' gtk4; }
        gtk_meets_floor() {
            [[ "$(printf '%s\n' 4.10 "$(gtk_version)" | sort -V | head -n 1)" == 4.10 ]]
        }
        package_owns_ui() {
            [[ "$(rpm -qf --qf '%{NAME}\n' /usr/bin/noire)" == "noire-ui" ]]
        }
        ;;
    *)
        echo "Unsupported package family: $family" >&2
        exit 2
        ;;
esac

run_user() {
    runuser -u "$test_user" -- env HOME="$test_home" "$@"
}

assert_headless_boundary() {
    [[ ! -e /usr/bin/noire ]]
    gtk_installed && {
        echo "Headless daemon package unexpectedly installed GTK" >&2
        exit 1
    }
    for binary in /usr/bin/noired /usr/bin/noirectl; do
        [[ -x "$binary" ]]
        if ldd "$binary" 2>&1 | grep -E 'libgtk|not found' >/dev/null; then
            echo "Headless binary has a GTK or unresolved runtime dependency: $binary" >&2
            exit 1
        fi
    done
    run_user /usr/bin/noired --version | grep -Fx 'noired 1.0.0' >/dev/null
    run_user /usr/bin/noirectl --version | grep -Fx 'noirectl 1.0.0' >/dev/null
}

run_headless_session() {
    # The script and its variables intentionally expand in the inner user shell.
    # shellcheck disable=SC2016
    run_user dbus-run-session -- bash -eu -o pipefail -c '
        daemon_log="$1"
        /usr/bin/noired >"$daemon_log" 2>&1 &
        daemon_pid=$!
        cleanup_session() {
            kill "$daemon_pid" >/dev/null 2>&1 || true
            wait "$daemon_pid" >/dev/null 2>&1 || true
        }
        trap cleanup_session EXIT
        for _attempt in $(seq 1 50); do
            if dbus-send --session --dest=org.freedesktop.DBus \
                --type=method_call --print-reply /org/freedesktop/DBus \
                org.freedesktop.DBus.NameHasOwner \
                string:io.github.rayan6ms.Noire.Noire1 2>/dev/null | \
                grep -F "boolean true" >/dev/null; then
                break
            fi
            sleep 0.05
        done
        kill -0 "$daemon_pid"
        /usr/bin/noirectl --json status | grep -F schema_version >/dev/null
    ' bash "$work_dir/headless-daemon.log"
}

run_display_session() {
    # The script and its variables intentionally expand in the inner user shell.
    # shellcheck disable=SC2016
    run_user dbus-run-session -- bash -eu -o pipefail -c '
        daemon_log="$1"
        ui_log="$2"
        /usr/bin/noired >"$daemon_log" 2>&1 &
        daemon_pid=$!
        cleanup_session() {
            kill "$daemon_pid" >/dev/null 2>&1 || true
            wait "$daemon_pid" >/dev/null 2>&1 || true
        }
        trap cleanup_session EXIT
        for _attempt in $(seq 1 50); do
            if dbus-send --session --dest=org.freedesktop.DBus \
                --type=method_call --print-reply /org/freedesktop/DBus \
                org.freedesktop.DBus.NameHasOwner \
                string:io.github.rayan6ms.Noire.Noire1 2>/dev/null | \
                grep -F "boolean true" >/dev/null; then
                break
            fi
            sleep 0.05
        done
        kill -0 "$daemon_pid"
        /usr/bin/noirectl --json status | grep -F schema_version >/dev/null
        set +e
        GTK_A11Y=none xvfb-run --auto-servernum timeout --signal=TERM 3 \
            /usr/bin/noire >"$ui_log" 2>&1
        ui_status=$?
        set -e
        [[ "$ui_status" == 124 ]]
        kill -0 "$daemon_pid"
        /usr/bin/noirectl --json status | grep -F schema_version >/dev/null
        if pgrep -u "$(id -u)" -x noire >/dev/null; then
            echo "Packaged UI left a process after window shutdown" >&2
            exit 1
        fi
    ' bash "$work_dir/daemon.log" "$work_dir/ui.log"
}

install_daemon
installed_daemon=true
assert_headless_boundary
run_headless_session

install_ui
installed_ui=true
gtk_installed || {
    echo "UI package did not install GTK" >&2
    exit 1
}
gtk_meets_floor || {
    echo "Installed GTK runtime is older than 4.10: $(gtk_version)" >&2
    exit 1
}
package_owns_ui
run_user /usr/bin/noire --version | grep -Fx 'noire 1.0.0' >/dev/null
if ldd /usr/bin/noire 2>&1 | grep -F 'not found' >/dev/null; then
    echo "Installed UI has an unresolved runtime dependency" >&2
    exit 1
fi
installed_gtk_version="$(gtk_version)"

set +e
run_user env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE \
    timeout --signal=TERM 3 /usr/bin/noire \
    >"$work_dir/no-display.out" 2>"$work_dir/no-display.err"
no_display_status=$?
set -e
[[ "$no_display_status" != 0 ]] || {
    echo "Packaged UI unexpectedly stayed running without a display" >&2
    exit 1
}
[[ "$no_display_status" != 124 ]] || {
    echo "Packaged UI did not exit promptly without a display" >&2
    exit 1
}
grep -Eqi 'display|graphical session' "$work_dir/no-display.err" || {
    echo "Packaged UI did not explain its no-display failure" >&2
    exit 1
}

run_display_session

remove_ui
installed_ui=false
[[ ! -e /usr/bin/noire ]]
run_user /usr/bin/noired --version | grep -Fx 'noired 1.0.0' >/dev/null
run_user /usr/bin/noirectl --version | grep -Fx 'noirectl 1.0.0' >/dev/null
run_headless_session

echo "NOIRE_PHASE8_PACKAGED_UI family=$family headless_no_gtk=pass headless_dbus=pass gtk_version=$installed_gtk_version no_display=clear-failure xvfb_runtime=pass ui_exit_cleanup=pass ui_remove_headless=pass"
