#!/usr/bin/env python3
"""Reject unaudited unsafe Rust and changes to Noire's audited FFI boundary."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from collections import Counter
from pathlib import Path, PurePosixPath
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
ALLOWLIST_PATH = ROOT / ".github/unsafe-allowlist.json"
SECURITY_PATH = ROOT / "SECURITY.md"
UNSAFE_PATTERN = re.compile(
    r"(?<![A-Za-z0-9_])unsafe\s*(?:\{|fn\b|impl\b|trait\b|extern\b|\()"
)
SHA256_PATTERN = re.compile(r"[0-9a-f]{64}")


def parse_arguments() -> argparse.Namespace:
    """Parse command-line options."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="exercise the scanner before checking the repository",
    )
    return parser.parse_args()


def unsafe_lines(source: str) -> list[tuple[int, str]]:
    """Return unsafe Rust constructs as one-based line numbers and exact text."""
    lines = source.splitlines()
    findings = []
    for match in UNSAFE_PATTERN.finditer(source):
        line_number = source.count("\n", 0, match.start()) + 1
        findings.append((line_number, lines[line_number - 1].strip()))
    return findings


def validate_relative_rust_path(value: str) -> None:
    """Reject allowlist paths that can escape or broaden the Rust source scope."""
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or ".." in path.parts
        or not path.parts
        or path.parts[0] != "crates"
        or path.suffix != ".rs"
    ):
        raise ValueError(f"unsafe allowlist contains an invalid Rust path: {value!r}")


def load_allowlist() -> dict[str, dict[str, Any]]:
    """Load and structurally validate the machine-readable unsafe policy."""
    try:
        document = json.loads(ALLOWLIST_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {ALLOWLIST_PATH.relative_to(ROOT)}: {error}") from error

    if (
        not isinstance(document, dict)
        or set(document) != {"version", "files"}
        or document["version"] != 1
    ):
        raise ValueError("unsafe allowlist must contain version 1 and files only")
    files = document["files"]
    if not isinstance(files, dict) or not files:
        raise ValueError("unsafe allowlist files must be a non-empty object")

    for relative, entry in files.items():
        if not isinstance(relative, str):
            raise ValueError("unsafe allowlist paths must be strings")
        validate_relative_rust_path(relative)
        if not isinstance(entry, dict) or set(entry) != {"sha256", "unsafe"}:
            raise ValueError(f"unsafe allowlist entry has invalid fields: {relative}")
        digest = entry["sha256"]
        constructs = entry["unsafe"]
        if not isinstance(digest, str) or SHA256_PATTERN.fullmatch(digest) is None:
            raise ValueError(f"unsafe allowlist entry has an invalid SHA-256: {relative}")
        if (
            not isinstance(constructs, list)
            or not constructs
            or any(not isinstance(item, str) or not item for item in constructs)
            or len(set(constructs)) != len(constructs)
        ):
            raise ValueError(f"unsafe allowlist entry has invalid constructs: {relative}")
        if any(UNSAFE_PATTERN.search(item) is None for item in constructs):
            raise ValueError(f"unsafe allowlist entry contains a non-unsafe line: {relative}")
    return files


def scan_workspace() -> dict[str, list[tuple[int, str]]]:
    """Find every syntactic unsafe marker in first-party Rust crates."""
    findings: dict[str, list[tuple[int, str]]] = {}
    for path in sorted((ROOT / "crates").rglob("*.rs")):
        relative = path.relative_to(ROOT).as_posix()
        matches = unsafe_lines(path.read_text(encoding="utf-8"))
        if matches:
            findings[relative] = matches
    return findings


def run_self_tests() -> None:
    """Prove the matcher and exact-set comparison catch policy changes."""
    sample = """\
unsafe extern "C" {
unsafe impl Send for Boundary {}
let value = unsafe { call() };
unsafe
fn split_across_lines() {}
let policy = "unsafe-failure";
"""
    assert [line for _, line in unsafe_lines(sample)] == [
        'unsafe extern "C" {',
        "unsafe impl Send for Boundary {}",
        "let value = unsafe { call() };",
        "unsafe",
    ]
    allowed = Counter({"unsafe impl Send for Boundary {}": 1})
    observed = Counter(line for _, line in unsafe_lines(sample))
    assert observed - allowed
    assert not (allowed - observed)


def verify() -> int:
    """Compare the repository with its documented exact unsafe inventory."""
    try:
        allowlist = load_allowlist()
        security = SECURITY_PATH.read_text(encoding="utf-8")
    except (OSError, ValueError) as error:
        print(f"unsafe policy error: {error}", file=sys.stderr)
        return 1

    failed = False
    observed = scan_workspace()
    for relative in sorted(set(observed) | set(allowlist)):
        findings = observed.get(relative, [])
        expected = allowlist.get(relative, {}).get("unsafe", [])
        observed_counts = Counter(line for _, line in findings)
        expected_counts = Counter(expected)

        unexpected = observed_counts - expected_counts
        for line_number, line in findings:
            if unexpected[line] > 0:
                print(f"{relative}:{line_number}: unaudited unsafe Rust: {line}", file=sys.stderr)
                unexpected[line] -= 1
                failed = True

        for line, count in (expected_counts - observed_counts).items():
            print(
                f"{relative}: allowlisted unsafe construct missing ({count}): {line}",
                file=sys.stderr,
            )
            failed = True

    for relative, entry in allowlist.items():
        path = ROOT / relative
        try:
            actual_digest = hashlib.sha256(path.read_bytes()).hexdigest()
        except OSError as error:
            print(f"{relative}: cannot hash audited boundary: {error}", file=sys.stderr)
            failed = True
            continue
        if actual_digest != entry["sha256"]:
            print(
                f"{relative}: audited boundary SHA-256 changed; review SECURITY.md "
                "and update the allowlist deliberately",
                file=sys.stderr,
            )
            failed = True
        if relative not in security:
            print(f"{relative}: audited boundary is absent from SECURITY.md", file=sys.stderr)
            failed = True

    if failed:
        return 1

    count = sum(len(findings) for findings in observed.values())
    print(f"unsafe policy passed: {count} audited constructs in {len(observed)} file(s)")
    return 0


def main() -> int:
    """Optionally self-test, then enforce the repository policy."""
    arguments = parse_arguments()
    if arguments.self_test:
        run_self_tests()
        print("unsafe policy self-test passed")
    return verify()


if __name__ == "__main__":
    raise SystemExit(main())
