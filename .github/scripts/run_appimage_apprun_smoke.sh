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
mkdir -p "$app_dir/usr/bin" "$app_dir/usr/share/applications" \
    "$app_dir/usr/share/icons/hicolor/scalable/apps" \
    "$data_dir/applications" "$runtime_dir"
install -m 0755 "$repo_dir/packaging/appimage/AppRun" "$app_dir/AppRun"
install -m 0644 "$repo_dir/data/applications/io.github.rayan6ms.Noire.desktop" \
    "$app_dir/usr/share/applications/io.github.rayan6ms.Noire.desktop"
install -m 0644 "$repo_dir/icons/noire.svg" \
    "$app_dir/usr/share/icons/hicolor/scalable/apps/io.github.rayan6ms.Noire.svg"
# The generated script, rather than this parent, expands these variables.
# shellcheck disable=SC2016
{
    echo '#!/bin/sh'
    echo 'if [ "${NOIRE_SMOKE_RECORD_XDG:-}" = 1 ]; then'
    printf '%s\n' '    printf "%s\n" "$XDG_DATA_DIRS" >"$XDG_RUNTIME_DIR/xdg-data-dirs"'
    echo 'fi'
    echo 'if [ "${NOIRE_SMOKE_HOLD:-}" = 1 ]; then'
    echo '    printf ready >"$XDG_RUNTIME_DIR/controller-ready"'
    echo '    activation_file="$XDG_RUNTIME_DIR/noire-controller.activate"'
    echo '    attempt=0'
    echo '    while [ ! -s "$activation_file" ] && [ "$attempt" -lt 40 ]; do'
    echo '        sleep 0.05'
    echo '        attempt=$((attempt + 1))'
    echo '    done'
    echo '    cat "$activation_file" >"$XDG_RUNTIME_DIR/activation-request"'
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

# Informational commands stay read-only, including when an old portable
# launcher exists.
appimage_version=$(APPDIR="$app_dir" APPIMAGE="$fake_appimage" \
    HOME="$home_dir" XDG_DATA_HOME="$data_dir" XDG_RUNTIME_DIR="$runtime_dir" \
    "$app_dir/AppRun" --version)
[ "$appimage_version" = 'noire 1.1.0' ]
[ -f "$integrated_launcher" ]
[ -f "$legacy_launcher" ]

# Opening either duplicate must remove the legacy launcher. In particular, the
# old launcher does not set DESKTOPINTEGRATION.
APPDIR="$app_dir" APPIMAGE="$fake_appimage" HOME="$home_dir" \
    XDG_DATA_HOME="$data_dir" XDG_RUNTIME_DIR="$runtime_dir" \
    "$app_dir/AppRun" >/dev/null
[ ! -e "$legacy_launcher" ]

# A direct AppImage launch installs the desktop metadata needed by older
# Wayland compositors to resolve the application icon. A repeated launch must
# leave the same managed files in place.
managed_data_dir="$home_dir/managed-data"
managed_runtime_dir="$work_dir/run-managed"
managed_launcher="$managed_data_dir/applications/io.github.rayan6ms.Noire.desktop"
managed_icon="$managed_data_dir/icons/hicolor/scalable/apps/io.github.rayan6ms.Noire.svg"
mkdir -p "$managed_runtime_dir"
APPDIR="$app_dir" APPIMAGE="$fake_appimage" HOME="$home_dir" \
    XDG_DATA_HOME="$managed_data_dir" XDG_RUNTIME_DIR="$managed_runtime_dir" \
    "$app_dir/AppRun" >/dev/null
[ -f "$managed_launcher" ]
[ -f "$managed_icon" ]
grep -Fqx "Exec=\"$fake_appimage\"" "$managed_launcher"
grep -Fqx 'X-Noire-AppImage-Managed=true' "$managed_launcher"
managed_launcher_checksum=$(sha256sum "$managed_launcher")
managed_icon_checksum=$(sha256sum "$managed_icon")
APPDIR="$app_dir" APPIMAGE="$fake_appimage" HOME="$home_dir" \
    XDG_DATA_HOME="$managed_data_dir" XDG_RUNTIME_DIR="$managed_runtime_dir" \
    "$app_dir/AppRun" >/dev/null
[ "$(sha256sum "$managed_launcher")" = "$managed_launcher_checksum" ]
[ "$(sha256sum "$managed_icon")" = "$managed_icon_checksum" ]

# Even a completely empty data home stays untouched for informational calls.
info_data_dir="$home_dir/info-data"
APPDIR="$app_dir" APPIMAGE="$fake_appimage" HOME="$home_dir" \
    XDG_DATA_HOME="$info_data_dir" XDG_RUNTIME_DIR="$runtime_dir" \
    "$app_dir/AppRun" --version >/dev/null
[ ! -e "$info_data_dir" ]

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

echo 'NOIRE_APPIMAGE_APPRUN metadata=pass info_read_only=pass legacy_migration=pass unrelated_launcher=preserved lifecycle=pass activation=pass'
