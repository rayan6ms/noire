#!/usr/bin/env python3
"""Verify that Noire's headless binaries have no UI/display dependencies."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections.abc import Iterable
from typing import Any

TARGET = "x86_64-unknown-linux-gnu"

FORBIDDEN_EXACT = frozenset(
    {
        "cairo-rs",
        "cairo-sys-rs",
        "gdk-pixbuf",
        "gdk-pixbuf-sys",
        "gdk4",
        "gdk4-sys",
        "gio-sys",
        "glib",
        "glib-macros",
        "glib-sys",
        "graphene-rs",
        "graphene-sys",
        "gsk4",
        "gsk4-sys",
        "gtk4",
        "gtk4-macros",
        "gtk4-sys",
        "noire-ui",
        "pango",
        "pango-sys",
        "smithay-client-toolkit",
        "winit",
        "xkbcommon",
    }
)

FORBIDDEN_PREFIXES = ("wayland-", "x11", "xkbcommon-")

REQUIRED_NATIVE = frozenset({"libspa", "pipewire", "zbus"})


def parse_arguments() -> argparse.Namespace:
    """Parse the small project-specific command line."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        action="append",
        dest="roots",
        default=[],
        help="workspace package whose dependency closure is checked",
    )
    parser.add_argument(
        "--features",
        default="noired/runtime,noired/pipewire-backend",
        help="workspace-qualified features passed to cargo metadata",
    )
    return parser.parse_args()


def load_metadata(features: str) -> dict[str, Any]:
    """Resolve the locked Linux graph with the same headless features as CI."""
    command = [
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--filter-platform",
        TARGET,
        "--locked",
    ]
    if features:
        command.extend(("--features", features))

    completed = subprocess.run(
        command,
        check=False,
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr)
        raise SystemExit(completed.returncode)

    return json.loads(completed.stdout)


def runtime_dependency_ids(node: dict[str, Any]) -> Iterable[str]:
    """Yield normal/build dependency IDs while excluding test-only edges."""
    for dependency in node["deps"]:
        if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"]):
            yield dependency["pkg"]


def dependency_closure(metadata: dict[str, Any], roots: list[str]) -> set[str]:
    """Return package IDs reachable from the named workspace roots."""
    packages = metadata["packages"]
    workspace_ids = set(metadata["workspace_members"])
    root_ids: list[str] = []

    for root in roots:
        matches = [
            package["id"]
            for package in packages
            if package["id"] in workspace_ids and package["name"] == root
        ]
        if len(matches) != 1:
            raise SystemExit(f"expected exactly one workspace package named {root!r}")
        root_ids.append(matches[0])

    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    visited: set[str] = set()
    pending = list(root_ids)

    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        node = nodes.get(package_id)
        if node is None:
            raise SystemExit(f"resolved package is missing a node: {package_id}")
        pending.extend(runtime_dependency_ids(node))

    return visited


def is_forbidden(package_name: str) -> bool:
    """Return whether a package belongs to the UI or display-server boundary."""
    return package_name in FORBIDDEN_EXACT or package_name.startswith(FORBIDDEN_PREFIXES)


def main() -> int:
    """Resolve, inspect, and report the headless dependency closure."""
    arguments = parse_arguments()
    roots = arguments.roots or ["noired", "noirectl"]
    metadata = load_metadata(arguments.features)
    package_by_id = {package["id"]: package for package in metadata["packages"]}
    closure = dependency_closure(metadata, roots)
    names = {package_by_id[package_id]["name"] for package_id in closure}
    forbidden = sorted(name for name in names if is_forbidden(name))

    if forbidden:
        joined = ", ".join(forbidden)
        print(f"headless dependency boundary violated by: {joined}", file=sys.stderr)
        return 1

    missing = sorted(REQUIRED_NATIVE - names)
    if missing:
        joined = ", ".join(missing)
        print(f"headless native features are not active: {joined}", file=sys.stderr)
        return 1

    print(
        f"headless dependency boundary passed for {', '.join(roots)} "
        f"({len(names)} packages)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
