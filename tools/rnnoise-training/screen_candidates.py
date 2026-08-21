#!/usr/bin/env python3
"""Screen RNNoise checkpoints on speakers excluded from candidate training."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import math
import subprocess
import zipfile
from collections import defaultdict
from pathlib import Path

import numpy as np
import soundfile as sf
from pystoi import stoi


SAMPLE_RATE = 48_000
MODEL_ARGUMENTS = (
    "--model-high-pass",
    "60",
    "--speech-strength",
    "0.55",
    "--noise-strength",
    "0.70",
    "--vad-low",
    "0.20",
    "--vad-high",
    "0.80",
)


def parse_args() -> argparse.Namespace:
    repository = Path(__file__).resolve().parents[2]
    cache = repository / "docs" / ".cache"
    parser = argparse.ArgumentParser()
    parser.add_argument("models", nargs="+", type=Path)
    parser.add_argument(
        "--default-model",
        type=Path,
        default=cache / "rnnoise-training" / "default.rnn",
    )
    parser.add_argument(
        "--clean-dir",
        type=Path,
        default=cache / "rnnoise-training" / "data" / "clean_trainset_28spk_wav",
    )
    parser.add_argument(
        "--noisy-dir",
        type=Path,
        default=cache / "rnnoise-training" / "data" / "noisy_trainset_28spk_wav",
    )
    parser.add_argument(
        "--condition-log",
        type=Path,
        default=cache / "corpora" / "voicebank-demand" / "logfiles.zip",
    )
    parser.add_argument("--condition-member", default="log_trainset_28spk.txt")
    parser.add_argument(
        "--case-manifest",
        type=Path,
        help="JSON manifest with a cases array; bypasses VoiceBank condition selection",
    )
    parser.add_argument(
        "--quality-lab",
        type=Path,
        default=repository / "target" / "release" / "noire-quality-lab",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=cache / "rnnoise-training" / "screen",
    )
    parser.add_argument("--speakers", default="p226,p287")
    parser.add_argument("--limit", type=int, default=96)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument(
        "--candidate-strength",
        type=float,
        default=1.0,
        help="fixed dry/wet mix applied to non-default candidate outputs",
    )
    parser.add_argument("--force", action="store_true")
    parser.add_argument(
        "--skip-clean-render",
        action="store_true",
        help="score reference/input pairs without separately processing each reference",
    )
    parser.add_argument(
        "--default-processed-root",
        type=Path,
        help="reuse trusted default-model outputs containing clean/ and noisy/ subdirectories",
    )
    return parser.parse_args()


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def condition_rows(path: Path, member: str) -> dict[str, dict[str, str | float]]:
    with zipfile.ZipFile(path) as archive:
        lines = archive.read(member).decode("utf-8").splitlines()
    result = {}
    for line in lines:
        case_id, noise, snr = line.split()
        result[case_id] = {"noise": noise, "snr_db": float(snr)}
    return result


def choose_cases(arguments: argparse.Namespace) -> list[dict]:
    if arguments.case_manifest is not None:
        manifest = json.loads(arguments.case_manifest.read_text(encoding="utf-8"))
        cases = []
        for source in manifest["cases"]:
            case = dict(source)
            case.setdefault("speaker", case.get("language", "unknown"))
            case.setdefault("noise", "none")
            case.setdefault("snr_db", math.inf)
            case_id = str(case["case_id"])
            if not (arguments.clean_dir / f"{case_id}.wav").is_file():
                raise FileNotFoundError(arguments.clean_dir / f"{case_id}.wav")
            if not (arguments.noisy_dir / f"{case_id}.wav").is_file():
                raise FileNotFoundError(arguments.noisy_dir / f"{case_id}.wav")
            cases.append(case)
        return sorted(cases, key=lambda row: str(row["case_id"]))[: arguments.limit]

    speakers = {value.strip() for value in arguments.speakers.split(",") if value.strip()}
    if not speakers:
        raise ValueError("--speakers must contain at least one speaker")
    conditions = condition_rows(arguments.condition_log, arguments.condition_member)
    groups: dict[tuple[str, str, float], list[dict]] = defaultdict(list)
    for clean in sorted(arguments.clean_dir.glob("*.wav")):
        case_id = clean.stem
        speaker = case_id.split("_", 1)[0]
        if speaker not in speakers:
            continue
        noisy = arguments.noisy_dir / clean.name
        if not noisy.is_file() or case_id not in conditions:
            raise ValueError(f"missing noisy pair or condition for {case_id}")
        row = {"case_id": case_id, "speaker": speaker, **conditions[case_id]}
        groups[(speaker, str(row["noise"]), float(row["snr_db"]))].append(row)
    if not groups:
        raise ValueError("no held-out cases matched the selected speakers")

    selected = []
    ordered_groups = [groups[key] for key in sorted(groups)]
    offset = 0
    while len(selected) < arguments.limit:
        added = False
        for group in ordered_groups:
            if offset < len(group):
                selected.append(group[offset])
                added = True
                if len(selected) == arguments.limit:
                    break
        if not added:
            break
        offset += 1
    return selected


def read_audio(path: Path) -> np.ndarray:
    audio, rate = sf.read(path, dtype="float32")
    if rate != SAMPLE_RATE or audio.ndim != 1:
        raise ValueError(f"expected mono 48 kHz audio: {path}")
    if not np.all(np.isfinite(audio)):
        raise ValueError(f"non-finite audio: {path}")
    return audio


def si_sdr(clean: np.ndarray, degraded: np.ndarray) -> float:
    clean64 = clean.astype(np.float64)
    degraded64 = degraded.astype(np.float64)
    clean64 -= np.mean(clean64)
    degraded64 -= np.mean(degraded64)
    projection = np.dot(degraded64, clean64) / (np.dot(clean64, clean64) + 1.0e-15) * clean64
    residual = degraded64 - projection
    return float(
        10.0
        * np.log10(
            (np.dot(projection, projection) + 1.0e-15)
            / (np.dot(residual, residual) + 1.0e-15)
        )
    )


def frame_view(audio: np.ndarray, size: int, hop: int) -> np.ndarray:
    if audio.size < size:
        return audio.reshape(1, -1)
    return np.lib.stride_tricks.sliding_window_view(audio, size)[::hop]


def segmental_snr(clean: np.ndarray, degraded: np.ndarray) -> float:
    size = round(SAMPLE_RATE * 0.020)
    hop = size // 2
    clean_frames = frame_view(clean, size, hop)
    degraded_frames = frame_view(degraded, size, hop)
    count = min(len(clean_frames), len(degraded_frames))
    clean_frames = clean_frames[:count]
    degraded_frames = degraded_frames[:count]
    energy = np.mean(np.square(clean_frames, dtype=np.float64), axis=1)
    active = energy >= max(float(np.max(energy)) * 1.0e-4, 1.0e-6)
    error = np.mean(np.square(clean_frames - degraded_frames, dtype=np.float64), axis=1)
    values = np.clip(10.0 * np.log10((energy + 1.0e-15) / (error + 1.0e-15)), -10.0, 35.0)
    return float(np.mean(values[active])) if np.any(active) else math.nan


def scores(clean: np.ndarray, degraded: np.ndarray) -> dict[str, float]:
    return {
        "stoi": float(stoi(clean, degraded, SAMPLE_RATE, extended=False)),
        "si_sdr_db": si_sdr(clean, degraded),
        "segmental_snr_db": segmental_snr(clean, degraded),
    }


def render_one(
    lab: Path,
    model: Path,
    source: Path,
    output: Path,
    force: bool,
) -> None:
    if output.is_file() and not force:
        return
    output.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [str(lab), *MODEL_ARGUMENTS, "--model-file", str(model), str(source), str(output)],
        check=True,
        stdout=subprocess.DEVNULL,
    )


def score_case(
    case: dict,
    clean_dir: Path,
    noisy_dir: Path,
    models: list[Path],
    roots: dict[Path, Path],
    identities: dict[Path, str],
    strengths: dict[Path, float],
    skip_clean_render: bool,
) -> list[dict]:
    name = f"{case['case_id']}.wav"
    clean = read_audio(clean_dir / name)
    noisy = read_audio(noisy_dir / name)
    noisy_scores = scores(clean, noisy)
    rows = []
    for model in models:
        root = roots[model]
        processed = read_audio(root / "noisy" / name)
        clean_processed = None if skip_clean_render else read_audio(root / "clean" / name)
        if len(clean) != len(processed) or (
            clean_processed is not None and len(clean) != len(clean_processed)
        ):
            raise ValueError(f"length mismatch for {case['case_id']} / {model}")
        strength = strengths[model]
        if strength < 1.0:
            processed = noisy * (1.0 - strength) + processed * strength
            if clean_processed is not None:
                clean_processed = clean * (1.0 - strength) + clean_processed * strength
        processed_scores = scores(clean, processed)
        clean_scores = scores(clean, clean_processed) if clean_processed is not None else None
        row = {
            **case,
            "model": identities[model],
            "model_path": str(model),
            "newly_clipped": bool(np.max(np.abs(processed)) >= 1.0 and np.max(np.abs(noisy)) < 1.0),
            "non_finite_samples": int(np.count_nonzero(~np.isfinite(processed))),
            "clean_stoi_damage": (
                max(0.0, 1.0 - clean_scores["stoi"]) if clean_scores is not None else math.nan
            ),
        }
        for metric, value in processed_scores.items():
            row[metric] = value
            row[f"{metric}_improvement"] = value - noisy_scores[metric]
        rows.append(row)
    return rows


def distribution(values) -> dict[str, float | int]:
    array = np.asarray(list(values), dtype=np.float64)
    array = array[np.isfinite(array)]
    if not array.size:
        return {"count": 0}
    return {
        "count": int(array.size),
        "mean": float(np.mean(array)),
        "median": float(np.median(array)),
        "p05": float(np.percentile(array, 5)),
        "p95": float(np.percentile(array, 95)),
        "minimum": float(np.min(array)),
        "maximum": float(np.max(array)),
    }


def main() -> None:
    arguments = parse_args()
    if not 8 <= arguments.limit <= 2_000:
        raise ValueError("--limit must be within 8..=2000")
    if not 1 <= arguments.workers <= 8:
        raise ValueError("--workers must be within 1..=8")
    if not math.isfinite(arguments.candidate_strength) or not 0.0 <= arguments.candidate_strength <= 1.0:
        raise ValueError("--candidate-strength must be finite and within 0..=1")
    if not arguments.quality_lab.is_file():
        raise FileNotFoundError(arguments.quality_lab)

    models = [arguments.default_model, *arguments.models]
    for model in models:
        if not model.is_file():
            raise FileNotFoundError(model)
    cases = choose_cases(arguments)
    render_identities = {model: f"{model.stem}-{digest(model)[:12]}" for model in models}
    strengths = {
        model: 1.0 if model == arguments.default_model else arguments.candidate_strength
        for model in models
    }
    identities = {
        model: (
            render_identities[model]
            if strengths[model] == 1.0
            else f"{render_identities[model]}-mix{round(strengths[model] * 100):03d}"
        )
        for model in models
    }
    roots = {
        model: (
            arguments.default_processed_root
            if model == arguments.default_model and arguments.default_processed_root is not None
            else arguments.output_dir / "audio" / render_identities[model]
        )
        for model in models
    }
    if arguments.default_processed_root is not None:
        required_kinds = ("noisy",) if arguments.skip_clean_render else ("clean", "noisy")
        for kind in required_kinds:
            if not (arguments.default_processed_root / kind).is_dir():
                raise FileNotFoundError(arguments.default_processed_root / kind)

    jobs = []
    for model in models:
        if model == arguments.default_model and arguments.default_processed_root is not None:
            continue
        root = roots[model]
        for case in cases:
            name = f"{case['case_id']}.wav"
            jobs.append(
                (arguments.quality_lab, model, arguments.noisy_dir / name, root / "noisy" / name, arguments.force)
            )
            if not arguments.skip_clean_render:
                jobs.append(
                    (arguments.quality_lab, model, arguments.clean_dir / name, root / "clean" / name, arguments.force)
                )
    with concurrent.futures.ThreadPoolExecutor(max_workers=arguments.workers) as executor:
        list(executor.map(lambda job: render_one(*job), jobs))

    with concurrent.futures.ThreadPoolExecutor(max_workers=arguments.workers) as executor:
        case_rows = executor.map(
            lambda case: score_case(
                case,
                arguments.clean_dir,
                arguments.noisy_dir,
                models,
                roots,
                identities,
                strengths,
                arguments.skip_clean_render,
            ),
            cases,
        )
        rows = [row for grouped_rows in case_rows for row in grouped_rows]

    by_model = {identity: [row for row in rows if row["model"] == identity] for identity in identities.values()}
    default_identity = identities[arguments.default_model]
    baseline_by_case = {row["case_id"]: row for row in by_model[default_identity]}
    summaries = {}
    for model, identity in identities.items():
        model_rows = by_model[identity]
        comparisons = {
            field: distribution(
                row[field] - baseline_by_case[row["case_id"]][field] for row in model_rows
            )
            for field in ("stoi", "si_sdr_db", "segmental_snr_db", "clean_stoi_damage")
        }
        summaries[identity] = {
            "path": str(model),
            "sha256": digest(model),
            "metrics": {
                field: distribution(row[field] for row in model_rows)
                for field in (
                    "stoi_improvement",
                    "si_sdr_db_improvement",
                    "segmental_snr_db_improvement",
                    "clean_stoi_damage",
                )
            },
            "versus_default": comparisons,
            "newly_clipped_files": sum(bool(row["newly_clipped"]) for row in model_rows),
            "non_finite_samples": sum(int(row["non_finite_samples"]) for row in model_rows),
        }

    report = {
        "schema_version": 1,
        "selection_split": "VoiceBank-DEMAND 28-speaker train: held-out speakers only",
        "speakers": sorted({str(case["speaker"]) for case in cases}),
        "case_count": len(cases),
        "model_arguments": list(MODEL_ARGUMENTS),
        "candidate_strength": arguments.candidate_strength,
        "default_model": default_identity,
        "summaries": summaries,
        "rows": rows,
    }
    arguments.output_dir.mkdir(parents=True, exist_ok=True)
    report_path = arguments.output_dir / "candidate-screen.json"
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    for identity, result in summaries.items():
        delta = result["versus_default"]
        print(
            f"{identity}: STOI vs default median={delta['stoi'].get('median', math.nan):+.6f}; "
            f"SI-SDR={delta['si_sdr_db'].get('median', math.nan):+.3f} dB; "
            f"clean damage={delta['clean_stoi_damage'].get('median', math.nan):+.6f}"
        )
    print(report_path)


if __name__ == "__main__":
    main()
