#!/usr/bin/env bash
set -euo pipefail

unit="data/systemd/user/noire.service"
if [[ ! -f "$unit" ]]; then
    echo "Missing packaged user unit: $unit" >&2
    exit 1
fi
if ! command -v systemd-analyze >/dev/null; then
    echo "Phase-9 service verification requires systemd-analyze" >&2
    exit 2
fi

phase9_root="$(mktemp -d)"
cleanup() {
    rm -rf -- "$phase9_root"
}
trap cleanup EXIT

install -d "$phase9_root/etc/systemd/system" "$phase9_root/etc" "$phase9_root/usr/bin"
install -m 0644 /etc/os-release "$phase9_root/etc/os-release"
install -m 0644 "$unit" "$phase9_root/etc/systemd/system/noire.service"
install -m 0755 /bin/true "$phase9_root/usr/bin/noired"
systemd-analyze --root="$phase9_root" --recursive-errors=no verify noire.service

python3 - "$unit" <<'PY'
from pathlib import Path
import configparser
import sys

unit = Path(sys.argv[1])
parser = configparser.ConfigParser(interpolation=None, strict=True)
parser.optionxform = str
with unit.open(encoding="utf-8") as source:
    parser.read_file(source)

expected = {
    ("Unit", "StartLimitIntervalSec"): "30s",
    ("Unit", "StartLimitBurst"): "3",
    ("Service", "Type"): "dbus",
    ("Service", "BusName"): "io.github.rayan6ms.Noire.Noire1",
    ("Service", "ExecStart"): "/usr/bin/noired",
    ("Service", "Restart"): "on-failure",
    ("Service", "RestartSec"): "2s",
    ("Service", "TimeoutStopSec"): "5s",
    ("Service", "UMask"): "0077",
    ("Service", "ConfigurationDirectory"): "noire",
    ("Service", "ConfigurationDirectoryMode"): "0700",
    ("Service", "NoNewPrivileges"): "true",
    ("Service", "ProtectSystem"): "strict",
    ("Service", "ProtectHome"): "read-only",
    ("Service", "RestrictAddressFamilies"): "AF_UNIX",
    ("Install", "WantedBy"): "default.target",
}
for (section, key), value in expected.items():
    actual = parser.get(section, key, fallback=None)
    if actual != value:
        raise SystemExit(f"{section}.{key}: expected {value!r}, got {actual!r}")
if parser.has_option("Install", "Alias"):
    raise SystemExit("The package must not create an implicit enablement alias")
PY

if [[ "${NOIRE_PHASE9_SYSTEMD_LIFECYCLE:-0}" != "1" ]]; then
    echo "NOIRE_PHASE9_UNIT static_verify=pass lifecycle=not-requested"
    exit 0
fi

if [[ "${NOIRE_PHASE9_DISPOSABLE_VM:-0}" != "1" ]]; then
    echo "Refusing to change a user manager without NOIRE_PHASE9_DISPOSABLE_VM=1" >&2
    exit 2
fi
if [[ "$(systemctl --user is-system-running 2>/dev/null || true)" == "offline" ]]; then
    echo "A running disposable systemd user manager is required" >&2
    exit 2
fi

systemctl --user disable --now noire.service >/dev/null 2>&1 || true
systemctl --user link "$(realpath "$unit")" >/dev/null
systemctl --user daemon-reload
if systemctl --user is-enabled --quiet noire.service; then
    echo "Fresh installation unexpectedly enabled noire.service" >&2
    exit 1
fi

systemctl --user enable --now noire.service >/dev/null
systemctl --user is-enabled --quiet noire.service
systemctl --user is-active --quiet noire.service
main_pid="$(systemctl --user show noire.service --property=MainPID --value)"
process_uid="$(ps -o uid= -p "$main_pid" | tr -d ' ')"
if [[ "$process_uid" != "$(id -u)" ]]; then
    echo "User service ran as UID $process_uid instead of $(id -u)" >&2
    exit 1
fi

kill -KILL "$main_pid"
for _attempt in $(seq 1 50); do
    restarts="$(systemctl --user show noire.service --property=NRestarts --value)"
    if systemctl --user is-active --quiet noire.service && [[ "$restarts" -ge 1 ]]; then
        break
    fi
    sleep 0.1
done
systemctl --user is-active --quiet noire.service
restarts="$(systemctl --user show noire.service --property=NRestarts --value)"
if [[ "$restarts" -lt 1 ]]; then
    echo "noire.service did not restart after failure" >&2
    exit 1
fi

systemctl --user stop noire.service
systemctl --user is-active --quiet noire.service && exit 1
systemctl --user disable noire.service >/dev/null
systemctl --user is-enabled --quiet noire.service && exit 1
systemctl --user daemon-reload

echo "NOIRE_PHASE9_UNIT static_verify=pass enable=pass start=pass uid=$process_uid restart=$restarts stop=pass disable=pass"
