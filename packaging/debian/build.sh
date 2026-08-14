#!/bin/sh
set -eu

usage() {
    echo "usage: $0 <version> <amd64> <binary-dir> <output-dir>" >&2
    exit 2
}

[ "$#" -eq 4 ] || usage
version=$1
architecture=$2
binary_dir=$(CDPATH='' cd -- "$3" && pwd)
output_dir=$4
repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)

command -v dpkg-deb >/dev/null 2>&1 || {
    echo "dpkg-deb is required to build Debian packages" >&2
    exit 1
}

is_elf() {
    magic=$(dd if="$1" bs=4 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
    [ "$magic" = 7f454c46 ]
}

case "$version" in
    ''|*[!0-9A-Za-z.+:~-]*) echo "invalid Debian version: $version" >&2; exit 2 ;;
esac
[ "$architecture" = amd64 ] || {
    echo "Noire 1.0 packaging supports only the amd64 Debian architecture" >&2
    exit 2
}
binary_version=${version#*:}
binary_version=${binary_version%%-*}

mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT HUP INT TERM

sh "$repo_dir/packaging/validate-binaries.sh" "$binary_version" x86_64 "$binary_dir"

shlib_dependencies() {
    package=$1
    root=$2
    shift 2
    has_elf=false
    for binary in "$@"; do
        if is_elf "$root/usr/bin/$binary"; then
            has_elf=true
        fi
    done
    if [ "$has_elf" = false ]; then
        return
    fi
    command -v dpkg-shlibdeps >/dev/null 2>&1 || {
        echo "dpkg-shlibdeps from dpkg-dev is required for ELF package binaries" >&2
        exit 1
    }

    analysis_dir="$work_dir/shlibs-$package"
    mkdir -p "$analysis_dir/debian"
    {
        echo "Source: noire"
        echo "Section: sound"
        echo "Priority: optional"
        echo "Maintainer: rayan6ms"
        echo "Standards-Version: 4.7.0"
        echo
        echo "Package: $package"
        echo "Architecture: any"
        echo "Description: Noire dependency analysis"
    } >"$analysis_dir/debian/control"
    cp -a "$root" "$analysis_dir/debian/$package"

    set -- "$@"
    binaries=
    for binary in "$@"; do
        binaries="$binaries debian/$package/usr/bin/$binary"
    done
    # Word splitting here is intentional: every generated entry is a controlled
    # package-relative path whose binary name contains no shell metacharacters.
    # shellcheck disable=SC2086
    (cd "$analysis_dir" && dpkg-shlibdeps -O $binaries) |
        sed -n 's/^shlibs:Depends=//p'
}

add_debian_docs() {
    root=$1
    package=$2
    install -D -m 0644 "$repo_dir/packaging/debian/copyright" \
        "$root/usr/share/doc/$package/copyright"
    {
        echo "noire ($version) unstable; urgency=medium"
        echo
        echo "  * Initial stable release candidate."
        echo
        echo " -- rayan6ms  Thu, 13 Aug 2026 00:00:00 +0000"
    } | gzip -9n >"$root/usr/share/doc/$package/changelog.Debian.gz"
    if [ -d "$root/usr/share/man" ]; then
        find "$root/usr/share/man" -type f -name '*.1' -exec gzip -9n '{}' \;
    fi
}

write_control() {
    root=$1
    package=$2
    depends=$3
    description=$4
    installed_size=$(du -sk "$root/usr" 2>/dev/null | awk '{print $1}')
    mkdir -p "$root/DEBIAN"
    {
        echo "Package: $package"
        echo "Version: $version"
        echo "Architecture: $architecture"
        echo "Maintainer: rayan6ms"
        echo "Installed-Size: ${installed_size:-0}"
        echo "Section: sound"
        echo "Priority: optional"
        [ -z "$depends" ] || echo "Depends: $depends"
        echo "Homepage: https://github.com/rayan6ms/noire"
        echo "Description: $description"
    } >"$root/DEBIAN/control"
}

daemon_root="$work_dir/noire-daemon"
ui_root="$work_dir/noire-ui"
meta_root="$work_dir/noire"
sh "$repo_dir/packaging/stage-package.sh" daemon "$daemon_root" "$binary_dir"
sh "$repo_dir/packaging/stage-package.sh" ui "$ui_root" "$binary_dir"
mkdir -p "$meta_root"

daemon_shlibs=$(shlib_dependencies noire-daemon "$daemon_root" noired noirectl)
ui_shlibs=$(shlib_dependencies noire-ui "$ui_root" noire)
if [ -z "$daemon_shlibs" ]; then
    daemon_shlibs="libpipewire-0.3-0"
fi
if [ -z "$ui_shlibs" ]; then
    ui_shlibs="libgtk-4-1 (>= 4.10)"
else
    ui_shlibs=$(printf '%s\n' "$ui_shlibs" |
        sed 's/libgtk-4-1 ([^)]*)/libgtk-4-1 (>= 4.10)/')
fi

add_debian_docs "$daemon_root" noire-daemon
add_debian_docs "$ui_root" noire-ui
add_debian_docs "$meta_root" noire

write_control "$daemon_root" noire-daemon "$daemon_shlibs" "Native per-user microphone noise-suppression daemon and CLI"
write_control "$ui_root" noire-ui "noire-daemon (= $version), $ui_shlibs" "GTK4 interface for Noire microphone noise suppression"
write_control "$meta_root" noire "noire-daemon (= $version), noire-ui (= $version)" "Complete Noire microphone noise-suppression application"

for package in noire-daemon noire-ui noire; do
    dpkg-deb --root-owner-group --build "$work_dir/$package" "$output_dir/${package}_${version}_${architecture}.deb"
done
