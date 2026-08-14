#!/usr/bin/env python3
"""Generate and verify deterministic Noire release supply-chain metadata."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
REPOSITORY = "https://github.com/rayan6ms/noire"
TARGET = "x86_64-unknown-linux-gnu"
ROOT_PACKAGES = frozenset({"noired", "noirectl", "noire-ui"})
ARTIFACT_SUFFIXES = (".deb", ".rpm", ".tar.gz", ".tar.xz", ".tar.zst")
MODEL_ID = "org.rnnoise.nnnoiseless.default"
MODEL_VERSION = "nnnoiseless-0.5.2/default-e6de5fbfadf7ec91"
MODEL_LICENSE = "BSD-3-Clause"
MODEL_SHA256 = "e6de5fbfadf7ec91d1b24d6a6ccfd0290cb4d8bf555c5eab3ce41506f67a58b1"
MODEL_SOURCE = "nnnoiseless 0.5.2 src/weights.rnn"
SPDX_VERSION = "SPDX-2.3"
SLSA_PREDICATE = "https://slsa.dev/provenance/v1"
BUILD_TYPE = f"{REPOSITORY}/blob/main/packaging/release-metadata-build-v1.md"
BUILDER_ID = f"{REPOSITORY}/blob/main/packaging/generate-release-metadata.py"


class MetadataError(RuntimeError):
    """A release-metadata policy or verification failure."""


def run(*arguments: str) -> str:
    result = subprocess.run(
        arguments,
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise MetadataError(f"command failed ({' '.join(arguments)}): {detail}")
    return result.stdout.strip()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode()


def canonical_json_line(value: Any) -> bytes:
    return (json.dumps(value, separators=(",", ":"), sort_keys=True, ensure_ascii=False) + "\n").encode()


def write_atomic(path: Path, content: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temporary:
        temporary.write(content)
        temporary.flush()
        os.fsync(temporary.fileno())
        temporary_path = Path(temporary.name)
    temporary_path.chmod(0o644)
    temporary_path.replace(path)


def validate_version(version: str) -> None:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise MetadataError(f"invalid release version: {version!r}")


def source_timestamp(epoch: int) -> str:
    if epoch < 0:
        raise MetadataError("SOURCE_DATE_EPOCH must be non-negative")
    return dt.datetime.fromtimestamp(epoch, tz=dt.timezone.utc).isoformat().replace("+00:00", "Z")


def source_state(allow_dirty: bool, generated_files: list[Path]) -> tuple[str, bool]:
    head = run("git", "rev-parse", "HEAD")
    tracked_dirty = bool(run("git", "status", "--porcelain=v1", "--untracked-files=no"))
    untracked_output = subprocess.run(
        ("git", "ls-files", "--others", "--exclude-standard", "-z"),
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if untracked_output.returncode != 0:
        raise MetadataError(
            f"command failed (git ls-files): {untracked_output.stderr.decode(errors='replace').strip()}"
        )
    excluded = {path.resolve() for path in generated_files}
    untracked = [
        (ROOT / item.decode()).resolve()
        for item in untracked_output.stdout.split(b"\0")
        if item
    ]
    source_untracked = [path for path in untracked if path not in excluded]
    dirty = tracked_dirty or bool(source_untracked)
    if dirty and not allow_dirty:
        raise MetadataError(
            "source tree is dirty; release metadata requires a clean commit "
            "(set NOIRE_RELEASE_ALLOW_DIRTY_SOURCE=1 only for development validation)"
        )
    return head, dirty


def cargo_metadata() -> dict[str, Any]:
    output = run(
        "cargo",
        "metadata",
        "--locked",
        "--offline",
        "--format-version",
        "1",
        "--all-features",
        "--filter-platform",
        TARGET,
    )
    return json.loads(output)


def release_packages(metadata: dict[str, Any]) -> list[dict[str, Any]]:
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = [
        package["id"]
        for package in metadata["packages"]
        if package["name"] in ROOT_PACKAGES and package["id"] in metadata["workspace_members"]
    ]
    if len(roots) != len(ROOT_PACKAGES):
        found = sorted(packages[package_id]["name"] for package_id in roots)
        raise MetadataError(f"release Cargo roots are incomplete: found {found}")

    selected: set[str] = set()
    pending = list(roots)
    while pending:
        package_id = pending.pop()
        if package_id in selected:
            continue
        selected.add(package_id)
        node = nodes.get(package_id)
        if node is None:
            raise MetadataError(f"Cargo resolve node missing for {package_id}")
        for dependency in node["deps"]:
            kinds = dependency.get("dep_kinds", [])
            if kinds and all(item.get("kind") == "dev" for item in kinds):
                continue
            pending.append(dependency["pkg"])

    result = [packages[package_id] for package_id in selected]
    result.sort(key=lambda package: (package["name"], package["version"], package["id"]))
    for package in result:
        if not package.get("license") and not package.get("license_file"):
            raise MetadataError(f"Cargo package lacks license metadata: {package['id']}")
    return result


def verify_embedded_model(packages: list[dict[str, Any]]) -> None:
    matches = [
        package
        for package in packages
        if package["name"] == "nnnoiseless" and package["version"] == "0.5.2"
    ]
    if len(matches) != 1:
        raise MetadataError("expected exactly one nnnoiseless 0.5.2 package")
    weights = Path(matches[0]["manifest_path"]).parent / "src" / "weights.rnn"
    if not weights.is_file() or weights.is_symlink():
        raise MetadataError(f"embedded RNNoise weights are unavailable: {weights}")
    actual = sha256_file(weights)
    if actual != MODEL_SHA256:
        raise MetadataError(f"embedded RNNoise weights digest mismatch: {actual}")


def collect_artifacts(directories: list[Path]) -> dict[str, Path]:
    if not directories:
        raise MetadataError("at least one --artifact-dir is required")
    artifacts: dict[str, Path] = {}
    directory_names: set[str] = set()
    for raw_directory in directories:
        if raw_directory.is_symlink():
            raise MetadataError(f"artifact directory must not be a symbolic link: {raw_directory}")
        directory = raw_directory.resolve()
        if not directory.is_dir():
            raise MetadataError(f"artifact directory is not a real directory: {raw_directory}")
        if directory.name in directory_names:
            raise MetadataError(f"duplicate artifact directory basename: {directory.name}")
        directory_names.add(directory.name)
        for candidate in sorted(directory.rglob("*")):
            if candidate.is_symlink():
                raise MetadataError(f"artifact tree contains a symbolic link: {candidate}")
            if not candidate.is_file():
                continue
            if not candidate.name.endswith(ARTIFACT_SUFFIXES):
                continue
            relative = candidate.relative_to(directory)
            logical = (Path(directory.name) / relative).as_posix()
            if logical in artifacts:
                raise MetadataError(f"duplicate logical artifact path: {logical}")
            artifacts[logical] = candidate
    if not artifacts:
        raise MetadataError("artifact directories contain no recognized release artifacts")
    return dict(sorted(artifacts.items()))


def package_spdx_id(package: dict[str, Any]) -> str:
    identity = f"{package['name']}:{package['version']}:{package['id']}"
    suffix = hashlib.sha256(identity.encode()).hexdigest()[:12]
    safe_name = re.sub(r"[^A-Za-z0-9.-]", "-", package["name"])
    return f"SPDXRef-Package-{safe_name}-{suffix}"


def package_download_location(package: dict[str, Any]) -> str:
    source = package.get("source")
    if isinstance(source, str) and source.startswith("registry+"):
        return f"https://crates.io/crates/{package['name']}/{package['version']}/download"
    repository = package.get("repository")
    return repository if repository else "NOASSERTION"


def package_license(package: dict[str, Any]) -> str:
    declared = package.get("license")
    if declared:
        # Older Cargo manifests used a slash for dual licensing. SPDX expressions
        # require the explicit OR operator.
        return re.sub(r"\s*/\s*", " OR ", declared)
    license_file = Path(package["license_file"]).name
    suffix = hashlib.sha256(package["id"].encode()).hexdigest()[:12]
    return f"LicenseRef-{re.sub(r'[^A-Za-z0-9.-]', '-', license_file)}-{suffix}"


def build_spdx(
    version: str,
    timestamp: str,
    packages: list[dict[str, Any]],
    artifact_digests: dict[str, str],
    identity_digest: str,
) -> dict[str, Any]:
    document_id = "SPDXRef-DOCUMENT"
    product_id = "SPDXRef-Package-Noire"
    spdx_packages: list[dict[str, Any]] = [
        {
            "SPDXID": product_id,
            "name": "noire",
            "versionInfo": version,
            "downloadLocation": REPOSITORY,
            "filesAnalyzed": False,
            "licenseConcluded": "GPL-3.0-or-later",
            "licenseDeclared": "GPL-3.0-or-later",
            "copyrightText": "NOASSERTION",
            "externalRefs": [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": f"pkg:generic/noire@{version}",
                }
            ],
        }
    ]
    relationships: list[dict[str, str]] = [
        {
            "spdxElementId": document_id,
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": product_id,
        }
    ]
    for package in packages:
        spdx_id = package_spdx_id(package)
        entry: dict[str, Any] = {
            "SPDXID": spdx_id,
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": package_download_location(package),
            "filesAnalyzed": False,
            "licenseConcluded": package_license(package),
            "licenseDeclared": package_license(package),
            "copyrightText": "NOASSERTION",
        }
        if package.get("source"):
            entry["externalRefs"] = [
                {
                    "referenceCategory": "PACKAGE-MANAGER",
                    "referenceType": "purl",
                    "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
                }
            ]
        spdx_packages.append(entry)
        relationships.append(
            {
                "spdxElementId": product_id,
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": spdx_id,
            }
        )

    model_id = "SPDXRef-Package-RNNoise-Embedded-Model"
    spdx_packages.append(
        {
            "SPDXID": model_id,
            "name": MODEL_ID,
            "versionInfo": MODEL_VERSION,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "checksums": [{"algorithm": "SHA256", "checksumValue": MODEL_SHA256}],
            "licenseConcluded": MODEL_LICENSE,
            "licenseDeclared": MODEL_LICENSE,
            "copyrightText": "NOASSERTION",
            "comment": f"Embedded model source: {MODEL_SOURCE}",
        }
    )
    relationships.append(
        {
            "spdxElementId": product_id,
            "relationshipType": "CONTAINS",
            "relatedSpdxElement": model_id,
        }
    )

    files: list[dict[str, Any]] = []
    for index, (logical, digest) in enumerate(artifact_digests.items(), start=1):
        file_id = f"SPDXRef-Artifact-{index}"
        files.append(
            {
                "SPDXID": file_id,
                "fileName": f"./{logical}",
                "checksums": [{"algorithm": "SHA256", "checksumValue": digest}],
                "licenseConcluded": "NOASSERTION",
                "copyrightText": "NOASSERTION",
            }
        )
        relationships.append(
            {
                "spdxElementId": product_id,
                "relationshipType": "CONTAINS",
                "relatedSpdxElement": file_id,
            }
        )

    return {
        "spdxVersion": SPDX_VERSION,
        "dataLicense": "CC0-1.0",
        "SPDXID": document_id,
        "name": f"Noire {version} release",
        "documentNamespace": f"{REPOSITORY}/spdx/noire-{version}-{identity_digest}",
        "creationInfo": {
            "created": timestamp,
            "creators": ["Organization: Noire project", "Tool: packaging/generate-release-metadata.py"],
        },
        "documentDescribes": [product_id],
        "packages": spdx_packages,
        "files": files,
        "relationships": relationships,
    }


def build_notices(version: str, packages: list[dict[str, Any]]) -> bytes:
    external = [package for package in packages if package.get("source")]
    lines = [
        "# Third-party licenses and notices",
        "",
        f"This inventory covers the locked Rust dependency graph used by Noire {version}.",
        "License identifiers are taken from package metadata; the corresponding license",
        "texts remain in each upstream source distribution.",
        "",
        "## Embedded model",
        "",
        f"- ID: `{MODEL_ID}`",
        f"- Version: `{MODEL_VERSION}`",
        f"- License: `{MODEL_LICENSE}`",
        f"- SHA-256: `{MODEL_SHA256}`",
        f"- Source: `{MODEL_SOURCE}`",
        "- Upstream: https://github.com/jneem/nnnoiseless",
        "",
        "## Rust packages",
        "",
        "| Package | Version | License | Source |",
        "| --- | --- | --- | --- |",
    ]
    for package in external:
        source = package.get("repository") or package_download_location(package)
        lines.append(
            f"| `{package['name']}` | `{package['version']}` | "
            f"`{package_license(package)}` | {source} |"
        )
    lines.append("")
    return "\n".join(lines).encode()


def build_provenance(
    version: str,
    epoch: int,
    head: str,
    dirty: bool,
    artifact_digests: dict[str, str],
    lock_digest: str,
    metadata_digests: dict[str, str],
) -> bytes:
    statement = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [
            {"name": name, "digest": {"sha256": digest}}
            for name, digest in artifact_digests.items()
        ],
        "predicateType": SLSA_PREDICATE,
        "predicate": {
            "buildDefinition": {
                "buildType": BUILD_TYPE,
                "externalParameters": {
                    "version": version,
                    "target": TARGET,
                    "sourceDateEpoch": epoch,
                },
                "internalParameters": {
                    "sourceTreeDirty": dirty,
                    "metadata": metadata_digests,
                },
                "resolvedDependencies": [
                    {"uri": f"git+{REPOSITORY}@{head}", "digest": {"gitCommit": head}},
                    {"uri": f"{REPOSITORY}/blob/{head}/Cargo.lock", "digest": {"sha256": lock_digest}},
                ],
            },
            "runDetails": {
                "builder": {"id": BUILDER_ID},
            },
        },
    }
    return canonical_json_line(statement)


def expected_names(version: str) -> tuple[str, str, str, str]:
    return (
        "SHA256SUMS",
        f"noire-{version}.spdx.json",
        "THIRD_PARTY_LICENSES.md",
        f"noire-{version}.intoto.jsonl",
    )


def generate(arguments: argparse.Namespace) -> None:
    validate_version(arguments.version)
    epoch = arguments.source_date_epoch
    timestamp = source_timestamp(epoch)
    allow_dirty = os.environ.get("NOIRE_RELEASE_ALLOW_DIRTY_SOURCE") == "1"
    artifacts = collect_artifacts(arguments.artifact_dir)
    sums_name, spdx_name, notices_name, provenance_name = expected_names(arguments.version)
    if arguments.output_dir.is_symlink():
        raise MetadataError(f"output directory must not be a symbolic link: {arguments.output_dir}")
    output = arguments.output_dir.resolve()
    generated_files = [*artifacts.values()]
    generated_files.extend(output / name for name in expected_names(arguments.version))
    head, dirty = source_state(allow_dirty, generated_files)
    artifact_digests = {name: sha256_file(path) for name, path in artifacts.items()}
    metadata = cargo_metadata()
    packages = release_packages(metadata)
    verify_embedded_model(packages)
    lock_digest = sha256_file(ROOT / "Cargo.lock")

    identity = canonical_json(
        {
            "version": arguments.version,
            "head": head,
            "Cargo.lock": lock_digest,
            "artifacts": artifact_digests,
        }
    )
    identity_digest = hashlib.sha256(identity).hexdigest()
    spdx_bytes = canonical_json(
        build_spdx(arguments.version, timestamp, packages, artifact_digests, identity_digest)
    )
    notices_bytes = build_notices(arguments.version, packages)
    metadata_digests = {
        spdx_name: hashlib.sha256(spdx_bytes).hexdigest(),
        notices_name: hashlib.sha256(notices_bytes).hexdigest(),
    }
    provenance_bytes = build_provenance(
        arguments.version,
        epoch,
        head,
        dirty,
        artifact_digests,
        lock_digest,
        metadata_digests,
    )

    write_atomic(output / spdx_name, spdx_bytes)
    write_atomic(output / notices_name, notices_bytes)
    write_atomic(output / provenance_name, provenance_bytes)
    all_digests = dict(artifact_digests)
    all_digests[spdx_name] = metadata_digests[spdx_name]
    all_digests[notices_name] = metadata_digests[notices_name]
    all_digests[provenance_name] = hashlib.sha256(provenance_bytes).hexdigest()
    sums = "".join(f"{digest}  {name}\n" for name, digest in sorted(all_digests.items()))
    write_atomic(output / sums_name, sums.encode())
    verify_release(arguments.version, arguments.artifact_dir, output)
    print(
        "NOIRE_RELEASE_METADATA "
        f"version={arguments.version} artifacts={len(artifacts)} packages={len(packages)} "
        f"dirty={str(dirty).lower()} verify=pass"
    )


def parse_sums(path: Path) -> dict[str, str]:
    entries: dict[str, str] = {}
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        match = re.fullmatch(r"([0-9a-f]{64})  ([^\0\r\n]+)", line)
        if match is None:
            raise MetadataError(f"{path}:{line_number}: malformed checksum line")
        digest, name = match.groups()
        if name in entries:
            raise MetadataError(f"{path}:{line_number}: duplicate checksum name {name}")
        if name.startswith("/") or ".." in Path(name).parts:
            raise MetadataError(f"{path}:{line_number}: unsafe checksum path {name}")
        entries[name] = digest
    if not entries:
        raise MetadataError(f"{path}: checksum manifest is empty")
    return entries


def verify_release(version: str, directories: list[Path], metadata_dir: Path) -> None:
    validate_version(version)
    artifacts = collect_artifacts(directories)
    artifact_digests = {name: sha256_file(path) for name, path in artifacts.items()}
    packages = release_packages(cargo_metadata())
    verify_embedded_model(packages)
    sums_name, spdx_name, notices_name, provenance_name = expected_names(version)
    required = [sums_name, spdx_name, notices_name, provenance_name]
    for name in required:
        path = metadata_dir / name
        if not path.is_file() or path.is_symlink():
            raise MetadataError(f"required release metadata is missing or unsafe: {path}")

    sums = parse_sums(metadata_dir / sums_name)
    expected_sum_names = set(artifact_digests) | {spdx_name, notices_name, provenance_name}
    if set(sums) != expected_sum_names:
        missing = sorted(expected_sum_names - set(sums))
        extra = sorted(set(sums) - expected_sum_names)
        raise MetadataError(f"checksum inventory mismatch: missing={missing}, extra={extra}")
    for name, expected in sums.items():
        path = artifacts.get(name, metadata_dir / name)
        actual = sha256_file(path)
        if actual != expected:
            raise MetadataError(f"checksum mismatch for {name}: expected {expected}, got {actual}")

    spdx = json.loads((metadata_dir / spdx_name).read_text(encoding="utf-8"))
    if spdx.get("spdxVersion") != SPDX_VERSION or spdx.get("dataLicense") != "CC0-1.0":
        raise MetadataError("SBOM is not an SPDX 2.3 document")
    package_entries = spdx.get("packages", [])
    package_ids = [entry.get("SPDXID") for entry in package_entries]
    if len(package_ids) != len(set(package_ids)):
        raise MetadataError("SBOM contains duplicate package SPDX identifiers")
    indexed_packages = {entry.get("SPDXID"): entry for entry in package_entries}
    expected_package_ids = {"SPDXRef-Package-Noire", "SPDXRef-Package-RNNoise-Embedded-Model"}
    expected_package_ids.update(package_spdx_id(package) for package in packages)
    if set(indexed_packages) != expected_package_ids:
        raise MetadataError("SBOM package inventory does not match the locked release dependency graph")
    product = indexed_packages["SPDXRef-Package-Noire"]
    if product.get("name") != "noire" or product.get("versionInfo") != version:
        raise MetadataError("SBOM product identity does not match the requested release")
    for package in packages:
        entry = indexed_packages[package_spdx_id(package)]
        expected = (package["name"], package["version"], package_license(package))
        actual = (entry.get("name"), entry.get("versionInfo"), entry.get("licenseDeclared"))
        if actual != expected:
            raise MetadataError(f"SBOM package metadata mismatch for {package['id']}")
    spdx_files = {
        entry["fileName"].removeprefix("./"): entry["checksums"][0]["checksumValue"]
        for entry in spdx.get("files", [])
    }
    if spdx_files != artifact_digests:
        raise MetadataError("SBOM artifact inventory or digests do not match release artifacts")
    models = [entry for entry in package_entries if entry.get("name") == MODEL_ID]
    if len(models) != 1 or models[0].get("checksums", [{}])[0].get("checksumValue") != MODEL_SHA256:
        raise MetadataError("SBOM embedded-model identity is missing or incorrect")

    notices = (metadata_dir / notices_name).read_text(encoding="utf-8")
    for required_notice in (MODEL_ID, MODEL_VERSION, MODEL_LICENSE, MODEL_SHA256, "nnnoiseless"):
        if required_notice not in notices:
            raise MetadataError(f"third-party notices omit {required_notice}")
    if notices.encode() != build_notices(version, packages):
        raise MetadataError("third-party notices do not match the locked release dependency graph")

    provenance_lines = (metadata_dir / provenance_name).read_text(encoding="utf-8").splitlines()
    if len(provenance_lines) != 1:
        raise MetadataError("in-toto provenance must contain exactly one JSONL statement")
    provenance = json.loads(provenance_lines[0])
    if provenance.get("_type") != "https://in-toto.io/Statement/v1":
        raise MetadataError("provenance is not an in-toto Statement v1")
    if provenance.get("predicateType") != SLSA_PREDICATE:
        raise MetadataError("provenance is not SLSA provenance v1")
    subjects = {
        subject["name"]: subject["digest"]["sha256"] for subject in provenance.get("subject", [])
    }
    if subjects != artifact_digests:
        raise MetadataError("provenance subjects do not match release artifacts")
    build_definition = provenance.get("predicate", {}).get("buildDefinition", {})
    if build_definition.get("buildType") != BUILD_TYPE:
        raise MetadataError("provenance build type is incorrect")
    external_parameters = build_definition.get("externalParameters")
    if not isinstance(external_parameters, dict):
        raise MetadataError("provenance release parameters are missing")
    if external_parameters.get("version") != version or external_parameters.get("target") != TARGET:
        raise MetadataError("provenance release parameters are incorrect")
    source_epoch = external_parameters.get("sourceDateEpoch")
    if not isinstance(source_epoch, int) or source_epoch < 0:
        raise MetadataError("provenance SOURCE_DATE_EPOCH is invalid")
    internal_parameters = build_definition.get("internalParameters", {})
    expected_metadata = {
        spdx_name: sha256_file(metadata_dir / spdx_name),
        notices_name: sha256_file(metadata_dir / notices_name),
    }
    if internal_parameters.get("metadata") != expected_metadata:
        raise MetadataError("provenance does not bind the generated SBOM and notices")
    head = run("git", "rev-parse", "HEAD")
    lock_digest = sha256_file(ROOT / "Cargo.lock")
    expected_dependencies = [
        {"uri": f"git+{REPOSITORY}@{head}", "digest": {"gitCommit": head}},
        {"uri": f"{REPOSITORY}/blob/{head}/Cargo.lock", "digest": {"sha256": lock_digest}},
    ]
    if build_definition.get("resolvedDependencies") != expected_dependencies:
        raise MetadataError("provenance source commit or Cargo.lock digest is incorrect")
    builder = provenance.get("predicate", {}).get("runDetails", {}).get("builder", {})
    if builder.get("id") != BUILDER_ID:
        raise MetadataError("provenance builder identity is incorrect")


def verify(arguments: argparse.Namespace) -> None:
    if arguments.metadata_dir.is_symlink():
        raise MetadataError(f"metadata directory must not be a symbolic link: {arguments.metadata_dir}")
    verify_release(arguments.version, arguments.artifact_dir, arguments.metadata_dir.resolve())
    print(f"NOIRE_RELEASE_METADATA_VERIFY version={arguments.version} verify=pass")


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    subparsers = result.add_subparsers(dest="command", required=True)

    generate_parser = subparsers.add_parser("generate", help="generate and verify metadata")
    generate_parser.add_argument("--version", required=True)
    generate_parser.add_argument(
        "--source-date-epoch",
        type=int,
        default=int(os.environ.get("SOURCE_DATE_EPOCH", "-1")),
        help="reproducible Unix timestamp (defaults to SOURCE_DATE_EPOCH)",
    )
    generate_parser.add_argument("--artifact-dir", action="append", required=True, type=Path)
    generate_parser.add_argument("--output-dir", required=True, type=Path)
    generate_parser.set_defaults(action=generate)

    verify_parser = subparsers.add_parser("verify", help="verify existing metadata and artifacts")
    verify_parser.add_argument("--version", required=True)
    verify_parser.add_argument("--artifact-dir", action="append", required=True, type=Path)
    verify_parser.add_argument("--metadata-dir", required=True, type=Path)
    verify_parser.set_defaults(action=verify)
    return result


def main() -> int:
    try:
        arguments = parser().parse_args()
        if arguments.command == "generate" and arguments.source_date_epoch < 0:
            raise MetadataError("set SOURCE_DATE_EPOCH or pass --source-date-epoch")
        arguments.action(arguments)
        return 0
    except (
        MetadataError,
        OSError,
        json.JSONDecodeError,
        IndexError,
        KeyError,
        TypeError,
        ValueError,
    ) as error:
        print(f"release metadata error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
