#!/bin/sh
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
work_dir=$(mktemp -d)
trap 'find "$work_dir" -depth -delete' EXIT HUP INT TERM

bin_dir="$work_dir/bin"
runtime_dir="$work_dir/run"
mkdir -p "$bin_dir" "$runtime_dir"

{
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'exit 1'
} >"$bin_dir/noirectl"
{
    printf '%s\n' '#!/bin/sh'
    # The generated script, rather than this parent, expands these variables.
    # shellcheck disable=SC2016
    printf '%s\n' 'printf "%s\n" "${NOIRE_PORTABLE_CONTROLLER_PID:-}" >"$XDG_RUNTIME_DIR/controller-pid"'
} >"$bin_dir/noired"
{
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'exit 0'
} >"$bin_dir/noire"
chmod 0755 "$bin_dir/noirectl" "$bin_dir/noired" "$bin_dir/noire"

PATH="$bin_dir:$PATH" XDG_RUNTIME_DIR="$runtime_dir" \
    "$repo_dir/packaging/flatpak/noire-wrapper"

attempt=0
while [ ! -s "$runtime_dir/controller-pid" ] && [ "$attempt" -lt 20 ]; do
    sleep 0.05
    attempt=$((attempt + 1))
done
controller_pid=$(cat "$runtime_dir/controller-pid")
case "$controller_pid" in
    ''|*[!0-9]*) echo 'Flatpak controller PID was not propagated' >&2; exit 1 ;;
esac
[ "$controller_pid" -gt 1 ]

echo 'NOIRE_FLATPAK_WRAPPER single_instance=pass controller_lifetime=pass'
