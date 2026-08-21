#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
generator="$repo_root/packaging/generate-release-metadata.py"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/noire-release-metadata.XXXXXX")
dirty_marker="$repo_root/.noire-release-metadata-smoke-dirty"

cleanup() {
  rm -rf -- "$scratch"
  rm -f -- "$dirty_marker"
}
trap cleanup EXIT

if [[ -e "$dirty_marker" ]]; then
  echo "refusing to overwrite existing dirty-source marker: $dirty_marker" >&2
  exit 1
fi

mkdir -p "$scratch/appimage" "$scratch/deb" "$scratch/flatpak" "$scratch/rpm" \
  "$scratch/output-a" "$scratch/output-b"
printf 'synthetic AppImage release artifact\n' > "$scratch/appimage/Noire-1.1.0-x86_64.AppImage"
printf 'synthetic Debian release artifact\n' > "$scratch/deb/noire_1.1.0-1_amd64.deb"
printf 'synthetic daemon Debian release artifact\n' > "$scratch/deb/noire-daemon_1.1.0-1_amd64.deb"
printf 'synthetic Flatpak release artifact\n' > "$scratch/flatpak/Noire-1.1.0-x86_64.flatpak"
printf 'synthetic Fedora release artifact\n' > "$scratch/rpm/noire-1.1.0-1.x86_64.rpm"

printf 'dirty-source policy fixture\n' > "$dirty_marker"
if SOURCE_DATE_EPOCH=1786579200 "$generator" generate \
  --version 1.1.0 \
  --artifact-dir "$scratch/appimage" \
  --artifact-dir "$scratch/deb" \
  --artifact-dir "$scratch/flatpak" \
  --artifact-dir "$scratch/rpm" \
  --output-dir "$scratch/rejected" >"$scratch/rejected.log" 2>&1; then
  echo "release metadata generation accepted a dirty source tree" >&2
  exit 1
fi
if ! grep -Fq 'source tree is dirty' "$scratch/rejected.log"; then
  cat "$scratch/rejected.log" >&2
  echo "dirty-source rejection did not report its cause" >&2
  exit 1
fi
rm -f -- "$dirty_marker"

generate() {
  local output_dir=$1
  SOURCE_DATE_EPOCH=1786579200 NOIRE_RELEASE_ALLOW_DIRTY_SOURCE=1 \
    "$generator" generate \
      --version 1.1.0 \
      --artifact-dir "$scratch/appimage" \
      --artifact-dir "$scratch/deb" \
      --artifact-dir "$scratch/flatpak" \
      --artifact-dir "$scratch/rpm" \
      --output-dir "$output_dir"
}

generate "$scratch/output-a"
generate "$scratch/output-b"
diff --recursive --no-dereference "$scratch/output-a" "$scratch/output-b"

"$generator" verify \
  --version 1.1.0 \
  --artifact-dir "$scratch/appimage" \
  --artifact-dir "$scratch/deb" \
  --artifact-dir "$scratch/flatpak" \
  --artifact-dir "$scratch/rpm" \
  --metadata-dir "$scratch/output-a"

python3 - "$scratch/output-a" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
spdx = json.loads((root / "noire-1.1.0.spdx.json").read_text(encoding="utf-8"))
provenance = json.loads((root / "noire-1.1.0.intoto.jsonl").read_text(encoding="utf-8"))
assert spdx["spdxVersion"] == "SPDX-2.3"
assert provenance["predicateType"] == "https://slsa.dev/provenance/v1"
assert len(spdx["files"]) == 5
assert len(provenance["subject"]) == 5
assert any(package["name"] == "org.noire.fastenhancer.base-48khz" for package in spdx["packages"])
assert all("/" not in package["licenseDeclared"] for package in spdx["packages"])
PY

cp -a "$scratch/output-a" "$scratch/tampered"
printf 'tampered\n' >> "$scratch/tampered/THIRD_PARTY_LICENSES.md"
if "$generator" verify \
  --version 1.1.0 \
  --artifact-dir "$scratch/appimage" \
  --artifact-dir "$scratch/deb" \
  --artifact-dir "$scratch/flatpak" \
  --artifact-dir "$scratch/rpm" \
  --metadata-dir "$scratch/tampered" >"$scratch/tampered.log" 2>&1; then
  echo "release metadata verification accepted a tampered notice file" >&2
  exit 1
fi
if ! grep -Fq 'checksum mismatch' "$scratch/tampered.log"; then
  cat "$scratch/tampered.log" >&2
  echo "tamper rejection did not report a checksum mismatch" >&2
  exit 1
fi

printf '%s\n' \
  'NOIRE_PHASE9_RELEASE_METADATA deterministic=pass dirty_source=rejected checksums=pass sbom=pass notices=pass provenance=pass tamper=detected'
