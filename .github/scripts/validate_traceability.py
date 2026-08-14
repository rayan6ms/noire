#!/usr/bin/env python3
"""Validate Noire's requirement-to-evidence traceability contract."""

from __future__ import annotations

import argparse
import copy
import re
import sys
import tomllib
from pathlib import Path, PurePosixPath
from typing import Any

DEFAULT_ROOT = Path(__file__).resolve().parents[2]
REQUIREMENT_PATTERN = re.compile(r"^(FR|NFR|QG)-([0-9]{3})$")
TEMPLATE_PATTERN = re.compile(r"^(AT|ME|MX)-[A-Z0-9]+(?:-[A-Z0-9]+)*$")
TEST_PATTERN = re.compile(r"^T-[A-Z0-9]+(?:-[A-Z0-9]+)*$")
KIND_PREFIX = {"automated": "AT", "manual": "ME", "mixed": "MX"}

EXPECTED_PRIORITIES = {
    **{f"FR-{number:03d}": "must" for number in range(1, 15)},
    **{f"FR-{number:03d}": "later" for number in range(15, 17)},
    **{f"NFR-{number:03d}": "release" for number in range(1, 13)},
    **{f"QG-{number:03d}": "release" for number in range(1, 11)},
}
EXPECTED_ORDER = list(EXPECTED_PRIORITIES)

REQUIREMENT_KEYS = frozenset({"id", "summary", "priority", "evidence"})
TEMPLATE_KEYS = frozenset(
    {
        "id",
        "kind",
        "status",
        "phase",
        "owner",
        "requirements",
        "observable",
        "test_ids",
        "commands",
        "fixtures",
        "environments",
        "result_artifacts",
        "sources",
        "manual_reason",
        "manual_fields",
    }
)


def parse_arguments() -> argparse.Namespace:
    """Parse project-local validator options."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=DEFAULT_ROOT,
        help="repository root to validate",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="also prove representative invalid inputs are rejected",
    )
    return parser.parse_args()


def load_toml(path: Path, errors: list[str]) -> dict[str, Any]:
    """Load one TOML document while collecting a useful validation error."""
    try:
        with path.open("rb") as source:
            parsed = tomllib.load(source)
    except (OSError, tomllib.TOMLDecodeError) as error:
        errors.append(f"{path}: cannot load TOML: {error}")
        return {}

    if not isinstance(parsed, dict):
        errors.append(f"{path}: TOML root must be a table")
        return {}
    return parsed


def load_inputs(
    root: Path,
) -> tuple[dict[str, Any], list[tuple[Path, dict[str, Any]]], list[str]]:
    """Load the requirements manifest and every evidence-template document."""
    errors: list[str] = []
    manifest_path = root / "tests/requirements.toml"
    manifest = load_toml(manifest_path, errors)

    evidence_dir = root / "tests/evidence"
    template_paths = sorted(evidence_dir.glob("*.toml"))
    if not template_paths:
        errors.append(f"{evidence_dir}: no evidence template files found")
    documents = [(path, load_toml(path, errors)) for path in template_paths]
    return manifest, documents, errors


def nonempty_string(
    table: dict[str, Any], key: str, context: str, errors: list[str]
) -> str:
    """Return a required nonempty string or record a schema error."""
    value = table.get(key)
    if not isinstance(value, str) or not value.strip():
        errors.append(f"{context}: {key} must be a nonempty string")
        return ""
    return value.strip()


def string_list(
    table: dict[str, Any],
    key: str,
    context: str,
    errors: list[str],
    *,
    required: bool,
) -> list[str]:
    """Return a unique nonempty string list or record schema errors."""
    value = table.get(key)
    if value is None and not required:
        return []
    if not isinstance(value, list) or (required and not value):
        qualifier = "nonempty " if required else ""
        errors.append(f"{context}: {key} must be a {qualifier}list")
        return []

    strings: list[str] = []
    for index, item in enumerate(value):
        if not isinstance(item, str) or not item.strip():
            errors.append(f"{context}: {key}[{index}] must be a nonempty string")
            continue
        strings.append(item.strip())

    duplicates = sorted({item for item in strings if strings.count(item) > 1})
    if duplicates:
        errors.append(f"{context}: {key} contains duplicates: {', '.join(duplicates)}")
    return strings


def table_array(
    document: dict[str, Any], key: str, context: str, errors: list[str]
) -> list[dict[str, Any]]:
    """Return an array of TOML tables or record a schema error."""
    value = document.get(key)
    if not isinstance(value, list):
        errors.append(f"{context}: {key} must be an array of tables")
        return []
    tables: list[dict[str, Any]] = []
    for index, item in enumerate(value):
        if not isinstance(item, dict):
            errors.append(f"{context}: {key}[{index}] must be a table")
            continue
        tables.append(item)
    return tables


def validate_source_locator(
    root: Path, locator: str, context: str, errors: list[str]
) -> None:
    """Require an active evidence locator to name existing repository text."""
    relative_text, separator, needle = locator.partition("::")
    relative = PurePosixPath(relative_text)
    if (
        not separator
        or not needle
        or not relative_text
        or relative.is_absolute()
        or ".." in relative.parts
    ):
        errors.append(f"{context}: invalid source locator {locator!r}")
        return

    source_path = root.joinpath(*relative.parts)
    try:
        source_text = source_path.read_text(encoding="utf-8")
    except OSError as error:
        errors.append(f"{context}: source path is unavailable: {relative_text}: {error}")
        return

    if needle not in source_text:
        errors.append(
            f"{context}: source locator not found: {relative_text}::{needle}"
        )


def validate_requirements(
    manifest: dict[str, Any], errors: list[str]
) -> tuple[dict[str, set[str]], int]:
    """Validate the fixed current requirement set and return its mappings."""
    traceability = manifest.get("traceability")
    if not isinstance(traceability, dict):
        errors.append("tests/requirements.toml: traceability must be a table")
    else:
        expected_metadata = {
            "schema_version": 1,
            "plan_version": "1.2",
            "scope": "All current FR, NFR, and QG identifiers in the normative project plan",
        }
        unknown = sorted(set(traceability) - set(expected_metadata))
        if unknown:
            errors.append(f"traceability: unknown keys: {', '.join(unknown)}")
        for key, expected in expected_metadata.items():
            if traceability.get(key) != expected:
                errors.append(f"traceability: {key} must equal {expected!r}")

    requirements = table_array(
        manifest, "requirement", "tests/requirements.toml", errors
    )
    mappings: dict[str, set[str]] = {}
    ordered_ids: list[str] = []

    for index, requirement in enumerate(requirements):
        context = f"requirement[{index}]"
        unknown = sorted(set(requirement) - REQUIREMENT_KEYS)
        missing = sorted(REQUIREMENT_KEYS - set(requirement))
        if unknown:
            errors.append(f"{context}: unknown keys: {', '.join(unknown)}")
        if missing:
            errors.append(f"{context}: missing keys: {', '.join(missing)}")

        requirement_id = nonempty_string(requirement, "id", context, errors)
        nonempty_string(requirement, "summary", context, errors)
        priority = nonempty_string(requirement, "priority", context, errors)
        evidence = string_list(
            requirement, "evidence", context, errors, required=True
        )

        if requirement_id and not REQUIREMENT_PATTERN.fullmatch(requirement_id):
            errors.append(f"{context}: invalid requirement ID {requirement_id!r}")
        if requirement_id in mappings:
            errors.append(f"duplicate requirement ID: {requirement_id}")
        elif requirement_id:
            mappings[requirement_id] = set(evidence)
        ordered_ids.append(requirement_id)

        expected_priority = EXPECTED_PRIORITIES.get(requirement_id)
        if expected_priority is not None and priority != expected_priority:
            errors.append(
                f"{context}: {requirement_id} priority must be {expected_priority!r}"
            )

    missing_ids = sorted(set(EXPECTED_PRIORITIES) - set(mappings))
    unexpected_ids = sorted(set(mappings) - set(EXPECTED_PRIORITIES))
    if missing_ids:
        errors.append(f"missing requirement IDs: {', '.join(missing_ids)}")
    if unexpected_ids:
        errors.append(f"unexpected requirement IDs: {', '.join(unexpected_ids)}")
    if ordered_ids != EXPECTED_ORDER:
        errors.append("requirements must appear in canonical FR, NFR, QG numeric order")

    return mappings, len(requirements)


def validate_templates(
    root: Path,
    documents: list[tuple[Path, dict[str, Any]]],
    errors: list[str],
) -> tuple[dict[str, set[str]], int]:
    """Validate evidence templates and return their reciprocal mappings."""
    mappings: dict[str, set[str]] = {}
    seen_test_ids: dict[str, str] = {}

    for path, document in documents:
        relative_path = path.relative_to(root) if path.is_relative_to(root) else path
        if document.get("schema_version") != 1:
            errors.append(f"{relative_path}: schema_version must equal 1")
        unknown_document_keys = sorted(set(document) - {"schema_version", "template"})
        if unknown_document_keys:
            errors.append(
                f"{relative_path}: unknown keys: {', '.join(unknown_document_keys)}"
            )

        templates = table_array(document, "template", str(relative_path), errors)
        if not templates:
            errors.append(f"{relative_path}: at least one template is required")

        for index, template in enumerate(templates):
            context = f"{relative_path}:template[{index}]"
            unknown = sorted(set(template) - TEMPLATE_KEYS)
            if unknown:
                errors.append(f"{context}: unknown keys: {', '.join(unknown)}")

            template_id = nonempty_string(template, "id", context, errors)
            kind = nonempty_string(template, "kind", context, errors)
            status = nonempty_string(template, "status", context, errors)
            nonempty_string(template, "owner", context, errors)
            nonempty_string(template, "observable", context, errors)
            requirements = string_list(
                template, "requirements", context, errors, required=True
            )
            string_list(template, "fixtures", context, errors, required=True)
            string_list(template, "environments", context, errors, required=True)
            string_list(
                template, "result_artifacts", context, errors, required=True
            )

            phase = template.get("phase")
            if (
                not isinstance(phase, int)
                or isinstance(phase, bool)
                or not 0 <= phase <= 10
            ):
                errors.append(f"{context}: phase must be an integer from 0 through 10")
            if kind not in KIND_PREFIX:
                errors.append(f"{context}: kind must be automated, manual, or mixed")
            if status not in {"active", "planned", "waived"}:
                errors.append(f"{context}: status must be active, planned, or waived")
            if template_id and not TEMPLATE_PATTERN.fullmatch(template_id):
                errors.append(f"{context}: invalid evidence template ID {template_id!r}")
            expected_prefix = KIND_PREFIX.get(kind)
            if expected_prefix and not template_id.startswith(f"{expected_prefix}-"):
                errors.append(
                    f"{context}: {kind} template ID must start with {expected_prefix}-"
                )

            if template_id in mappings:
                errors.append(f"duplicate evidence template ID: {template_id}")
            elif template_id:
                mappings[template_id] = set(requirements)

            if kind in {"automated", "mixed"}:
                test_ids = string_list(
                    template, "test_ids", context, errors, required=True
                )
                string_list(template, "commands", context, errors, required=True)
                for test_id in test_ids:
                    if not TEST_PATTERN.fullmatch(test_id):
                        errors.append(f"{context}: invalid test ID {test_id!r}")
                    previous = seen_test_ids.get(test_id)
                    if previous is not None:
                        errors.append(
                            f"duplicate test ID {test_id}: {previous} and {template_id}"
                        )
                    else:
                        seen_test_ids[test_id] = template_id

            if kind in {"manual", "mixed"}:
                reason = nonempty_string(template, "manual_reason", context, errors)
                if reason and len(reason) < 40:
                    errors.append(
                        f"{context}: manual_reason must explain why automation is insufficient"
                    )
                string_list(
                    template, "manual_fields", context, errors, required=True
                )

            sources = string_list(
                template,
                "sources",
                context,
                errors,
                required=status in {"active", "waived"},
            )
            for source in sources:
                validate_source_locator(root, source, context, errors)

    return mappings, len(seen_test_ids)


def validate(
    root: Path,
    manifest: dict[str, Any],
    documents: list[tuple[Path, dict[str, Any]]],
) -> tuple[list[str], dict[str, int]]:
    """Validate schema, identifiers, and reciprocal traceability mappings."""
    errors: list[str] = []
    requirement_mappings, requirement_count = validate_requirements(manifest, errors)
    template_mappings, test_count = validate_templates(root, documents, errors)

    for requirement_id, evidence_ids in requirement_mappings.items():
        for evidence_id in sorted(evidence_ids):
            if evidence_id not in template_mappings:
                errors.append(
                    f"{requirement_id}: references unknown evidence template {evidence_id}"
                )

    for template_id, requirement_ids in template_mappings.items():
        for requirement_id in sorted(requirement_ids):
            if requirement_id not in requirement_mappings:
                errors.append(
                    f"{template_id}: references unknown requirement {requirement_id}"
                )

    all_evidence_ids = set(template_mappings)
    referenced_evidence_ids = (
        set().union(*requirement_mappings.values()) if requirement_mappings else set()
    )
    unreferenced = sorted(all_evidence_ids - referenced_evidence_ids)
    if unreferenced:
        errors.append(f"unreferenced evidence templates: {', '.join(unreferenced)}")

    for template_id in sorted(all_evidence_ids & referenced_evidence_ids):
        from_requirements = {
            requirement_id
            for requirement_id, evidence_ids in requirement_mappings.items()
            if template_id in evidence_ids
        }
        from_template = template_mappings[template_id]
        if from_requirements != from_template:
            missing = sorted(from_requirements - from_template)
            stale = sorted(from_template - from_requirements)
            details: list[str] = []
            if missing:
                details.append(f"missing from template: {', '.join(missing)}")
            if stale:
                details.append(f"stale in template: {', '.join(stale)}")
            errors.append(
                f"{template_id}: reciprocal mapping mismatch ({'; '.join(details)})"
            )

    return errors, {
        "requirements": requirement_count,
        "templates": len(template_mappings),
        "tests": test_count,
    }


def require_self_test_error(
    name: str, errors: list[str], expected_fragment: str
) -> None:
    """Fail when a deliberately invalid in-memory case is not rejected."""
    if not any(expected_fragment in error for error in errors):
        raise RuntimeError(
            f"self-test {name!r} did not report {expected_fragment!r}: {errors}"
        )


def run_self_tests(
    root: Path,
    manifest: dict[str, Any],
    documents: list[tuple[Path, dict[str, Any]]],
) -> None:
    """Prove duplicate, missing, and stale-reference checks remain effective."""
    duplicate_manifest = copy.deepcopy(manifest)
    duplicate_manifest["requirement"][0]["id"] = "FR-002"
    duplicate_errors, _ = validate(root, duplicate_manifest, documents)
    require_self_test_error(
        "duplicate requirement", duplicate_errors, "duplicate requirement ID: FR-002"
    )

    missing_manifest = copy.deepcopy(manifest)
    missing_manifest["requirement"][0]["evidence"] = ["AT-NOT-DEFINED"]
    missing_errors, _ = validate(root, missing_manifest, documents)
    require_self_test_error(
        "missing evidence",
        missing_errors,
        "references unknown evidence template AT-NOT-DEFINED",
    )

    stale_documents = copy.deepcopy(documents)
    active_template: dict[str, Any] | None = None
    for _, document in stale_documents:
        for template in document.get("template", []):
            if template.get("status") == "active":
                active_template = template
                break
        if active_template is not None:
            break
    if active_template is None:
        raise RuntimeError("self-test requires at least one active evidence template")
    active_template["sources"][0] = "README.md::traceability-self-test-missing"
    stale_errors, _ = validate(root, manifest, stale_documents)
    require_self_test_error(
        "stale source", stale_errors, "source locator not found"
    )


def main() -> int:
    """Load, validate, optionally self-test, and report compact results."""
    arguments = parse_arguments()
    root = arguments.root.resolve()
    manifest, documents, load_errors = load_inputs(root)
    errors, counts = validate(root, manifest, documents)
    errors = load_errors + errors

    if errors:
        for error in errors:
            print(f"traceability error: {error}", file=sys.stderr)
        return 1

    if arguments.self_test:
        try:
            run_self_tests(root, manifest, documents)
        except RuntimeError as error:
            print(f"traceability self-test error: {error}", file=sys.stderr)
            return 1
        print("traceability validator self-tests passed: duplicate, missing, stale")

    print(
        "traceability passed: "
        f"{counts['requirements']} requirements, "
        f"{counts['templates']} evidence templates, "
        f"{counts['tests']} automated test IDs"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
