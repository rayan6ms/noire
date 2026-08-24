#!/bin/sh
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT HUP INT TERM

app_dir="$work_dir/AppDir"
home_dir="$work_dir/home"
data_dir="$home_dir/data"
runtime_dir="$work_dir/run"
fake_appimage="$work_dir/Noire.AppImage"
mkdir -p "$app_dir/usr/bin" "$data_dir/applications" "$runtime_dir"
install -m 0755 "$repo_dir/packaging/appimage/AppRun" "$app_dir/AppRun"
# The generated script, rather than this parent, expands these variables.
# shellcheck disable=SC2016
{
    echo '#!/bin/sh'
    echo 'if [ "${NOIRE_SMOKE_RECORD_XDG:-}" = 1 ]; then'
    printf '%s\n' '    printf "%s\n" "$XDG_DATA_DIRS" >"$XDG_RUNTIME_DIR/xdg-data-dirs"'
    echo 'fi'
    echo 'if [ "${NOIRE_SMOKE_HOLD:-}" = 1 ]; then'
    echo '    printf ready >"$XDG_RUNTIME_DIR/controller-ready"'
    echo '    attempt=0'
    echo '    while [ ! -s "$NOIRE_ACTIVATION_FILE" ] && [ "$attempt" -lt 40 ]; do'
    echo '        sleep 0.05'
    echo '        attempt=$((attempt + 1))'
    echo '    done'
    echo '    cat "$NOIRE_ACTIVATION_FILE" >"$XDG_RUNTIME_DIR/activation-request"'
    echo '    exit 0'
    echo 'fi'
    echo "echo 'noire 1.1.0'"
} >"$app_dir/usr/bin/noire"
chmod 0755 "$app_dir/usr/bin/noire"
{
    echo '#!/bin/sh'
    echo 'exit 1'
} >"$app_dir/usr/bin/noirectl"
chmod 0755 "$app_dir/usr/bin/noirectl"
# The generated script, rather than this parent, expands these variables.
# shellcheck disable=SC2016
{
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'printf "%s\n" "${NOIRE_PORTABLE_CONTROLLER_PID:-}" >"$XDG_RUNTIME_DIR/controller-pid"'
} >"$app_dir/usr/bin/noired"
chmod 0755 "$app_dir/usr/bin/noired"
: >"$fake_appimage"
chmod 0755 "$fake_appimage"

integrated_launcher="$data_dir/applications/noire.desktop"
legacy_launcher="$data_dir/applications/io.github.rayan6ms.Noire.desktop"
{
    echo '[Desktop Entry]'
    echo 'Name=Noire'
    echo "TryExec=$fake_appimage"
    echo "Exec=env DESKTOPINTEGRATION=1 $fake_appimage"
    echo 'X-AppImage-Name=Noire'
} >"$integrated_launcher"
{
    echo '[Desktop Entry]'
    echo 'Name=Noire'
    echo "Exec=\"$fake_appimage\""
} >"$legacy_launcher"

# Opening either duplicate must remove the legacy launcher. In particular, the
# old launcher does not set DESKTOPINTEGRATION.
appimage_version=$(APPDIR="$app_dir" APPIMAGE="$fake_appimage" \
    HOME="$home_dir" XDG_DATA_HOME="$data_dir" XDG_RUNTIME_DIR="$runtime_dir" \
    "$app_dir/AppRun" --version)
[ "$appimage_version" = 'noire 1.1.0' ]
[ -f "$integrated_launcher" ]
[ ! -e "$legacy_launcher" ]

# AppRun must preserve the XDG specification's implicit host defaults. Vulkan
# loaders use these paths to find the user's graphics driver manifests, and an
# AppImage-private-only value makes GPUI fail before it can create a window.
env -u XDG_DATA_DIRS APPDIR="$app_dir" APPIMAGE="$fake_appimage" \
    HOME="$home_dir" NOIRE_SMOKE_RECORD_XDG=1 XDG_DATA_HOME="$data_dir" \
    XDG_RUNTIME_DIR="$runtime_dir" "$app_dir/AppRun" --version >/dev/null
[ "$(cat "$runtime_dir/xdg-data-dirs")" = \
    "$app_dir/usr/share:/usr/local/share:/usr/share" ]
XDG_DATA_DIRS=/opt/desktop/share APPDIR="$app_dir" APPIMAGE="$fake_appimage" \
    HOME="$home_dir" NOIRE_SMOKE_RECORD_XDG=1 XDG_DATA_HOME="$data_dir" \
    XDG_RUNTIME_DIR="$runtime_dir" "$app_dir/AppRun" --version >/dev/null
[ "$(cat "$runtime_dir/xdg-data-dirs")" = \
    "$app_dir/usr/share:/opt/desktop/share" ]

# Never remove a canonical launcher that points anywhere else.
{
    echo '[Desktop Entry]'
    echo 'Name=Noire'
    echo 'Exec="/different/Noire.AppImage"'
} >"$legacy_launcher"
APPDIR="$app_dir" APPIMAGE="$fake_appimage" HOME="$home_dir" \
    XDG_DATA_HOME="$data_dir" XDG_RUNTIME_DIR="$runtime_dir" \
    "$app_dir/AppRun" --version >/dev/null
[ -f "$legacy_launcher" ]

# A portable daemon receives the stable AppRun/controller PID so it can stop
# its audio graph and exit when the controller truly exits. The isolated
# runtime directory also proves this test cannot inspect or signal live user
# processes.
runtime_lifecycle="$work_dir/run-lifecycle"
mkdir -p "$runtime_lifecycle"
APPDIR="$app_dir" APPIMAGE="$app_dir/AppRun" HOME="$home_dir" \
    XDG_DATA_HOME="$data_dir" XDG_RUNTIME_DIR="$runtime_lifecycle" \
    "$app_dir/AppRun" >/dev/null
attempt=0
while [ ! -s "$runtime_lifecycle/controller-pid" ] && [ "$attempt" -lt 20 ]; do
    sleep 0.05
    attempt=$((attempt + 1))
done
controller_pid=$(cat "$runtime_lifecycle/controller-pid")
case "$controller_pid" in
    ''|*[!0-9]*) echo 'Portable controller PID was not propagated' >&2; exit 1 ;;
esac
[ "$controller_pid" -gt 1 ]

# A repeated launcher request must reach the existing controller rather than
# silently doing nothing or starting a second UI/tray instance.
runtime_activation="$work_dir/run-activation"
mkdir -p "$runtime_activation"
APPDIR="$app_dir" APPIMAGE="$app_dir/AppRun" HOME="$home_dir" \
    NOIRE_SMOKE_HOLD=1 XDG_DATA_HOME="$data_dir" XDG_RUNTIME_DIR="$runtime_activation" \
    "$app_dir/AppRun" >/dev/null &
controller=$!
attempt=0
while [ ! -s "$runtime_activation/controller-ready" ] && [ "$attempt" -lt 40 ]; do
    sleep 0.05
    attempt=$((attempt + 1))
done
[ -s "$runtime_activation/controller-ready" ]
APPDIR="$app_dir" APPIMAGE="$app_dir/AppRun" HOME="$home_dir" \
    NOIRE_SMOKE_HOLD=1 XDG_DATA_HOME="$data_dir" XDG_RUNTIME_DIR="$runtime_activation" \
    "$app_dir/AppRun" >/dev/null
wait "$controller"
[ "$(cat "$runtime_activation/activation-request")" = show ]

echo 'NOIRE_APPIMAGE_APPRUN legacy_migration=pass unrelated_launcher=preserved lifecycle=pass activation=pass'
