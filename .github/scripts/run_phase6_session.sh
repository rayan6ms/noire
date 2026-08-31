#!/usr/bin/env bash
set -euo pipefail

phase6_root="$(mktemp -d)"
daemon_pid=""

cleanup() {
  if [[ -n "$daemon_pid" ]]; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  rm -rf -- "$phase6_root"
}
trap cleanup EXIT

export XDG_CONFIG_HOME="$phase6_root/config"
export XDG_STATE_HOME="$phase6_root/state"

cargo test --package noired --features runtime \
  --test phase6_session \
  same_user_contract_rejects_stale_invalid_and_malformed_requests \
  --locked -- --ignored --nocapture
cargo build --package noired --package noirectl --locked

target/debug/noired >"$phase6_root/noired.log" 2>&1 &
daemon_pid="$!"

for _ in $(seq 1 100); do
  if timeout 2s target/debug/noirectl --json status >"$phase6_root/status.json" 2>/dev/null; then
    break
  fi
  sleep 0.02
done
test -s "$phase6_root/status.json"

python3 - "$phase6_root/status.json" <<'PY'
import json
import pathlib
import sys

snapshot = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert snapshot["schema_version"] == 1
assert snapshot["api_version"] == "1.0"
assert snapshot["state"] == "stopped"
assert snapshot["revision"] == 1
PY

timeout 2s target/debug/noirectl --json diagnostics >"$phase6_root/diagnostics.json"
python3 - "$phase6_root/diagnostics.json" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert report["schema_version"] == 1
assert "no audio" in report["privacy"]
assert report["journal_hint"].startswith("journalctl --user-unit=noire.service")
PY

timeout 2s target/debug/noirectl --json set --revision 1 strength 0.75 \
  >"$phase6_root/changed.json"
if timeout 2s target/debug/noirectl --json set --revision 1 strength 0.5 \
  >"$phase6_root/unexpected.json" 2>"$phase6_root/conflict.json"; then
  echo "stale revision unexpectedly succeeded" >&2
  exit 1
fi
python3 - "$phase6_root/conflict.json" <<'PY'
import json
import pathlib
import sys

error = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert error["schema_version"] == 1
assert error["error"]["code"] == "conflict"
PY

timeout 2s target/debug/noirectl --json status >"$phase6_root/final.json"
python3 - "$phase6_root/final.json" <<'PY'
import json
import pathlib
import sys

snapshot = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert snapshot["revision"] == 2
assert snapshot["active"] is False
assert snapshot["strength"] == 0.75
PY

echo "NOIRE_PHASE6_SESSION dbus_contract=pass cli_json=pass stale_revision=pass diagnostics_privacy=pass"
