#!/usr/bin/env python3
"""Audit paired mixtures and estimate the clean scale used in each noisy file."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path

import numpy as np
import soundfile as sf


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("clean_dir", type=Path)
    parser.add_argument("noisy_dir", type=Path)
    parser.add_argument("output_tsv", type=Path)
    parser.add_argument("--report", type=Path)
    return parser.parse_args()


def distribution(values: list[float]) -> dict[str, float | int]:
    array = np.asarray(values, dtype=np.float64)
    return {
        "count": int(array.size),
        "mean": float(np.mean(array)),
        "minimum": float(np.min(array)),
        "p01": float(np.percentile(array, 1)),
        "p05": float(np.percentile(array, 5)),
        "median": float(np.median(array)),
        "p95": float(np.percentile(array, 95)),
        "p99": float(np.percentile(array, 99)),
        "maximum": float(np.max(array)),
    }


def main() -> None:
    arguments = parse_args()
    clean_files = sorted(arguments.clean_dir.glob("*.wav"))
    if not clean_files:
        raise ValueError("no clean WAV files found")
    rows = []
    for index, clean_path in enumerate(clean_files, start=1):
        noisy_path = arguments.noisy_dir / clean_path.name
        if not noisy_path.is_file():
            raise FileNotFoundError(noisy_path)
        clean, clean_rate = sf.read(clean_path, dtype="float64")
        noisy, noisy_rate = sf.read(noisy_path, dtype="float64")
        if clean_rate != 48_000 or noisy_rate != 48_000 or clean.ndim != 1 or noisy.ndim != 1:
            raise ValueError(f"expected mono 48 kHz audio: {clean_path.name}")
        if clean.shape != noisy.shape or not np.all(np.isfinite(clean)) or not np.all(np.isfinite(noisy)):
            raise ValueError(f"invalid aligned pair: {clean_path.name}")
        clean = clean - np.mean(clean)
        noisy = noisy - np.mean(noisy)
        clean_energy = float(np.dot(clean, clean))
        if clean_energy <= 1.0e-12:
            raise ValueError(f"silent clean file: {clean_path.name}")
        clean_scale = float(np.dot(noisy, clean) / clean_energy)
        residual = noisy - clean_scale * clean
        residual_energy = float(np.dot(residual, residual))
        correlation = float(
            np.dot(clean, residual)
            / math.sqrt((clean_energy + 1.0e-20) * (residual_energy + 1.0e-20))
        )
        if not math.isfinite(clean_scale) or not 0.125 <= clean_scale <= 2.0:
            raise ValueError(f"implausible clean scale for {clean_path.name}: {clean_scale}")
        rows.append(
            {
                "case_id": clean_path.stem,
                "clean_scale": clean_scale,
                "corrected_residual_correlation": correlation,
            }
        )
        if index % 1_000 == 0:
            print(f"audited {index}/{len(clean_files)} pairs", flush=True)

    arguments.output_tsv.parent.mkdir(parents=True, exist_ok=True)
    tsv = "".join(f"{row['case_id']}\t{row['clean_scale']:.9g}\n" for row in rows)
    arguments.output_tsv.write_text(tsv, encoding="utf-8")
    digest = hashlib.sha256(tsv.encode("utf-8")).hexdigest()
    report = {
        "schema_version": 1,
        "pair_count": len(rows),
        "method": "least-squares noisy = alpha * clean + residual after DC removal",
        "scale_tsv": str(arguments.output_tsv),
        "scale_tsv_sha256": digest,
        "clean_scale": distribution([float(row["clean_scale"]) for row in rows]),
        "corrected_residual_correlation": distribution(
            [float(row["corrected_residual_correlation"]) for row in rows]
        ),
        "large_correction_counts": {
            "absolute_delta_above_0.01": sum(abs(float(row["clean_scale"]) - 1.0) > 0.01 for row in rows),
            "absolute_delta_above_0.03": sum(abs(float(row["clean_scale"]) - 1.0) > 0.03 for row in rows),
            "absolute_delta_above_0.10": sum(abs(float(row["clean_scale"]) - 1.0) > 0.10 for row in rows),
        },
    }
    report_path = arguments.report or arguments.output_tsv.with_suffix(".json")
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(report_path)


if __name__ == "__main__":
    main()
