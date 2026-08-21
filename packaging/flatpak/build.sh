#!/bin/sh
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
output_dir=${1:-"$repo_dir/dist/flatpak"}
command -v flatpak-builder >/dev/null 2>&1 || {
    echo "flatpak-builder is required" >&2
    exit 1
}

mkdir -p "$output_dir"
build_dir=$(mktemp -d)
trap 'rm -rf -- "$build_dir"' EXIT HUP INT TERM
flatpak-builder --force-clean --repo="$output_dir/repository" \
    "$build_dir" "$repo_dir/packaging/flatpak/io.github.rayan6ms.Noire.yml"
flatpak build-bundle "$output_dir/repository" \
    "$output_dir/Noire-1.1.0-x86_64.flatpak" io.github.rayan6ms.Noire
