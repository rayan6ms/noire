#!/bin/sh
set -eu

if [ "${NOIRE_PHASE9_DISPOSABLE_VM:-0}" != 1 ]; then
    echo "Refusing package-manager changes without NOIRE_PHASE9_DISPOSABLE_VM=1" >&2
    exit 2
fi
if [ "$#" -ne 3 ]; then
    echo "usage: $0 <deb|rpm> <baseline-package-dir> <upgrade-package-dir>" >&2
    exit 2
fi
if [ "$(id -u)" -ne 0 ]; then
    echo "The disposable package-manager harness must run as root" >&2
    exit 2
fi

family=$1
baseline_dir=$2
upgrade_dir=$3
test_user=noire-package-test

[ -d "$baseline_dir" ] && [ -d "$upgrade_dir" ] || {
    echo "Both package directories must exist" >&2
    exit 2
}
baseline_dir=$(realpath "$baseline_dir")
upgrade_dir=$(realpath "$upgrade_dir")

if ! id "$test_user" >/dev/null 2>&1; then
    useradd --create-home --shell /bin/sh "$test_user"
fi
test_home=$(getent passwd "$test_user" | cut -d: -f6)
test_uid=$(id -u "$test_user")
test_gid=$(id -g "$test_user")
install -d -m 0700 -o "$test_uid" -g "$test_uid" "$test_home/.config/noire"
{
    echo 'schema_version = 1'
    echo 'active = false'
    echo 'launch_at_login = false'
    echo '# phase9-preservation-marker'
} >"$test_home/.config/noire/config.toml"
chown "$test_uid:$test_gid" "$test_home/.config/noire/config.toml"
chmod 0600 "$test_home/.config/noire/config.toml"
config_expected=$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)

assert_config_preserved() {
    current=$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)
    [ "$current" = "$config_expected" ] || {
        echo "Package operation changed pre-existing user configuration" >&2
        exit 1
    }
}

assert_no_user_payload() {
    if package_files | grep -Eq '^/(home|root)/'; then
        echo "A Noire package owns a path inside a user home" >&2
        exit 1
    fi
}

assert_not_started() {
    if command -v pgrep >/dev/null 2>&1 && pgrep -u "$test_uid" -x noired >/dev/null; then
        echo "Package operation unexpectedly started noired" >&2
        exit 1
    fi
}

run_version_as_test_user() {
    binary=$1
    if command -v runuser >/dev/null 2>&1; then
        runuser -u "$test_user" -- "$binary" --version
    elif command -v su >/dev/null 2>&1; then
        su -s /bin/sh -c "exec $binary --version" "$test_user"
    elif command -v chroot >/dev/null 2>&1; then
        chroot --userspec="$test_uid:$test_gid" / "$binary" --version
    else
        echo "No supported privilege-drop tool is available for the runtime check" >&2
        exit 1
    fi
}

assert_binaries() {
    run_version_as_test_user /usr/bin/noired | grep -Fx 'noired 1.0.0' >/dev/null
    run_version_as_test_user /usr/bin/noirectl | grep -Fx 'noirectl 1.0.0' >/dev/null
    run_version_as_test_user /usr/bin/noire | grep -Fx 'noire 1.0.0' >/dev/null
    if ldd /usr/bin/noired /usr/bin/noirectl /usr/bin/noire 2>&1 | grep -F 'not found' >/dev/null; then
        echo "An installed Noire binary has an unresolved shared library" >&2
        exit 1
    fi
}

assert_disabled_by_default() {
    [ ! -e "$test_home/.config/systemd/user/default.target.wants/noire.service" ] || {
        echo "Fresh package install unexpectedly enabled noire.service" >&2
        exit 1
    }
}

case "$family" in
    deb)
        command -v apt-get >/dev/null 2>&1 || exit 2
        export DEBIAN_FRONTEND=noninteractive
        package_files() {
            dpkg-query -L noire-daemon noire-ui noire 2>/dev/null || true
        }
        install_headless() {
            apt-get update
            apt-get install --no-install-recommends --yes "$1"/noire-daemon_*.deb
        }
        install_full() {
            apt-get install --no-install-recommends --yes \
                "$1"/noire-daemon_*.deb "$1"/noire-ui_*.deb "$1"/noire_*.deb
        }
        upgrade_full() {
            apt-get install --no-install-recommends --yes \
                "$1"/noire-daemon_*.deb "$1"/noire-ui_*.deb "$1"/noire_*.deb
        }
        downgrade_full() {
            apt-get install --no-install-recommends --allow-downgrades --yes \
                "$1"/noire-daemon_*.deb "$1"/noire-ui_*.deb "$1"/noire_*.deb
        }
        remove_all() {
            apt-get remove --yes noire noire-ui noire-daemon
        }
        purge_all() {
            apt-get purge --yes noire noire-ui noire-daemon
        }
        ui_runtime_installed() {
            dpkg-query -W -f='${db:Status-Abbrev}\n' libgtk-4-1 2>/dev/null |
                grep -q '^ii '
        }
        package_version() {
            dpkg-query -W -f='${Version}\n' noire
        }
        ;;
    rpm)
        command -v dnf >/dev/null 2>&1 || exit 2
        package_files() {
            rpm -ql noire-daemon noire-ui noire 2>/dev/null || true
        }
        install_headless() {
            dnf install --assumeyes --setopt=install_weak_deps=False \
                "$1"/noire-daemon-*.rpm
        }
        install_full() {
            dnf install --assumeyes --setopt=install_weak_deps=False \
                "$1"/noire-daemon-*.rpm \
                "$1"/noire-ui-*.rpm "$1"/noire-[0-9]*.rpm
        }
        upgrade_full() {
            dnf upgrade --assumeyes --setopt=install_weak_deps=False \
                "$1"/noire-daemon-*.rpm \
                "$1"/noire-ui-*.rpm "$1"/noire-[0-9]*.rpm
        }
        downgrade_full() {
            dnf downgrade --assumeyes --setopt=install_weak_deps=False \
                "$1"/noire-daemon-*.rpm \
                "$1"/noire-ui-*.rpm "$1"/noire-[0-9]*.rpm
        }
        remove_all() {
            dnf remove --assumeyes --setopt=clean_requirements_on_remove=False \
                noire noire-ui noire-daemon
        }
        purge_all() {
            remove_all
        }
        ui_runtime_installed() {
            rpm -q gtk4 >/dev/null 2>&1
        }
        package_version() {
            rpm -q --qf '%{VERSION}-%{RELEASE}\n' noire
        }
        ;;
    *)
        echo "Unsupported package family: $family" >&2
        exit 2
        ;;
esac

if ui_runtime_installed; then
    echo "Disposable baseline unexpectedly contains the GTK runtime" >&2
    exit 1
fi

install_headless "$baseline_dir"
ui_runtime_installed && {
    echo "Headless package unexpectedly installed the GTK runtime" >&2
    exit 1
}
run_version_as_test_user /usr/bin/noired | grep -Fx 'noired 1.0.0' >/dev/null
run_version_as_test_user /usr/bin/noirectl | grep -Fx 'noirectl 1.0.0' >/dev/null
assert_config_preserved
assert_disabled_by_default
assert_not_started

install_full "$baseline_dir"
ui_runtime_installed || {
    echo "Full package did not install the GTK runtime" >&2
    exit 1
}
assert_binaries
assert_no_user_payload
assert_config_preserved
assert_disabled_by_default
assert_not_started
baseline_version=$(package_version)

upgrade_full "$upgrade_dir"
upgrade_version=$(package_version)
[ "$upgrade_version" != "$baseline_version" ] || {
    echo "Package-manager upgrade did not change the package revision" >&2
    exit 1
}
assert_binaries
assert_config_preserved
assert_disabled_by_default
assert_not_started

{
    echo 'schema_version = 99'
    echo 'future_only = "phase9-downgrade-refusal"'
} >"$test_home/.config/noire/config.toml"
chown "$test_uid:$test_gid" "$test_home/.config/noire/config.toml"
chmod 0600 "$test_home/.config/noire/config.toml"
config_expected=$(sha256sum "$test_home/.config/noire/config.toml" | cut -d' ' -f1)

downgrade_full "$baseline_dir"
downgrade_version=$(package_version)
[ "$downgrade_version" = "$baseline_version" ] || {
    echo "Package-manager downgrade did not restore the baseline revision" >&2
    exit 1
}
assert_binaries
assert_config_preserved
assert_disabled_by_default
assert_not_started

remove_all
[ ! -e /usr/bin/noired ] && [ ! -e /usr/bin/noirectl ] && [ ! -e /usr/bin/noire ]
assert_config_preserved
assert_not_started

install_full "$upgrade_dir"
assert_binaries
assert_config_preserved
assert_disabled_by_default
assert_not_started

purge_all
[ ! -e /usr/bin/noired ] && [ ! -e /usr/bin/noirectl ] && [ ! -e /usr/bin/noire ]
assert_config_preserved
assert_not_started

echo "NOIRE_PHASE9_PACKAGE_MANAGER family=$family headless=pass full=pass upgrade=pass downgrade=pass future_config=preserved remove=pass reinstall=pass config=preserved"
