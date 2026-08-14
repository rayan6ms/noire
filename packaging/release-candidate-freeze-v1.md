# Release-candidate freeze contract v1

This contract defines the first Noire 1.0 candidate boundary. A freeze is not a
release pass and does not permit tagging, signing, uploading, or publication.

## Freeze prerequisites

The candidate must come from one clean commit, use workspace/AppStream version
`1.0.0`, contain no Git-sourced Cargo dependencies, use the pinned Rust 1.97.0
toolchain, pass traceability and AppStream validation, and package the exact
current staged payload. Run:

```sh
python3 .github/scripts/audit_release_candidate.py \
  --expected-version 1.0.0 \
  --package-release 1 \
  --deb-dir ARTIFACT_ROOT/deb \
  --rpm-dir ARTIFACT_ROOT/rpm \
  --source-dir ARTIFACT_ROOT/source \
  --metadata-dir ARTIFACT_ROOT/release-metadata
```

`--report-only` is for development diagnosis and never constitutes a pass.

## Exact candidate set

The unsigned freeze set contains:

- source archive `noire-1.0.0.tar.xz` from the clean candidate commit;
- Debian packages `noire`, `noire-daemon`, and `noire-ui`, version `1.0.0-1`,
  architecture `amd64`;
- Fedora packages `noire`, `noire-daemon`, and `noire-ui`, version/release
  `1.0.0-1`, architecture `x86_64`;
- `SHA256SUMS`, `noire-1.0.0.spdx.json`, `THIRD_PARTY_LICENSES.md`, and
  `noire-1.0.0.intoto.jsonl` generated from that exact artifact set.

All package payloads, metadata, binaries, SBOM inputs, checksums, and provenance
must bind to the same clean commit. Development package revisions and any
artifact made before the final source change are excluded.

Create the deterministic source archive with the same format enforced by the
audit (`git archive` plus single-threaded XZ preset `-9e`, CRC64) before generating
the release metadata over all three artifact directories.

## Qualification after freeze

The frozen set remains non-releasable until the GNOME/KDE accessibility and
error-path review, clean-VM distribution/application matrix, QG-004 panel,
signed package-lifecycle repetition, and bounded real-time soak program pass.
The 8-hour pre-soak is the longest single run before the final 15-hour gate;
there is no 18-hour or 24-hour test. Every open release blocker must be closed or receive
an explicit owner-approved, time-bounded waiver before release.
