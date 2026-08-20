#!/bin/sh
set -eu

usage() {
    echo "usage: $0 <version> <x86_64> <binary-dir> <output-dir>" >&2
    exit 2
}

[ "$#" -eq 4 ] || usage
version=$1
architecture=$2
binary_dir=$(CDPATH='' cd -- "$3" && pwd)
output_dir=$4
repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)

command -v rpmbuild >/dev/null 2>&1 || {
    echo "rpmbuild is required to build Fedora packages" >&2
    exit 1
}
case "$version" in
    ''|*[!0-9A-Za-z._+~-]*) echo "invalid RPM version: $version" >&2; exit 2 ;;
esac
rpm_release=${NOIRE_RPM_RELEASE:-1}
case "$rpm_release" in
    ''|*[!0-9]*) echo "invalid RPM release: $rpm_release" >&2; exit 2 ;;
esac
[ "$architecture" = x86_64 ] || {
    echo "Noire packaging supports only the x86_64 RPM architecture" >&2
    exit 2
}

mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT HUP INT TERM
mkdir -p "$work_dir/BUILD" "$work_dir/BUILDROOT" "$work_dir/RPMS" \
    "$work_dir/SOURCES" "$work_dir/SPECS" "$work_dir/SRPMS" \
    "$work_dir/daemon" "$work_dir/ui"

sh "$repo_dir/packaging/validate-binaries.sh" "$version" "$architecture" "$binary_dir"

sh "$repo_dir/packaging/stage-package.sh" daemon "$work_dir/daemon" "$binary_dir"
sh "$repo_dir/packaging/stage-package.sh" ui "$work_dir/ui" "$binary_dir"

rpmbuild -bb "$repo_dir/packaging/rpm/noire.spec" \
    --define "_topdir $work_dir" \
    --define "_build_id_links none" \
    --define "noire_version $version" \
    --define "noire_release $rpm_release" \
    --define "noire_daemon_stage $work_dir/daemon" \
    --define "noire_ui_stage $work_dir/ui"

find "$work_dir/RPMS" -type f -name '*.rpm' -exec cp -f -- '{}' "$output_dir/" \;
