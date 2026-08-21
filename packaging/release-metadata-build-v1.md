# Release metadata build v1

Noire release metadata is generated from a clean, committed source tree with
`packaging/generate-release-metadata.py`. The generator records the source
commit and locked dependency graph, inventories every DEB, RPM, Flatpak,
AppImage, and source archive, and emits:

- a SHA-256 manifest;
- an SPDX 2.3 SBOM;
- third-party license notices;
- an in-toto statement using the SLSA provenance v1 predicate.

The SHA-256 manifest is signed separately with the Noire release OpenPGP key.
