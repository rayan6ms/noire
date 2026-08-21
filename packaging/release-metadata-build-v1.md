# Release metadata build v1

Noire release metadata is generated from a clean, committed source tree with
`packaging/generate-release-metadata.py`. The generator records the source
commit and locked dependency graph, inventories every DEB, RPM, Flatpak,
AppImage, and source archive, and emits:

- a SHA-256 manifest;
- an SPDX 2.3 SBOM;
- third-party license notices;
- an in-toto statement using the SLSA provenance v1 predicate.

GitHub releases receive GitHub-hosted build-provenance attestations for every
published artifact and metadata file, including the SHA-256 manifest.

The artifact inventory must include the `deb`, `rpm`, `flatpak`, `appimage`, and
`source` directories. Portable artifacts are part of the same checksum, SPDX,
and in-toto provenance set as the native packages.
