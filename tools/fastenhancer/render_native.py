#!/usr/bin/env python3
"""Render mono 48 kHz WAV files through the native FastEnhancer C runtime."""

from __future__ import annotations

import argparse
import concurrent.futures
import ctypes
import hashlib
import json
import math
import threading
import time
from pathlib import Path

import numpy as np
import soundfile as sf


SAMPLE_RATE = 48_000
HOP_SAMPLES = 512
DELAY_SAMPLES = 512
MODEL_BASE = 1
_THREAD_STATE = threading.local()


class NativeSession:
    def __init__(self, library_path: Path, weights_path: Path) -> None:
        self.library = ctypes.CDLL(str(library_path.resolve()))
        self.library.fe_init.argtypes = [
            ctypes.c_int,
            ctypes.POINTER(ctypes.c_uint8),
            ctypes.c_int,
        ]
        self.library.fe_init.restype = ctypes.c_void_p
        self.library.fe_process.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_float),
            ctypes.POINTER(ctypes.c_float),
        ]
        self.library.fe_process.restype = ctypes.c_int
        self.library.fe_reset.argtypes = [ctypes.c_void_p]
        self.library.fe_destroy.argtypes = [ctypes.c_void_p]
        weights = weights_path.read_bytes()
        self._weights = (ctypes.c_uint8 * len(weights)).from_buffer_copy(weights)
        self.state = self.library.fe_init(MODEL_BASE, self._weights, len(weights))
        if not self.state:
            raise ValueError("native FastEnhancer rejected the weight artifact")

    def reset(self) -> None:
        self.library.fe_reset(self.state)

    def process(self, frame: np.ndarray) -> np.ndarray:
        source = np.ascontiguousarray(frame, dtype=np.float32)
        destination = np.empty(HOP_SAMPLES, dtype=np.float32)
        result = self.library.fe_process(
            self.state,
            source.ctypes.data_as(ctypes.POINTER(ctypes.c_float)),
            destination.ctypes.data_as(ctypes.POINTER(ctypes.c_float)),
        )
        if result != 0:
            raise ValueError("native FastEnhancer inference failed")
        return destination

    def close(self) -> None:
        if self.state:
            self.library.fe_destroy(self.state)
            self.state = None

    def __del__(self) -> None:
        self.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("library", type=Path)
    parser.add_argument("weights", type=Path)
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


def read_audio(path: Path) -> np.ndarray:
    audio, rate = sf.read(path, dtype="float32")
    if rate != SAMPLE_RATE or audio.ndim != 1 or not np.all(np.isfinite(audio)):
        raise ValueError(f"expected finite mono 48 kHz audio: {path}")
    return np.clip(audio, -1.0, 1.0)


def thread_session(library: Path, weights: Path) -> NativeSession:
    session = getattr(_THREAD_STATE, "session", None)
    if session is None:
        session = NativeSession(library, weights)
        _THREAD_STATE.session = session
    return session


def enhance(session: NativeSession, audio: np.ndarray) -> np.ndarray:
    session.reset()
    length = int(audio.size)
    padded_length = math.ceil((length + DELAY_SAMPLES) / HOP_SAMPLES) * HOP_SAMPLES
    padded = np.pad(audio, (0, padded_length - length))
    chunks = [
        session.process(padded[offset : offset + HOP_SAMPLES])
        for offset in range(0, padded_length, HOP_SAMPLES)
    ]
    rendered = np.concatenate(chunks)[DELAY_SAMPLES : DELAY_SAMPLES + length]
    if rendered.size != length or not np.all(np.isfinite(rendered)):
        raise ValueError("native FastEnhancer produced malformed output")
    return np.clip(rendered, -1.0, 1.0)


def render_one(
    library: Path,
    weights: Path,
    source: Path,
    destination: Path,
    force: bool,
) -> dict[str, float | int | str]:
    audio = read_audio(source)
    if destination.is_file() and not force:
        return {
            "file": source.name,
            "seconds": float(audio.size / SAMPLE_RATE),
            "wall_seconds": 0.0,
            "cached": 1,
        }
    session = thread_session(library, weights)
    started = time.perf_counter()
    rendered = enhance(session, audio)
    wall_seconds = time.perf_counter() - started
    destination.parent.mkdir(parents=True, exist_ok=True)
    sf.write(destination, rendered, SAMPLE_RATE, subtype="FLOAT")
    return {
        "file": source.name,
        "seconds": float(audio.size / SAMPLE_RATE),
        "wall_seconds": wall_seconds,
        "cached": 0,
    }


def main() -> None:
    arguments = parse_args()
    if not arguments.library.is_file() or not arguments.weights.is_file():
        raise FileNotFoundError("native library or weights are missing")
    if not 1 <= arguments.workers <= 8:
        raise ValueError("--workers must be within 1..=8")
    sources = sorted(arguments.input_dir.glob("*.wav"))
    if not sources:
        raise ValueError(f"no WAV files found under {arguments.input_dir}")
    probe = NativeSession(arguments.library, arguments.weights)
    probe.close()

    jobs = [
        (
            arguments.library,
            arguments.weights,
            source,
            arguments.output_dir / source.name,
            arguments.force,
        )
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
        "library": str(arguments.library),
        "library_sha256": digest(arguments.library),
        "weights": str(arguments.weights),
        "weights_sha256": digest(arguments.weights),
        "sample_rate_hz": SAMPLE_RATE,
        "hop_samples": HOP_SAMPLES,
        "delay_samples_compensated": DELAY_SAMPLES,
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
