#!/usr/bin/env python3
"""Compare an enhancement candidate with Noire's frozen baseline gates."""

from __future__ import annotations

import argparse
import json
import math
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


QUALITY = (
    "wer",
    "substitutions",
    "insertions",
    "deletions",
    "stoi",
    "si_sdr",
    "dnsmos_sig",
    "dnsmos_bak",
    "dnsmos_ovrl",
    "clipped_samples",
    "input_l1",
    "input_l2",
)
PERFORMANCE = (
    "allocations",
    "cpu_percent",
    "p50_us",
    "p95_us",
    "p99_us",
    "algorithmic_latency_samples",
)
REQUIRED_BUCKETS = (
    "overlapping-speakers",
    "reverberant-rooms",
    "low-snr-speech",
    "keyboard-and-transients-during-speech",
    "music-and-television",
    "whispers",
    "shouting",
    "cheap-microphones",
    "codec-compressed-audio",
    "already-clean-speech",
)


@dataclass(frozen=True)
class Check:
    name: str
    passed: bool
    detail: str


class ReportError(ValueError):
    """A malformed or incomplete quality report."""


def finite_number(value: Any, path: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ReportError(f"{path} must be numeric")
    number = float(value)
    if not math.isfinite(number):
        raise ReportError(f"{path} must be finite")
    return number


def validate(report: Any, label: str) -> dict[str, Any]:
    if not isinstance(report, dict) or report.get("schema_version") != 1:
        raise ReportError(f"{label}.schema_version must equal 1")
    if not isinstance(report.get("system_id"), str) or not report["system_id"].strip():
        raise ReportError(f"{label}.system_id must be non-empty")
    for section, keys in (("aggregate", QUALITY), ("performance", PERFORMANCE)):
        values = report.get(section)
        if not isinstance(values, dict):
            raise ReportError(f"{label}.{section} must be an object")
        for key in keys:
            finite_number(values.get(key), f"{label}.{section}.{key}")
    buckets = report.get("buckets")
    if not isinstance(buckets, dict):
        raise ReportError(f"{label}.buckets must be an object")
    for bucket in REQUIRED_BUCKETS:
        values = buckets.get(bucket)
        if not isinstance(values, dict):
            raise ReportError(f"{label}.buckets.{bucket} is required")
        for key in QUALITY:
            finite_number(values.get(key), f"{label}.buckets.{bucket}.{key}")
    return report


def metric(report: dict[str, Any], section: str, key: str) -> float:
    return float(report[section][key])


def bucket_metric(report: dict[str, Any], bucket: str, key: str) -> float:
    return float(report["buckets"][bucket][key])


def upper_gate(name: str, baseline: float, candidate: float, tolerance: float) -> Check:
    return Check(name, candidate <= baseline + tolerance, f"baseline={baseline:.6g} candidate={candidate:.6g} tolerance=+{tolerance:.6g}")


def lower_gate(name: str, baseline: float, candidate: float, tolerance: float) -> Check:
    return Check(name, candidate >= baseline - tolerance, f"baseline={baseline:.6g} candidate={candidate:.6g} tolerance=-{tolerance:.6g}")


def no_quality_regressions(baseline: dict[str, Any], candidate: dict[str, Any]) -> list[Check]:
    checks: list[Check] = []
    upper = {"wer": 0.005, "substitutions": 0.005, "insertions": 0.003, "deletions": 0.005}
    lower = {"stoi": 0.005, "si_sdr": 0.5, "dnsmos_sig": 0.05, "dnsmos_bak": 0.05, "dnsmos_ovrl": 0.05}
    for key, tolerance in upper.items():
        checks.append(upper_gate(f"aggregate-{key}", metric(baseline, "aggregate", key), metric(candidate, "aggregate", key), tolerance))
    for key, tolerance in lower.items():
        checks.append(lower_gate(f"aggregate-{key}", metric(baseline, "aggregate", key), metric(candidate, "aggregate", key), tolerance))
    for bucket in REQUIRED_BUCKETS:
        for key, tolerance in upper.items():
            checks.append(upper_gate(f"{bucket}-{key}", bucket_metric(baseline, bucket, key), bucket_metric(candidate, bucket, key), tolerance))
        for key, tolerance in lower.items():
            checks.append(lower_gate(f"{bucket}-{key}", bucket_metric(baseline, bucket, key), bucket_metric(candidate, bucket, key), tolerance))
    return checks


def hard_bucket_improvement(baseline: dict[str, Any], candidate: dict[str, Any], bucket: str) -> Check:
    # SI-SDR is deliberately not counted toward promotion. It remains a
    # regression guard, while intelligibility and naturalness drive success.
    improvements = {
        "wer": bucket_metric(baseline, bucket, "wer") - bucket_metric(candidate, bucket, "wer") >= 0.02,
        "deletions": bucket_metric(baseline, bucket, "deletions") - bucket_metric(candidate, bucket, "deletions") >= 0.01,
        "stoi": bucket_metric(candidate, bucket, "stoi") - bucket_metric(baseline, bucket, "stoi") >= 0.01,
        "dnsmos_sig": bucket_metric(candidate, bucket, "dnsmos_sig") - bucket_metric(baseline, bucket, "dnsmos_sig") >= 0.05,
        "dnsmos_ovrl": bucket_metric(candidate, bucket, "dnsmos_ovrl") - bucket_metric(baseline, bucket, "dnsmos_ovrl") >= 0.05,
    }
    passed = sum(improvements.values()) >= 2
    detail = ", ".join(f"{key}={'yes' if value else 'no'}" for key, value in improvements.items())
    return Check(f"meaningful-{bucket}-improvement", passed, detail)


def clean_gate(baseline: dict[str, Any], candidate: dict[str, Any]) -> list[Check]:
    bucket = "already-clean-speech"
    return [
        upper_gate("clean-wer", bucket_metric(baseline, bucket, "wer"), bucket_metric(candidate, bucket, "wer"), 0.002),
        upper_gate("clean-deletions", bucket_metric(baseline, bucket, "deletions"), bucket_metric(candidate, bucket, "deletions"), 0.001),
        lower_gate("clean-stoi", bucket_metric(baseline, bucket, "stoi"), bucket_metric(candidate, bucket, "stoi"), 0.002),
        lower_gate("clean-dnsmos-sig", bucket_metric(baseline, bucket, "dnsmos_sig"), bucket_metric(candidate, bucket, "dnsmos_sig"), 0.02),
        upper_gate("clean-input-l1", bucket_metric(baseline, bucket, "input_l1"), bucket_metric(candidate, bucket, "input_l1"), 0.0005),
        upper_gate("clean-input-l2", bucket_metric(baseline, bucket, "input_l2"), bucket_metric(candidate, bucket, "input_l2"), 0.0005),
    ]


def realtime_gates(baseline: dict[str, Any], candidate: dict[str, Any]) -> list[Check]:
    performance = candidate["performance"]
    return [
        Check("zero-allocations", performance["allocations"] == 0, f"candidate={performance['allocations']:.6g}"),
        Check("zero-clipping", candidate["aggregate"]["clipped_samples"] == 0, f"candidate={candidate['aggregate']['clipped_samples']:.6g}"),
        Check("cpu-below-four-percent", performance["cpu_percent"] < 4.0, f"candidate={performance['cpu_percent']:.6g}%"),
        Check("p99-below-500us", performance["p99_us"] < 500.0, f"candidate={performance['p99_us']:.6g}us"),
        Check(
            "minimal-additional-latency",
            performance["algorithmic_latency_samples"] <= baseline["performance"]["algorithmic_latency_samples"] + 480,
            f"baseline={baseline['performance']['algorithmic_latency_samples']:.6g} candidate={performance['algorithmic_latency_samples']:.6g}",
        ),
    ]


def compare(baseline: dict[str, Any], candidate: dict[str, Any]) -> list[Check]:
    checks = no_quality_regressions(baseline, candidate)
    checks.extend(clean_gate(baseline, candidate))
    checks.extend(realtime_gates(baseline, candidate))
    checks.append(hard_bucket_improvement(baseline, candidate, "overlapping-speakers"))
    checks.append(hard_bucket_improvement(baseline, candidate, "reverberant-rooms"))
    return checks


def synthetic_report(system_id: str) -> dict[str, Any]:
    quality = {
        "wer": 0.20, "substitutions": 0.10, "insertions": 0.03, "deletions": 0.07,
        "stoi": 0.80, "si_sdr": 5.0, "dnsmos_sig": 3.2, "dnsmos_bak": 3.0,
        "dnsmos_ovrl": 3.0, "clipped_samples": 0, "input_l1": 0.01, "input_l2": 0.02,
    }
    return {
        "schema_version": 1,
        "system_id": system_id,
        "aggregate": dict(quality),
        "buckets": {bucket: dict(quality) for bucket in REQUIRED_BUCKETS},
        "performance": {"allocations": 0, "cpu_percent": 3.0, "p50_us": 150.0, "p95_us": 250.0, "p99_us": 350.0, "algorithmic_latency_samples": 480},
    }


def self_test() -> int:
    baseline = synthetic_report("baseline")
    candidate = synthetic_report("candidate")
    for bucket in ("overlapping-speakers", "reverberant-rooms"):
        candidate["buckets"][bucket]["wer"] -= 0.03
        candidate["buckets"][bucket]["stoi"] += 0.02
    if not all(check.passed for check in compare(validate(baseline, "baseline"), validate(candidate, "candidate"))):
        print("self-test passing candidate was rejected", file=sys.stderr)
        return 1
    candidate["buckets"]["already-clean-speech"]["deletions"] += 0.01
    if all(check.passed for check in compare(baseline, candidate)):
        print("self-test clean-speech regression was accepted", file=sys.stderr)
        return 1
    print("enhancement comparator self-test passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("baseline", nargs="?", type=Path)
    parser.add_argument("candidate", nargs="?", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    if arguments.self_test:
        return self_test()
    if arguments.baseline is None or arguments.candidate is None:
        parser.error("baseline and candidate JSON reports are required")
    try:
        baseline = validate(json.loads(arguments.baseline.read_text(encoding="utf-8")), "baseline")
        candidate = validate(json.loads(arguments.candidate.read_text(encoding="utf-8")), "candidate")
        checks = compare(baseline, candidate)
    except (OSError, json.JSONDecodeError, ReportError) as error:
        print(f"invalid enhancement report: {error}", file=sys.stderr)
        return 2
    result = {
        "schema_version": 1,
        "baseline": baseline["system_id"],
        "candidate": candidate["system_id"],
        "passed": all(check.passed for check in checks),
        "checks": [check.__dict__ for check in checks],
    }
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if arguments.output:
        arguments.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
