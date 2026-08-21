#!/usr/bin/env python3
"""Fine-tune the frozen nnnoiseless RNNoise network on deterministic features."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import struct
from pathlib import Path
from typing import Any


ROW_VALUES = 87
FEATURE_VALUES = 42
GAIN_VALUES = 22
DEFAULT_SEED = 0x4E4F4952


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("features", type=Path)
    parser.add_argument("initial_model", type=Path)
    parser.add_argument("output_directory", type=Path)
    parser.add_argument("--epochs", type=int, default=5)
    parser.add_argument("--sequence-frames", type=int, default=400)
    parser.add_argument("--batch-size", type=int, default=16)
    parser.add_argument("--learning-rate", type=float, default=1.0e-4)
    parser.add_argument("--speech-gain-blend", type=float, default=0.0)
    parser.add_argument(
        "--trainable-profile",
        choices=("all", "denoise-tail", "heads", "denoise-output", "vad-output"),
        default="all",
    )
    parser.add_argument("--validation-fraction", type=float, default=0.1)
    parser.add_argument("--validation-features", type=Path)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    return parser.parse_args()


def configure_runtime(arguments: argparse.Namespace) -> None:
    if arguments.threads < 1 or arguments.threads > 8:
        raise ValueError("--threads must be within 1..=8")
    os.environ.setdefault("CUDA_VISIBLE_DEVICES", "-1")
    os.environ.setdefault("OMP_NUM_THREADS", str(arguments.threads))
    os.environ.setdefault("TF_NUM_INTRAOP_THREADS", str(arguments.threads))
    os.environ.setdefault("TF_NUM_INTEROP_THREADS", "1")
    os.environ.setdefault("TF_CPP_MIN_LOG_LEVEL", "2")


def sha256(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def audit_features(path: Path) -> dict[str, Any]:
    import numpy as np

    row_bytes = ROW_VALUES * np.dtype("<f4").itemsize
    byte_count = path.stat().st_size
    if byte_count == 0 or byte_count % row_bytes:
        raise ValueError(f"invalid feature-file length: {path}")
    row_count = byte_count // row_bytes
    rows = np.memmap(path, dtype="<f4", mode="r", shape=(row_count, ROW_VALUES))
    finite = True
    valid_gain_range = True
    valid_vad_range = True
    gain_bins = np.zeros(3, dtype=np.int64)
    masked_frames = 0
    vad_bins = np.zeros(3, dtype=np.int64)
    for start in range(0, row_count, 100_000):
        chunk = rows[start : start + 100_000]
        finite = finite and bool(np.all(np.isfinite(chunk)))
        gains = chunk[:, FEATURE_VALUES : FEATURE_VALUES + GAIN_VALUES]
        vad = chunk[:, -1]
        valid_gain_range = valid_gain_range and bool(np.all((gains >= -1.0) & (gains <= 1.0)))
        valid_vad_range = valid_vad_range and bool(np.all((vad >= 0.0) & (vad <= 1.0)))
        valid = gains != -1.0
        counts = np.sum(valid, axis=1)
        masked_frames += int(np.count_nonzero(counts == 0))
        means = np.divide(
            np.sum(np.where(valid, gains, 0.0), axis=1),
            counts,
            out=np.full(len(gains), np.nan),
            where=counts != 0,
        )
        gain_bins += np.asarray(
            [
                np.count_nonzero(means < 1.0 / 3.0),
                np.count_nonzero((means >= 1.0 / 3.0) & (means <= 2.0 / 3.0)),
                np.count_nonzero(means > 2.0 / 3.0),
            ],
            dtype=np.int64,
        )
        vad_bins += np.asarray(
            [
                np.count_nonzero(vad == 0.0),
                np.count_nonzero(vad == 0.5),
                np.count_nonzero(vad == 1.0),
            ],
            dtype=np.int64,
        )
    if not finite or not valid_gain_range or not valid_vad_range:
        raise ValueError(f"feature audit failed for {path}")
    return {
        "sha256": sha256(path),
        "rows": row_count,
        "finite": finite,
        "gain_range_valid": valid_gain_range,
        "vad_range_valid": valid_vad_range,
        "gain_mean_bins": {
            "low": int(gain_bins[0]),
            "medium": int(gain_bins[1]),
            "high": int(gain_bins[2]),
            "fully_masked": masked_frames,
        },
        "vad_bins": {
            "inactive": int(vad_bins[0]),
            "uncertain": int(vad_bins[1]),
            "active": int(vad_bins[2]),
        },
    }


def read_i8(data: bytes, offset: int, count: int) -> tuple[Any, int]:
    import numpy as np

    end = offset + count
    if end > len(data):
        raise ValueError("truncated model")
    return np.frombuffer(data[offset:end], dtype=np.int8).astype(np.float32) / 256.0, end


def read_layer(data: bytes, offset: int, recurrent: bool) -> tuple[dict[str, Any], int]:
    if offset + 3 > len(data):
        raise ValueError("truncated model layer header")
    inputs, neurons, activation = struct.unpack_from("bbb", data, offset)
    offset += 3
    if inputs <= 0 or neurons <= 0 or activation not in (0, 1, 2):
        raise ValueError("invalid model layer header")
    multiplier = 3 if recurrent else 1
    kernel, offset = read_i8(data, offset, inputs * neurons * multiplier)
    result: dict[str, Any] = {
        "inputs": inputs,
        "neurons": neurons,
        "activation": activation,
        "kernel": kernel.reshape(inputs, neurons * multiplier),
    }
    if recurrent:
        recurrent_kernel, offset = read_i8(data, offset, neurons * neurons * 3)
        result["recurrent_kernel"] = recurrent_kernel.reshape(neurons, neurons * 3)
    bias, offset = read_i8(data, offset, neurons * multiplier)
    result["bias"] = bias
    return result, offset


def read_model(path: Path) -> dict[str, dict[str, Any]]:
    data = path.read_bytes()
    offset = 0
    result: dict[str, dict[str, Any]] = {}
    for name, recurrent in (
        ("input_dense", False),
        ("vad_gru", True),
        ("noise_gru", True),
        ("denoise_gru", True),
        ("denoise_output", False),
        ("vad_output", False),
    ):
        result[name], offset = read_layer(data, offset, recurrent)
    if offset != len(data):
        raise ValueError(f"model has {len(data) - offset} trailing bytes")
    return result


def activation_name(code: int) -> str:
    return {0: "tanh", 1: "sigmoid", 2: "relu"}[code]


def build_model(
    initial: dict[str, dict[str, Any]], learning_rate: float, trainable_profile: str
):
    import keras
    import tensorflow as tf

    class WeightClip(keras.constraints.Constraint):
        def __call__(self, weights):
            return tf.clip_by_value(weights, -0.499, 0.499)

        def get_config(self):
            return {}

    constraint = WeightClip()
    regularizer = keras.regularizers.L2(1.0e-6)
    inputs = keras.Input(shape=(None, FEATURE_VALUES), name="main_input")
    dense = keras.layers.Dense(
        initial["input_dense"]["neurons"],
        activation=activation_name(initial["input_dense"]["activation"]),
        kernel_constraint=constraint,
        bias_constraint=constraint,
        name="input_dense",
    )(inputs)
    vad_gru = keras.layers.GRU(
        initial["vad_gru"]["neurons"],
        reset_after=False,
        activation=activation_name(initial["vad_gru"]["activation"]),
        recurrent_activation="sigmoid",
        return_sequences=True,
        kernel_regularizer=regularizer,
        recurrent_regularizer=regularizer,
        kernel_constraint=constraint,
        recurrent_constraint=constraint,
        bias_constraint=constraint,
        name="vad_gru",
    )(dense)
    vad_output = keras.layers.Dense(
        1,
        activation="sigmoid",
        kernel_constraint=constraint,
        bias_constraint=constraint,
        name="vad_output",
    )(vad_gru)
    noise_input = keras.layers.Concatenate(name="noise_input")([dense, vad_gru, inputs])
    noise_gru = keras.layers.GRU(
        initial["noise_gru"]["neurons"],
        reset_after=False,
        activation=activation_name(initial["noise_gru"]["activation"]),
        recurrent_activation="sigmoid",
        return_sequences=True,
        kernel_regularizer=regularizer,
        recurrent_regularizer=regularizer,
        kernel_constraint=constraint,
        recurrent_constraint=constraint,
        bias_constraint=constraint,
        name="noise_gru",
    )(noise_input)
    denoise_input = keras.layers.Concatenate(name="denoise_input")([vad_gru, noise_gru, inputs])
    denoise_gru = keras.layers.GRU(
        initial["denoise_gru"]["neurons"],
        reset_after=False,
        activation=activation_name(initial["denoise_gru"]["activation"]),
        recurrent_activation="sigmoid",
        return_sequences=True,
        kernel_regularizer=regularizer,
        recurrent_regularizer=regularizer,
        kernel_constraint=constraint,
        recurrent_constraint=constraint,
        bias_constraint=constraint,
        name="denoise_gru",
    )(denoise_input)
    gains = keras.layers.Dense(
        GAIN_VALUES,
        activation="sigmoid",
        kernel_constraint=constraint,
        bias_constraint=constraint,
        name="denoise_output",
    )(denoise_gru)
    model = keras.Model(inputs=inputs, outputs={"denoise_output": gains, "vad_output": vad_output})
    for name, values in initial.items():
        layer = model.get_layer(name)
        weights = [values["kernel"]]
        if "recurrent_kernel" in values:
            weights.append(values["recurrent_kernel"])
        weights.append(values["bias"])
        layer.set_weights(weights)

    trainable_layers = {
        "all": set(initial),
        "denoise-tail": {"denoise_gru", "denoise_output"},
        "heads": {"denoise_output", "vad_output"},
        "denoise-output": {"denoise_output"},
        "vad-output": {"vad_output"},
    }[trainable_profile]
    for name in initial:
        model.get_layer(name).trainable = name in trainable_layers

    def gain_loss(target, prediction):
        mask = tf.minimum(target + 1.0, 1.0)
        root_error = tf.sqrt(tf.maximum(prediction, 1.0e-7)) - tf.sqrt(tf.maximum(target, 0.0))
        binary = keras.losses.binary_crossentropy(target, prediction)
        while len(binary.shape) < len(root_error.shape):
            binary = tf.expand_dims(binary, axis=-1)
        value = 10.0 * tf.square(tf.square(root_error)) + tf.square(root_error) + 0.01 * binary
        return tf.reduce_mean(mask * value, axis=-1)

    def vad_loss(target, prediction):
        weight = 2.0 * tf.abs(target - 0.5)
        binary = keras.losses.binary_crossentropy(target, prediction)
        if len(binary.shape) < len(weight.shape):
            binary = tf.expand_dims(binary, axis=-1)
        return tf.reduce_mean(weight * binary, axis=-1)

    model.compile(
        optimizer=keras.optimizers.Adam(learning_rate=learning_rate, clipnorm=1.0),
        loss={"denoise_output": gain_loss, "vad_output": vad_loss},
        loss_weights={"denoise_output": 10.0, "vad_output": 0.5},
    )
    return model


def quantized_bytes(values) -> bytes:
    import numpy as np

    quantized = np.clip(np.rint(np.asarray(values).reshape(-1) * 256.0), -128, 127).astype(np.int8)
    return quantized.tobytes()


def export_model(model, destination: Path) -> None:
    import keras

    result = bytearray()
    for name, recurrent in (
        ("input_dense", False),
        ("vad_gru", True),
        ("noise_gru", True),
        ("denoise_gru", True),
        ("denoise_output", False),
        ("vad_output", False),
    ):
        layer = model.get_layer(name)
        weights = layer.get_weights()
        activation = {"tanh": 0, "sigmoid": 1, "relu": 2}.get(
            keras.activations.serialize(layer.activation)
        )
        if activation is None:
            raise ValueError(f"unsupported activation on {name}")
        inputs, packed_neurons = weights[0].shape
        neurons = packed_neurons // 3 if recurrent else packed_neurons
        if inputs > 127 or neurons > 127:
            raise ValueError("nnnoiseless layer dimensions exceed its byte format")
        result.extend(struct.pack("bbb", inputs, neurons, activation))
        for values in weights:
            result.extend(quantized_bytes(values))
    destination.write_bytes(result)


def feature_sequences(path: Path, sequence_frames: int, speech_gain_blend: float):
    import numpy as np

    byte_count = path.stat().st_size
    row_bytes = ROW_VALUES * np.dtype("<f4").itemsize
    if byte_count == 0 or byte_count % row_bytes:
        raise ValueError("feature file size is not a non-empty multiple of one row")
    row_count = byte_count // row_bytes
    sequence_count = row_count // sequence_frames
    if sequence_count < 2:
        raise ValueError("feature file contains fewer than two complete sequences")
    rows = np.memmap(path, dtype="<f4", mode="r", shape=(row_count, ROW_VALUES))
    usable = rows[: sequence_count * sequence_frames].reshape(
        sequence_count, sequence_frames, ROW_VALUES
    )

    def split(values):
        x = values[:, :, :FEATURE_VALUES]
        gains = values[:, :, FEATURE_VALUES : FEATURE_VALUES + GAIN_VALUES]
        vad = values[:, :, -1:]
        if speech_gain_blend > 0.0:
            gains = np.array(gains, copy=True)
            valid = gains != -1.0
            preservation = speech_gain_blend * vad
            gains = np.where(valid, gains + preservation * (1.0 - gains), gains)
        return x, {"denoise_output": gains, "vad_output": vad}

    return split(usable), row_count, sequence_count


def load_training_data(
    path: Path,
    validation_path: Path | None,
    sequence_frames: int,
    validation_fraction: float,
    speech_gain_blend: float,
):
    training, row_count, sequence_count = feature_sequences(
        path, sequence_frames, speech_gain_blend
    )
    if validation_path is not None:
        validation, validation_rows, validation_sequences = feature_sequences(
            validation_path, sequence_frames, speech_gain_blend
        )
        return (
            training,
            validation,
            row_count,
            sequence_count,
            validation_rows,
            validation_sequences,
        )
    if not 0.01 <= validation_fraction <= 0.25:
        raise ValueError("--validation-fraction must be within 0.01..=0.25")
    validation_sequences = max(1, round(sequence_count * validation_fraction))
    split_at = sequence_count - validation_sequences
    if split_at < 1:
        raise ValueError("validation fraction leaves no training sequence")
    train_x, train_y = training
    validation = (
        train_x[split_at:],
        {name: values[split_at:] for name, values in train_y.items()},
    )
    training = (
        train_x[:split_at],
        {name: values[:split_at] for name, values in train_y.items()},
    )
    return training, validation, row_count, sequence_count, None, validation_sequences


def main() -> None:
    arguments = parse_args()
    configure_runtime(arguments)
    import keras

    if arguments.epochs < 1 or arguments.epochs > 100:
        raise ValueError("--epochs must be within 1..=100")
    if arguments.sequence_frames < 50 or arguments.sequence_frames > 2_000:
        raise ValueError("--sequence-frames must be within 50..=2000")
    if not 0.0 <= arguments.speech_gain_blend <= 0.5:
        raise ValueError("--speech-gain-blend must be within 0..=0.5")
    arguments.output_directory.mkdir(parents=True, exist_ok=True)
    keras.utils.set_random_seed(arguments.seed)
    training_audit = audit_features(arguments.features)
    validation_audit = (
        audit_features(arguments.validation_features) if arguments.validation_features else None
    )
    initial = read_model(arguments.initial_model)
    model = build_model(initial, arguments.learning_rate, arguments.trainable_profile)
    training, validation, row_count, sequence_count, validation_rows, validation_sequences = (
        load_training_data(
            arguments.features,
            arguments.validation_features,
            arguments.sequence_frames,
            arguments.validation_fraction,
            arguments.speech_gain_blend,
        )
    )

    initial_export = arguments.output_directory / "candidate-epoch00.rnn"
    export_model(model, initial_export)
    if initial_export.read_bytes() != arguments.initial_model.read_bytes():
        raise ValueError("initial model did not survive load/export exactly")

    class CandidateCheckpoint(keras.callbacks.Callback):
        def on_epoch_end(self, epoch, logs=None):
            export_model(self.model, arguments.output_directory / f"candidate-epoch{epoch + 1:02d}.rnn")

    history = model.fit(
        training[0],
        training[1],
        validation_data=validation,
        epochs=arguments.epochs,
        batch_size=arguments.batch_size,
        shuffle=True,
        callbacks=[CandidateCheckpoint()],
        verbose=2,
    )
    candidate_files = sorted(arguments.output_directory.glob("candidate-epoch*.rnn"))
    manifest = {
        "schema_version": 1,
        "features": str(arguments.features),
        "validation_features": (
            str(arguments.validation_features) if arguments.validation_features else None
        ),
        "feature_bytes": arguments.features.stat().st_size,
        "rows": row_count,
        "sequences": sequence_count,
        "validation_rows": validation_rows,
        "validation_sequences": validation_sequences,
        "sequence_frames": arguments.sequence_frames,
        "batch_size": arguments.batch_size,
        "epochs": arguments.epochs,
        "learning_rate": arguments.learning_rate,
        "speech_gain_blend": arguments.speech_gain_blend,
        "trainable_profile": arguments.trainable_profile,
        "threads": arguments.threads,
        "seed": arguments.seed,
        "initial_model_sha256": sha256(arguments.initial_model),
        "training_feature_audit": training_audit,
        "validation_feature_audit": validation_audit,
        "candidate_sha256": {path.name: sha256(path) for path in candidate_files},
        "history": {key: [float(value) for value in values] for key, values in history.history.items()},
    }
    (arguments.output_directory / "training-report.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()
