#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <noire executable or AppImage>" >&2
  exit 2
fi

target=$(realpath "$1")
test -x "$target"

smoke_root=$(mktemp -d)
cleanup() {
  rm -rf -- "$smoke_root"
}
trap cleanup EXIT

mkdir -p "$smoke_root/config" "$smoke_root/runtime" "$smoke_root/home"
chmod 0700 "$smoke_root/runtime"
export HOME="$smoke_root/home"
export XDG_CONFIG_HOME="$smoke_root/config"
export XDG_RUNTIME_DIR="$smoke_root/runtime"
export APPIMAGE_EXTRACT_AND_RUN=1
export NO_AT_BRIDGE=1

set +e
timeout --signal=TERM --kill-after=2s 7s "$target" \
  >"$smoke_root/stdout.log" 2>"$smoke_root/stderr.log"
status=$?
set -e

if [[ $status -ne 124 ]]; then
  echo "Noire did not remain alive for the startup smoke (status $status)." >&2
  sed -n '1,200p' "$smoke_root/stdout.log" >&2
  sed -n '1,200p' "$smoke_root/stderr.log" >&2
  exit 1
fi

if grep -Eiq 'panicked at|cannot (read|update).*already being updated|segmentation fault|core dumped' \
  "$smoke_root/stdout.log" "$smoke_root/stderr.log"; then
  echo "Noire emitted crash output during the startup smoke." >&2
  sed -n '1,200p' "$smoke_root/stdout.log" >&2
  sed -n '1,200p' "$smoke_root/stderr.log" >&2
  exit 1
fi

# A portable daemon must notice that the timed controller exited and release
# its private AppImage mount promptly. Native-binary smoke runs have no daemon.
for _ in $(seq 1 20); do
  if ! pgrep -x noired >/dev/null; then
    break
  fi
  sleep 0.1
done
if pgrep -x noired >/dev/null; then
  echo "The portable daemon outlived the UI startup smoke." >&2
  exit 1
fi

echo "NOIRE_UI_STARTUP_SMOKE window_liveness=pass panic_scan=pass daemon_cleanup=pass"
