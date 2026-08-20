#!/bin/sh
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_dir/Cargo.toml" | head -n 1)
binary_dir=${NOIRE_BINARY_DIR:-"$repo_dir/target/release"}
output_dir=${NOIRE_OUTPUT_DIR:-"$repo_dir/dist"}

cargo build --workspace --release --locked
mkdir -p "$output_dir"

case "${1:-all}" in
    deb) sh "$repo_dir/packaging/debian/build.sh" "$version" amd64 "$binary_dir" "$output_dir/deb" ;;
    rpm) sh "$repo_dir/packaging/rpm/build.sh" "$version" x86_64 "$binary_dir" "$output_dir/rpm" ;;
    appimage) sh "$repo_dir/packaging/appimage/build.sh" "$version" x86_64 "$binary_dir" "$output_dir/appimage" ;;
    flatpak) sh "$repo_dir/packaging/flatpak/build.sh" "$output_dir/flatpak" ;;
    all)
        sh "$repo_dir/packaging/debian/build.sh" "$version" amd64 "$binary_dir" "$output_dir/deb"
        sh "$repo_dir/packaging/rpm/build.sh" "$version" x86_64 "$binary_dir" "$output_dir/rpm"
        sh "$repo_dir/packaging/appimage/build.sh" "$version" x86_64 "$binary_dir" "$output_dir/appimage"
        sh "$repo_dir/packaging/flatpak/build.sh" "$output_dir/flatpak"
        ;;
    *) echo "usage: $0 [all|deb|rpm|appimage|flatpak]" >&2; exit 2 ;;
esac
