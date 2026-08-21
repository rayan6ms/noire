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

[ "$architecture" = x86_64 ] || {
    echo "Noire AppImage packaging supports only x86_64" >&2
    exit 2
}

appimagetool=${NOIRE_APPIMAGETOOL:-}
if [ -z "$appimagetool" ]; then
    appimagetool=$(command -v appimagetool || true)
fi
[ -n "$appimagetool" ] && [ -x "$appimagetool" ] || {
    echo "set NOIRE_APPIMAGETOOL to an executable appimagetool" >&2
    exit 1
}
runtime=${NOIRE_APPIMAGE_RUNTIME:-}
if [ -n "$runtime" ] && { [ ! -f "$runtime" ] || [ -L "$runtime" ]; }; then
    echo "NOIRE_APPIMAGE_RUNTIME must be a regular, non-symbolic-link file" >&2
    exit 1
fi

mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT HUP INT TERM
app_dir="$work_dir/Noire.AppDir"

sh "$repo_dir/packaging/validate-binaries.sh" "$version" "$architecture" "$binary_dir"
sh "$repo_dir/packaging/stage-package.sh" all "$app_dir" "$binary_dir"
install -m 0755 "$repo_dir/packaging/appimage/AppRun" "$app_dir/AppRun"
cp "$repo_dir/data/applications/io.github.rayan6ms.Noire.desktop" \
    "$app_dir/io.github.rayan6ms.Noire.desktop"
cp "$repo_dir/icons/noire.svg" "$app_dir/io.github.rayan6ms.Noire.svg"
ln -s io.github.rayan6ms.Noire.svg "$app_dir/.DirIcon"
# appimagetool discovers AppStream metadata by the legacy .appdata.xml name.
# Keep the canonical metainfo file installed by stage-package.sh as well.
cp "$app_dir/usr/share/metainfo/io.github.rayan6ms.Noire.metainfo.xml" \
    "$app_dir/usr/share/metainfo/io.github.rayan6ms.Noire.appdata.xml"

# Bundle the non-core ELF dependencies found on the build system. Graphics
# drivers and glibc stay host-provided so the image uses the user's GPU stack.
mkdir -p "$app_dir/usr/lib"
library_queue="$work_dir/libraries"
: >"$library_queue"
for binary in noire noired noirectl; do
    ldd "$app_dir/usr/bin/$binary" | awk '/=> \/|^\// { for (i = 1; i <= NF; i++) if ($i ~ /^\//) print $i }' >>"$library_queue"
done

round=0
while [ "$round" -lt 4 ]; do
    round=$((round + 1))
    sort -u "$library_queue" -o "$library_queue"
    next_queue="$work_dir/libraries.$round"
    : >"$next_queue"
    while IFS= read -r library; do
        name=$(basename "$library")
        case "$name" in
            ld-linux-*|libc.so.*|libdl.so.*|libm.so.*|libpthread.so.*|libresolv.so.*|librt.so.*|libvulkan.so.*|libGLX_mesa.so.*|libEGL_mesa.so.*|libgbm.so.*|libdrm.so.*) continue ;;
        esac
        [ -e "$app_dir/usr/lib/$name" ] && continue
        cp -L "$library" "$app_dir/usr/lib/$name"
        ldd "$library" 2>/dev/null | awk '/=> \/|^\// { for (i = 1; i <= NF; i++) if ($i ~ /^\//) print $i }' >>"$next_queue" || true
    done <"$library_queue"
    mv "$next_queue" "$library_queue"
done

output="$output_dir/Noire-${version}-${architecture}.AppImage"
if [ -n "$runtime" ]; then
    ARCH="$architecture" VERSION="$version" \
        "$appimagetool" --runtime-file "$runtime" "$app_dir" "$output"
else
    ARCH="$architecture" VERSION="$version" "$appimagetool" "$app_dir" "$output"
fi
chmod 0755 "$output"
sha256sum "$output" >"$output.sha256"
printf '%s\n' "$output"
