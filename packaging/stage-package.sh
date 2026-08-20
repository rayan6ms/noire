#!/bin/sh
set -eu

usage() {
    echo "usage: $0 <daemon|ui|all> <destdir> <binary-dir>" >&2
    exit 2
}

[ "$#" -eq 3 ] || usage
component=$1
destdir=$2
binary_dir=$3

case "$component" in
    daemon|ui|all) ;;
    *) usage ;;
esac

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

install_file() {
    mode=$1
    source=$2
    target=$3
    install -D -m "$mode" "$source" "$destdir$target"
}

stage_daemon() {
    install_file 0755 "$binary_dir/noired" /usr/bin/noired
    install_file 0755 "$binary_dir/noirectl" /usr/bin/noirectl
    install_file 0644 "$repo_dir/data/systemd/user/noire.service" /usr/lib/systemd/user/noire.service
    install_file 0644 "$repo_dir/data/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service" /usr/share/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service
    install_file 0644 "$repo_dir/data/dbus-1/interfaces/io.github.rayan6ms.Noire.Noire1.xml" /usr/share/dbus-1/interfaces/io.github.rayan6ms.Noire.Noire1.xml
    install_file 0644 "$repo_dir/data/config/config-v1.toml" /usr/share/doc/noire-daemon/config-v1.toml
    install_file 0644 "$repo_dir/LICENSE" /usr/share/licenses/noire-daemon/LICENSE
    install_file 0644 "$repo_dir/data/man/noired.1" /usr/share/man/man1/noired.1
    install_file 0644 "$repo_dir/data/man/noirectl.1" /usr/share/man/man1/noirectl.1
    install_file 0644 "$repo_dir/data/completions/noirectl.bash" /usr/share/bash-completion/completions/noirectl
    install_file 0644 "$repo_dir/data/completions/_noirectl" /usr/share/zsh/site-functions/_noirectl
    install_file 0644 "$repo_dir/data/completions/noirectl.fish" /usr/share/fish/vendor_completions.d/noirectl.fish
}

stage_ui() {
    install_file 0755 "$binary_dir/noire" /usr/bin/noire
    install_file 0644 "$repo_dir/data/applications/io.github.rayan6ms.Noire.desktop" /usr/share/applications/io.github.rayan6ms.Noire.desktop
    install_file 0644 "$repo_dir/data/metainfo/io.github.rayan6ms.Noire.metainfo.xml" /usr/share/metainfo/io.github.rayan6ms.Noire.metainfo.xml
    install_file 0644 "$repo_dir/icons/noire.svg" /usr/share/icons/hicolor/scalable/apps/io.github.rayan6ms.Noire.svg
    install_file 0644 "$repo_dir/LICENSE" /usr/share/licenses/noire-ui/LICENSE
    install_file 0644 "$repo_dir/icons/LICENSE" /usr/share/licenses/noire-ui/icon-LICENSE
    install_file 0644 "$repo_dir/data/man/noire.1" /usr/share/man/man1/noire.1
}

case "$component" in
    daemon) stage_daemon ;;
    ui) stage_ui ;;
    all)
        stage_daemon
        stage_ui
        ;;
esac
