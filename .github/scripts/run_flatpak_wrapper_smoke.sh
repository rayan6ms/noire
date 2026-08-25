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
# The generated script, rather than this parent, expands these variables.
# shellcheck disable=SC2016
{
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'if [ "${NOIRE_SMOKE_HOLD:-}" = 1 ]; then'
    printf '%s\n' '    printf ready >"$XDG_RUNTIME_DIR/controller-ready"'
    printf '%s\n' '    activation_file="$XDG_RUNTIME_DIR/noire-controller.activate"'
    printf '%s\n' '    attempt=0'
    printf '%s\n' '    while [ ! -s "$activation_file" ] && [ "$attempt" -lt 40 ]; do'
    printf '%s\n' '        sleep 0.05'
    printf '%s\n' '        attempt=$((attempt + 1))'
    printf '%s\n' '    done'
    printf '%s\n' '    cat "$activation_file" >"$XDG_RUNTIME_DIR/activation-request"'
    printf '%s\n' '    exit 0'
    printf '%s\n' 'fi'
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

runtime_activation="$work_dir/run-activation"
mkdir -p "$runtime_activation"
PATH="$bin_dir:$PATH" NOIRE_SMOKE_HOLD=1 XDG_RUNTIME_DIR="$runtime_activation" \
    "$repo_dir/packaging/flatpak/noire-wrapper" &
controller=$!
attempt=0
while [ ! -s "$runtime_activation/controller-ready" ] && [ "$attempt" -lt 40 ]; do
    sleep 0.05
    attempt=$((attempt + 1))
done
[ -s "$runtime_activation/controller-ready" ]
PATH="$bin_dir:$PATH" NOIRE_SMOKE_HOLD=1 XDG_RUNTIME_DIR="$runtime_activation" \
    "$repo_dir/packaging/flatpak/noire-wrapper"
wait "$controller"
[ "$(cat "$runtime_activation/activation-request")" = show ]

echo 'NOIRE_FLATPAK_WRAPPER single_instance=pass controller_lifetime=pass activation=pass'
