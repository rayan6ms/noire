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
{
    echo '#!/bin/sh'
    echo "echo 'noire 1.1.0'"
} >"$app_dir/usr/bin/noire"
chmod 0755 "$app_dir/usr/bin/noire"
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

echo 'NOIRE_APPIMAGE_APPRUN legacy_migration=pass unrelated_launcher=preserved'
