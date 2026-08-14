# Noire release-metadata build type v1

This document defines the `buildType` used by Noire's SLSA provenance v1
statements. It covers the final release-assembly boundary: native artifacts have
been produced by the repository's pinned binary and package builders, and
`generate-release-metadata.py` inventories and binds that staged set to the
source revision and lockfile.

## External parameters

- `version`: Noire semantic version represented by the artifacts.
- `target`: Rust target triple. Version 1 permits only
  `x86_64-unknown-linux-gnu`.
- `sourceDateEpoch`: non-negative Unix timestamp used for every generated
  metadata timestamp. It controls the SPDX creation time; the provenance omits
  optional invocation timestamps rather than presenting this reproducibility
  input as wall-clock build time.

## Internal parameters

- `sourceTreeDirty`: whether the development-only dirty-source escape hatch was
  used. Release candidates must record `false`.
- `metadata`: SHA-256 digests of the generated SPDX SBOM and third-party notice
  inventory, binding them into the provenance statement.

## Resolved dependencies and subjects

The resolved dependencies are the exact Git commit and `Cargo.lock`. The
statement subjects are every recognized `.deb`, `.rpm`, or release source archive
under the explicitly supplied artifact directories, addressed by logical relative
path and SHA-256 digest. Release candidates must be built from the same clean
commit in the pinned clean environment before this final assembly step.

## Invocation

From a clean checkout with the locked Cargo dependency graph available offline:

```sh
SOURCE_DATE_EPOCH=$(git show -s --format=%ct HEAD) \
  packaging/generate-release-metadata.py generate \
    --version VERSION \
    --artifact-dir DEB_ARTIFACT_DIRECTORY \
    --artifact-dir RPM_ARTIFACT_DIRECTORY \
    --artifact-dir SOURCE_ARTIFACT_DIRECTORY \
    --output-dir RELEASE_METADATA_DIRECTORY
```

The invocation emits `SHA256SUMS`, `noire-VERSION.spdx.json`,
`THIRD_PARTY_LICENSES.md`, and `noire-VERSION.intoto.jsonl`, then independently
verifies their artifact, dependency, embedded-model, source, and lockfile bindings.
This local provenance is unsigned and makes no SLSA level claim; candidate
signing and hosted-builder qualification belong to the frozen release workflow.
