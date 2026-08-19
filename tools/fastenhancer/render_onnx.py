#!/usr/bin/env python3
"""Render mono 48 kHz WAV files with the official streaming FastEnhancer ONNX graph."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import math
import threading
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
import soundfile as sf


SAMPLE_RATE = 48_000
HOP_SAMPLES = 512
FFT_SAMPLES = 1_024
_THREAD_STATE = threading.local()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("model", type=Path)
    parser.add_argument("input_dir", type=Path)
    parser.add_argument("output_dir", type=Path)
    parser.add_argument("--workers", type=int, default=1)
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--report", type=Path)
    return parser.parse_args()


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def create_session(model: Path) -> ort.InferenceSession:
    options = ort.SessionOptions()
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    options.intra_op_num_threads = 1
    options.inter_op_num_threads = 1
    return ort.InferenceSession(
        model,
        sess_options=options,
        providers=["CPUExecutionProvider"],
    )


def thread_session(model: Path) -> ort.InferenceSession:
    session = getattr(_THREAD_STATE, "session", None)
    if session is None:
        session = create_session(model)
        _THREAD_STATE.session = session
    return session


def validate_graph(session: ort.InferenceSession) -> None:
    inputs = {value.name: value.shape for value in session.get_inputs()}
    outputs = {value.name: value.shape for value in session.get_outputs()}
    if inputs.get("wav_in") != [1, HOP_SAMPLES] or outputs.get("wav_out") != [1, HOP_SAMPLES]:
        raise ValueError("model does not expose the expected 48 kHz FastEnhancer waveform contract")
    cache_inputs = sorted(name for name in inputs if name.startswith("cache_in_"))
    cache_outputs = sorted(name for name in outputs if name.startswith("cache_out_"))
    if not cache_inputs or len(cache_inputs) != len(cache_outputs):
        raise ValueError("model has an invalid streaming-cache contract")


def read_audio(path: Path) -> np.ndarray:
    audio, rate = sf.read(path, dtype="float32")
    if rate != SAMPLE_RATE or audio.ndim != 1:
        raise ValueError(f"expected mono 48 kHz audio: {path}")
    if not np.all(np.isfinite(audio)):
        raise ValueError(f"non-finite input audio: {path}")
    return np.clip(audio, -1.0, 1.0)


def enhance(session: ort.InferenceSession, audio: np.ndarray) -> np.ndarray:
    length = int(audio.size)
    padded = np.pad(audio.reshape(1, -1), ((0, 0), (0, FFT_SAMPLES)))
    inputs = {
        value.name: np.zeros(value.shape, dtype=np.float32)
        for value in session.get_inputs()
        if value.name.startswith("cache_in_")
    }
    chunks = []
    for offset in range(0, length + FFT_SAMPLES - HOP_SAMPLES, HOP_SAMPLES):
        inputs["wav_in"] = padded[:, offset : offset + HOP_SAMPLES]
        outputs = session.run(None, inputs)
        chunks.append(outputs[0][0])
        for index, cache in enumerate(outputs[1:]):
            inputs[f"cache_in_{index}"] = cache
    rendered = np.concatenate(chunks)
    delay = FFT_SAMPLES - HOP_SAMPLES
    rendered = rendered[delay : delay + length]
    if rendered.size != length or not np.all(np.isfinite(rendered)):
        raise ValueError("model produced malformed output")
    return np.clip(rendered, -1.0, 1.0)


def render_one(model: Path, source: Path, destination: Path, force: bool) -> dict[str, float | int | str]:
    if destination.is_file() and not force:
        audio = read_audio(source)
        return {
            "file": source.name,
            "samples": int(audio.size),
            "seconds": float(audio.size / SAMPLE_RATE),
            "wall_seconds": 0.0,
            "cached": 1,
        }
    session = thread_session(model)
    audio = read_audio(source)
    started = time.perf_counter()
    rendered = enhance(session, audio)
    wall_seconds = time.perf_counter() - started
    destination.parent.mkdir(parents=True, exist_ok=True)
    sf.write(destination, rendered, SAMPLE_RATE, subtype="FLOAT")
    return {
        "file": source.name,
        "samples": int(audio.size),
        "seconds": float(audio.size / SAMPLE_RATE),
        "wall_seconds": wall_seconds,
        "cached": 0,
    }


def main() -> None:
    arguments = parse_args()
    if not arguments.model.is_file():
        raise FileNotFoundError(arguments.model)
    if not 1 <= arguments.workers <= 4:
        raise ValueError("--workers must be within 1..=4")
    sources = sorted(arguments.input_dir.glob("*.wav"))
    if not sources:
        raise ValueError(f"no WAV files found under {arguments.input_dir}")
    validate_graph(create_session(arguments.model))

    jobs = [
        (arguments.model, source, arguments.output_dir / source.name, arguments.force)
        for source in sources
    ]
    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=arguments.workers) as executor:
        rows = list(executor.map(lambda job: render_one(*job), jobs))
    total_wall_seconds = time.perf_counter() - started
    audio_seconds = math.fsum(float(row["seconds"]) for row in rows)
    inference_wall_seconds = math.fsum(float(row["wall_seconds"]) for row in rows)
    report = {
        "schema_version": 1,
        "model": str(arguments.model),
        "model_sha256": digest(arguments.model),
        "sample_rate_hz": SAMPLE_RATE,
        "hop_samples": HOP_SAMPLES,
        "delay_samples_compensated": FFT_SAMPLES - HOP_SAMPLES,
        "workers": arguments.workers,
        "files": len(rows),
        "audio_seconds": audio_seconds,
        "inference_wall_seconds": inference_wall_seconds,
        "total_wall_seconds": total_wall_seconds,
        "aggregate_worker_rtf": inference_wall_seconds / audio_seconds,
        "wall_rtf": total_wall_seconds / audio_seconds,
        "cached_files": sum(int(row["cached"]) for row in rows),
    }
    report_path = arguments.report or arguments.output_dir.parent / f"{arguments.output_dir.name}-render.json"
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
