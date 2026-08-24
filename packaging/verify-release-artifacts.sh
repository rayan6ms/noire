#!/bin/sh
set -eu

usage() {
    echo "usage: $0 <version> <package-release> <deb-dir> <rpm-dir> <flatpak-dir> <appimage-dir> <source-dir>" >&2
    exit 2
}

[ "$#" -eq 7 ] || usage
version=$1
package_release=$2
deb_dir=$3
rpm_dir=$4
flatpak_dir=$5
appimage_dir=$6
source_dir=$7
repo_dir=${NOIRE_REPO_DIR:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}

require_file() {
    [ -f "$1" ] && [ ! -L "$1" ] && [ -s "$1" ] || {
        echo "missing, empty, or symbolic-link release artifact: $1" >&2
        exit 1
    }
}

assert_count() {
    expected=$1
    shift
    [ "$#" -eq "$expected" ] || {
        echo "expected $expected artifact(s), found $#: $*" >&2
        exit 1
    }
}

set -- "$deb_dir"/*.deb
assert_count 3 "$@"
for package in noire noire-daemon noire-ui; do
    artifact="$deb_dir/${package}_${version}-${package_release}_amd64.deb"
    require_file "$artifact"
    [ "$(dpkg-deb --field "$artifact" Package)" = "$package" ]
    [ "$(dpkg-deb --field "$artifact" Version)" = "$version-$package_release" ]
    [ "$(dpkg-deb --field "$artifact" Architecture)" = amd64 ]
done

set -- "$rpm_dir"/*.rpm
assert_count 3 "$@"
for package in noire noire-daemon noire-ui; do
    set -- "$rpm_dir/${package}-${version}-${package_release}."*.x86_64.rpm
    assert_count 1 "$@"
    require_file "$1"
    rpm_identity=$(rpm --query --package --queryformat '%{NAME} %{VERSION} %{RELEASE} %{ARCH}\n' "$1")
    case "$rpm_identity" in
        "$package $version $package_release."*" x86_64") ;;
        *) echo "unexpected RPM identity: $rpm_identity" >&2; exit 1 ;;
    esac
done

flatpak="$flatpak_dir/Noire-${version}-x86_64.flatpak"
appimage="$appimage_dir/Noire-${version}-x86_64.AppImage"
source="$source_dir/noire-${version}.tar.xz"
require_file "$flatpak"
require_file "$appimage"
require_file "$source"
assert_count 1 "$flatpak_dir"/*.flatpak
assert_count 1 "$appimage_dir"/*.AppImage
assert_count 1 "$source_dir"/*.tar.xz

# GitHub's artifact service does not preserve executable mode bits.
chmod 0755 "$appimage"
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT HUP INT TERM
appimage_home="$work_dir/appimage-home"
mkdir -p "$appimage_home"
[ "$(HOME="$appimage_home" XDG_DATA_HOME="$appimage_home/data" \
    APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --version)" = "noire $version" ]
[ -z "$(find "$appimage_home" -mindepth 1 -print -quit)" ] || {
    echo "AppImage informational command wrote to the user's home directory" >&2
    exit 1
}

appimage_absolute=$(CDPATH='' cd -- "$(dirname -- "$appimage")" && pwd)/$(basename "$appimage")
appimage_extract="$work_dir/appimage-extract"
mkdir -p "$appimage_extract"
(cd "$appimage_extract" && "$appimage_absolute" --appimage-extract >/dev/null)
for bundled_runtime_file in \
    usr/lib/pipewire-0.3/libpipewire-module-protocol-native.so \
    usr/lib/pipewire-0.3/libpipewire-module-client-node.so \
    usr/lib/spa-0.2/support/libspa-support.so \
    usr/lib/spa-0.2/audioconvert/libspa-audioconvert.so \
    usr/share/pipewire/client.conf
do
    require_file "$appimage_extract/squashfs-root/$bundled_runtime_file"
done

export FLATPAK_USER_DIR="$work_dir/flatpak-user"
flatpak install --user --noninteractive --no-deps --no-related --bundle "$flatpak"
flatpak_ref=$(flatpak info --user --show-ref io.github.rayan6ms.Noire)
case "$flatpak_ref" in
    app/io.github.rayan6ms.Noire/x86_64/*) ;;
    *) echo "unexpected Flatpak ref: $flatpak_ref" >&2; exit 1 ;;
esac

git -C "$repo_dir" archive --format=tar --prefix="noire-${version}/" HEAD |
    xz --threads=1 -9e >"$work_dir/noire-${version}.tar.xz"
cmp "$work_dir/noire-${version}.tar.xz" "$source"

printf 'NOIRE_RELEASE_ARTIFACTS version=%s deb=3 rpm=3 flatpak=1 appimage=1 source=1 verify=pass\n' "$version"
