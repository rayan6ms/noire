#!/usr/bin/env python3
"""Run Noire's bounded, non-publishing release-candidate freeze audit."""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import lzma
import re
import subprocess
import sys
import tomllib
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_VERSION = "1.0.0"
DEFAULT_PACKAGE_RELEASE = "1"
APPSTREAM = ROOT / "data/metainfo/io.github.rayan6ms.Noire.metainfo.xml"
APPSTREAM_PACKAGE_PATH = "./usr/share/metainfo/io.github.rayan6ms.Noire.metainfo.xml"
SOAK_EVIDENCE = ROOT / "tests/performance/phase7-hardening.toml"
QUALIFICATION_DECISION = ROOT / "tests/release/qualification-decision-1.0.0.toml"


@dataclass(frozen=True)
class Check:
    name: str
    passed: bool
    detail: str


def run(*arguments: str, input_bytes: bytes | None = None) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        arguments,
        cwd=ROOT,
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def text(command: subprocess.CompletedProcess[bytes]) -> str:
    return (command.stderr or command.stdout).decode(errors="replace").strip()


def digest(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def workspace_version() -> str:
    with (ROOT / "Cargo.toml").open("rb") as source:
        return tomllib.load(source)["workspace"]["package"]["version"]


def cargo_versions(expected: str) -> Check:
    command = run(
        "cargo",
        "+1.97.0",
        "metadata",
        "--locked",
        "--offline",
        "--format-version",
        "1",
        "--no-deps",
    )
    if command.returncode != 0:
        return Check("cargo-version-closure", False, text(command))
    import json

    metadata = json.loads(command.stdout)
    workspace_ids = set(metadata["workspace_members"])
    mismatches = sorted(
        f"{package['name']}={package['version']}"
        for package in metadata["packages"]
        if package["id"] in workspace_ids and package["version"] != expected
    )
    if mismatches:
        return Check("cargo-version-closure", False, ", ".join(mismatches))
    return Check("cargo-version-closure", True, f"{len(workspace_ids)} workspace packages={expected}")


def appstream_version(expected: str) -> Check:
    root = ET.parse(APPSTREAM).getroot()
    releases = root.findall("./releases/release")
    versions = [release.get("version", "") for release in releases]
    if not releases:
        return Check("appstream-version", False, "no release entries")
    if versions[0] != expected:
        return Check("appstream-version", False, f"latest={versions[0] or 'missing'}, expected={expected}")
    return Check("appstream-version", True, f"latest={expected}")


def toolchain_version() -> Check:
    with (ROOT / "rust-toolchain.toml").open("rb") as source:
        channel = tomllib.load(source)["toolchain"]["channel"]
    expected = "1.97.0"
    return Check("release-toolchain", channel == expected, f"channel={channel}, expected={expected}")


def soak_disposition_contract() -> Check:
    with SOAK_EVIDENCE.open("rb") as source:
        evidence = tomllib.load(source)
    with QUALIFICATION_DECISION.open("rb") as source:
        decision = tomllib.load(source)
    wall_clock = evidence.get("wall_clock_soak", {})
    final_test = decision.get("final_test", {})
    log_path = ROOT / final_test.get("log", "missing")
    log_digest = digest(log_path.read_bytes()) if log_path.is_file() else "missing"
    passed = (
        decision.get("release") == DEFAULT_VERSION
        and decision.get("decision") == "release-with-explicit-qualification-waivers"
        and wall_clock.get("pre_soak_hours") == 8
        and wall_clock.get("pre_soak_complete") is True
        and wall_clock.get("pre_soak_pass") is False
        and wall_clock.get("pre_soak_deadline_misses") == 2
        and wall_clock.get("release_soak_complete") is False
        and wall_clock.get("release_soak_waived") is True
        and final_test.get("result") == "failed-accepted-with-waiver"
        and final_test.get("deadline_misses") == 2
        and log_digest == final_test.get("log_sha256")
        and decision.get("owner") == "rayan6ms"
        and decision.get("expires")
    )
    return Check(
        "soak-disposition-contract",
        passed,
        "eight_hour_result=failed deadline_misses=2 final_fifteen_hour=waived "
        f"decision={decision.get('decision', 'missing')} log_sha256={log_digest}",
    )


def source_policy() -> Check:
    lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
    git_sources = re.findall(r'^source = "(git\+[^\"]+)"$', lock, flags=re.MULTILINE)
    manifests = [ROOT / "Cargo.toml", *(ROOT / "crates").glob("*/Cargo.toml")]
    git_specs = [
        path.relative_to(ROOT).as_posix()
        for path in manifests
        if re.search(r"\bgit\s*=", path.read_text(encoding="utf-8"))
    ]
    offenders = [*git_sources, *git_specs]
    if offenders:
        return Check("dependency-sources", False, ", ".join(offenders))

    with (ROOT / "vendor/checksums.toml").open("rb") as source:
        entries = tomllib.load(source)["crate"]
    errors: list[str] = []
    for entry in entries:
        directory = ROOT / entry["path"]
        files = sorted(path for path in directory.rglob("*") if path.is_file())
        tree = hashlib.sha256()
        for path in files:
            relative = path.relative_to(directory).as_posix().encode()
            file_digest = digest(path.read_bytes()).encode()
            tree.update(relative + b"\0" + file_digest + b"\0")
        if len(files) != entry["files"] or tree.hexdigest() != entry["tree_sha256"]:
            errors.append(entry["path"])
    if errors:
        return Check("dependency-sources", False, f"vendored tree mismatch: {','.join(errors)}")
    return Check(
        "dependency-sources",
        True,
        f"no Git sources; {len(entries)} vendored sys-crate trees verified",
    )


def source_state() -> Check:
    command = run("git", "status", "--porcelain=v1", "--untracked-files=all")
    if command.returncode != 0:
        return Check("source-state", False, text(command))
    entries = [line for line in command.stdout.decode().splitlines() if line]
    if entries:
        return Check("source-state", False, f"{len(entries)} changed/untracked paths")
    head = run("git", "rev-parse", "HEAD")
    if head.returncode != 0:
        return Check("source-state", False, text(head))
    return Check("source-state", True, f"clean commit={head.stdout.decode().strip()}")


def traceability() -> Check:
    command = run("python3", ".github/scripts/validate_traceability.py", "--self-test")
    if command.returncode != 0:
        return Check("traceability", False, text(command))
    detail = command.stdout.decode().strip().splitlines()[-1]
    return Check("traceability", True, detail)


def evidence_status() -> tuple[int, int, list[str], list[str]]:
    active = 0
    planned: list[str] = []
    waived: list[str] = []
    for path in sorted((ROOT / "tests/evidence").glob("*.toml")):
        with path.open("rb") as source:
            document = tomllib.load(source)
        for template in document.get("template", []):
            if template["status"] == "active":
                active += 1
            elif template["status"] == "planned":
                planned.append(template["id"])
            else:
                waived.append(template["id"])
    return active, active + len(planned) + len(waived), planned, waived


def expected_debs(version: str, release: str) -> list[str]:
    package_version = f"{version}-{release}"
    return [f"{name}_{package_version}_amd64.deb" for name in ("noire", "noire-daemon", "noire-ui")]


def expected_rpm_patterns(version: str, release: str) -> list[str]:
    return [f"{name}-{version}-{release}.*.x86_64.rpm" for name in ("noire", "noire-daemon", "noire-ui")]


def artifact_set(directory: Path, expected: list[str], patterns: bool) -> Check:
    if not directory.is_dir():
        return Check(f"{directory.name}-artifact-set", False, f"missing directory: {directory}")
    suffix = ".rpm" if patterns else ".deb"
    actual = sorted(path.name for path in directory.glob(f"*{suffix}") if path.is_file())
    if patterns:
        matched = all(sum(fnmatch.fnmatch(name, pattern) for name in actual) == 1 for pattern in expected)
        exact = matched and len(actual) == len(expected)
    else:
        exact = actual == sorted(expected)
    return Check(
        f"{directory.name}-artifact-set",
        exact,
        f"found={','.join(actual) or 'none'}; expected={','.join(expected)}",
    )


def extract_deb(path: Path, member: str) -> bytes:
    listing = run("ar", "t", str(path))
    if listing.returncode != 0:
        raise RuntimeError(text(listing))
    data_member = next(
        (name for name in listing.stdout.decode().splitlines() if name.startswith("data.tar")),
        None,
    )
    if data_member is None:
        raise RuntimeError("Debian archive has no data tar member")
    archive = run("ar", "p", str(path), data_member)
    if archive.returncode != 0:
        raise RuntimeError(text(archive))
    compression = {
        ".zst": "--zstd",
        ".xz": "--xz",
        ".gz": "--gzip",
        ".bz2": "--bzip2",
    }
    flag = next((value for suffix, value in compression.items() if data_member.endswith(suffix)), None)
    arguments = ["tar"]
    if flag is not None:
        arguments.append(flag)
    arguments.extend(("-xO", member))
    command = run(*arguments, input_bytes=archive.stdout)
    if command.returncode != 0:
        raise RuntimeError(text(command))
    return command.stdout


def extract_rpm(path: Path, member: str) -> bytes:
    archive = run("rpm2cpio", str(path))
    if archive.returncode != 0:
        raise RuntimeError(text(archive))
    command = run("cpio", "--quiet", "-i", "--to-stdout", member, input_bytes=archive.stdout)
    if command.returncode != 0:
        raise RuntimeError(text(command))
    return command.stdout


def packaged_appstream(directory: Path, package_glob: str, kind: str) -> Check:
    packages = sorted(directory.glob(package_glob)) if directory.is_dir() else []
    if len(packages) != 1:
        return Check(f"{kind}-appstream-payload", False, f"matching UI packages={len(packages)}")
    try:
        content = (
            extract_deb(packages[0], APPSTREAM_PACKAGE_PATH)
            if kind == "deb"
            else extract_rpm(packages[0], APPSTREAM_PACKAGE_PATH)
        )
    except (OSError, RuntimeError) as error:
        return Check(f"{kind}-appstream-payload", False, str(error))
    expected = APPSTREAM.read_bytes()
    return Check(
        f"{kind}-appstream-payload",
        content == expected,
        f"package_sha256={digest(content)}, source_sha256={digest(expected)}",
    )


def source_archive(path: Path, version: str) -> Check:
    expected_name = f"noire-{version}.tar.xz"
    if path.name != expected_name or not path.is_file():
        return Check("source-archive", False, f"missing exact archive: {path.parent / expected_name}")
    command = run(
        "git",
        "archive",
        "--format=tar",
        f"--prefix=noire-{version}/",
        "HEAD",
    )
    if command.returncode != 0:
        return Check("source-archive", False, text(command))
    expected = lzma.compress(
        command.stdout,
        format=lzma.FORMAT_XZ,
        check=lzma.CHECK_CRC64,
        preset=9 | lzma.PRESET_EXTREME,
    )
    actual = path.read_bytes()
    return Check(
        "source-archive",
        actual == expected,
        f"archive_sha256={digest(actual)}, clean_commit_sha256={digest(expected)}",
    )


def release_metadata(
    directory: Path,
    version: str,
    deb_dir: Path,
    rpm_dir: Path,
    source_dir: Path,
) -> Check:
    expected = {
        "SHA256SUMS",
        "THIRD_PARTY_LICENSES.md",
        f"noire-{version}.intoto.jsonl",
        f"noire-{version}.spdx.json",
    }
    actual = {path.name for path in directory.iterdir() if path.is_file()} if directory.is_dir() else set()
    if actual != expected:
        return Check(
            "release-metadata-set",
            False,
            f"found={','.join(sorted(actual)) or 'none'}; expected={','.join(sorted(expected))}",
        )
    command = run(
        "packaging/generate-release-metadata.py",
        "verify",
        "--version",
        version,
        "--artifact-dir",
        str(deb_dir),
        "--artifact-dir",
        str(rpm_dir),
        "--artifact-dir",
        str(source_dir),
        "--metadata-dir",
        str(directory),
    )
    if command.returncode != 0:
        return Check("release-metadata-set", False, text(command))
    return Check("release-metadata-set", True, command.stdout.decode().strip())


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-version", default=DEFAULT_VERSION)
    parser.add_argument("--package-release", default=DEFAULT_PACKAGE_RELEASE)
    parser.add_argument("--deb-dir", type=Path, default=ROOT / "target/phase8-packaged-ui/deb")
    parser.add_argument("--rpm-dir", type=Path, default=ROOT / "target/phase8-packaged-ui/rpm")
    parser.add_argument("--source-dir", type=Path, default=ROOT / "target/phase8-packaged-ui/source")
    parser.add_argument("--metadata-dir", type=Path, default=ROOT / "target/phase8-packaged-ui/metadata")
    parser.add_argument("--report-only", action="store_true", help="print blockers but exit successfully")
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    expected = arguments.expected_version
    release = arguments.package_release
    current = workspace_version()
    checks = [
        Check("workspace-version", current == expected, f"workspace={current}, expected={expected}"),
        cargo_versions(expected),
        appstream_version(expected),
        toolchain_version(),
        soak_disposition_contract(),
        source_policy(),
        source_state(),
        traceability(),
        artifact_set(arguments.deb_dir, expected_debs(expected, release), False),
        artifact_set(arguments.rpm_dir, expected_rpm_patterns(expected, release), True),
        packaged_appstream(arguments.deb_dir, f"noire-ui_{expected}-{release}_amd64.deb", "deb"),
        packaged_appstream(arguments.rpm_dir, f"noire-ui-{expected}-{release}.*.x86_64.rpm", "rpm"),
        source_archive(arguments.source_dir / f"noire-{expected}.tar.xz", expected),
        release_metadata(
            arguments.metadata_dir,
            expected,
            arguments.deb_dir,
            arguments.rpm_dir,
            arguments.source_dir,
        ),
    ]
    active, total, planned, waived = evidence_status()
    with QUALIFICATION_DECISION.open("rb") as source:
        qualification_decision = tomllib.load(source)
    declared_waived = qualification_decision.get("waived_template_statuses", [])
    checks.append(
        Check(
            "qualification-waiver-set",
            sorted(waived) == sorted(declared_waived),
            f"recorded={','.join(sorted(waived))}; declared={','.join(sorted(declared_waived))}",
        )
    )
    for check in checks:
        state = "PASS" if check.passed else "BLOCK"
        print(f"{state:5} {check.name}: {check.detail}")

    blockers = [check.name for check in checks if not check.passed]
    print(f"QUALIFICATION planned_evidence={','.join(planned)}")
    print(f"QUALIFICATION waived_evidence={','.join(waived)}")
    print(
        "NOIRE_RC_FREEZE_AUDIT "
        f"state={'ready' if not blockers else 'blocked'} "
        f"version={expected} package_release={release} "
        f"checks={len(checks) - len(blockers)}/{len(checks)} "
        f"active_evidence={active}/{total} freeze_blockers={len(blockers)} "
        f"qualification_planned={len(planned)} qualification_waived={len(waived)}"
    )
    if blockers and not arguments.report_only:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
