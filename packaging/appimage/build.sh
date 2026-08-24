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

# libpipewire discovers its client modules, configuration, and SPA plugins at
# runtime, so ldd cannot find them. Keep the matching build-host components in
# the image; otherwise the Ubuntu-built library looks in Ubuntu-only paths and
# fails on distributions such as Fedora before it can create a PipeWire loop.
pipewire_module_dir=$(pkg-config --variable=moduledir libpipewire-0.3)
spa_plugin_dir=$(pkg-config --variable=plugindir libspa-0.2)
pipewire_prefix=$(pkg-config --variable=prefix libpipewire-0.3)
pipewire_config_dir="$pipewire_prefix/share/pipewire"
[ -d "$pipewire_module_dir" ] || {
    echo "PipeWire module directory is unavailable: $pipewire_module_dir" >&2
    exit 1
}
[ -d "$spa_plugin_dir" ] || {
    echo "SPA plugin directory is unavailable: $spa_plugin_dir" >&2
    exit 1
}
[ -f "$pipewire_config_dir/client.conf" ] || {
    echo "PipeWire client configuration is unavailable" >&2
    exit 1
}
mkdir -p "$app_dir/usr/lib/pipewire-0.3" "$app_dir/usr/lib/spa-0.2" \
    "$app_dir/usr/share/pipewire"
for module in \
    libpipewire-module-rt.so \
    libpipewire-module-protocol-native.so \
    libpipewire-module-client-node.so \
    libpipewire-module-client-device.so \
    libpipewire-module-adapter.so \
    libpipewire-module-metadata.so \
    libpipewire-module-session-manager.so
do
    [ -f "$pipewire_module_dir/$module" ] || {
        echo "required PipeWire client module is unavailable: $module" >&2
        exit 1
    }
    cp -L "$pipewire_module_dir/$module" "$app_dir/usr/lib/pipewire-0.3/$module"
done
for plugin in \
    support/libspa-support.so \
    audioconvert/libspa-audioconvert.so
do
    [ -f "$spa_plugin_dir/$plugin" ] || {
        echo "required SPA plugin is unavailable: $plugin" >&2
        exit 1
    }
    mkdir -p "$app_dir/usr/lib/spa-0.2/$(dirname "$plugin")"
    cp -L "$spa_plugin_dir/$plugin" "$app_dir/usr/lib/spa-0.2/$plugin"
done
cp "$pipewire_config_dir/client.conf" "$app_dir/usr/share/pipewire/client.conf"

# Bundle the non-core ELF dependencies found on the build system. Graphics
# drivers and glibc stay host-provided so the image uses the user's GPU stack.
mkdir -p "$app_dir/usr/lib"
library_queue="$work_dir/libraries"
: >"$library_queue"
for binary in noire noired noirectl; do
    ldd "$app_dir/usr/bin/$binary" | awk '/=> \/|^\// { for (i = 1; i <= NF; i++) if ($i ~ /^\//) print $i }' >>"$library_queue"
done
find "$app_dir/usr/lib/pipewire-0.3" "$app_dir/usr/lib/spa-0.2" \
    -type f -name '*.so*' -exec ldd {} \; 2>/dev/null |
    awk '/=> \/|^\// { for (i = 1; i <= NF; i++) if ($i ~ /^\//) print $i }' \
        >>"$library_queue"

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
