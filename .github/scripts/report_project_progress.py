#!/usr/bin/env python3
"""Report distinct Noire implementation and evidence-coverage metrics."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
EXPECTED_PHASE_TASKS = {0: 6, 1: 8, 2: 8, 3: 7, 4: 7, 5: 7, 6: 8, 7: 8, 8: 8, 9: 7, 10: 7}


def evidence_counts() -> tuple[int, int]:
    active = 0
    total = 0
    for path in sorted((ROOT / "tests/evidence").glob("*.toml")):
        with path.open("rb") as source:
            document = tomllib.load(source)
        for template in document.get("template", []):
            total += 1
            active += template["status"] == "active"
    return active, total


def main() -> int:
    with (ROOT / "tests/project-progress.toml").open("rb") as source:
        document = tomllib.load(source)

    phases = document["phase"]
    if {phase["id"]: phase["tasks"] for phase in phases} != EXPECTED_PHASE_TASKS:
        raise ValueError("phase task totals drifted from the reviewed Phase 0-10 checklist")

    completed_ids: list[str] = []
    for phase in phases:
        prefix = f"P{phase['id']}-"
        if len(phase["completed"]) > phase["tasks"]:
            raise ValueError(f"phase {phase['id']} has more completed IDs than tasks")
        if any(not task.startswith(prefix) for task in phase["completed"]):
            raise ValueError(f"phase {phase['id']} contains a mismatched task ID")
        completed_ids.extend(phase["completed"])
    if len(completed_ids) != len(set(completed_ids)):
        raise ValueError("completed task IDs must be unique")

    completed = len(completed_ids)
    total = sum(EXPECTED_PHASE_TASKS.values())
    open_ids = document["qualification"]["open_tasks"]
    expected_ids = {
        f"P{phase}-{task:02d}"
        for phase, task_count in EXPECTED_PHASE_TASKS.items()
        for task in range(1, task_count + 1)
    }
    recorded_ids = set(completed_ids) | set(open_ids)
    if len(open_ids) != len(set(open_ids)):
        raise ValueError("open task IDs must be unique")
    if set(completed_ids) & set(open_ids):
        raise ValueError("a task cannot be both completed and open")
    if recorded_ids != expected_ids:
        missing = sorted(expected_ids - recorded_ids)
        unknown = sorted(recorded_ids - expected_ids)
        raise ValueError(f"task ledger mismatch: missing={missing}, unknown={unknown}")

    evidence_active, evidence_total = evidence_counts()
    if evidence_total == 0:
        raise ValueError("no evidence templates were found")
    implementation_percent = 100 * completed / total
    evidence_percent = 100 * evidence_active / evidence_total
    print(
        "NOIRE_PROJECT_PROGRESS "
        f"implementation={completed}/{total} "
        f"implementation_percent={implementation_percent:.1f} "
        f"evidence_coverage={evidence_active}/{evidence_total} "
        f"evidence_percent={evidence_percent:.1f} "
        f"open_tasks={len(open_ids)}"
    )
    print(f"OPEN_TASKS {','.join(open_ids)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
