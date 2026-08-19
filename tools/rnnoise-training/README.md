# RNNoise candidate training

This directory produces versioned `nnnoiseless`-compatible candidate weights.
It never replaces the embedded production weights. Data, features, virtual
environments, checkpoints, and rendered audio belong under ignored
`docs/.cache/rnnoise-training/`.

The quality-v1 candidate uses the official VoiceBank--DEMAND 28-speaker training
split (CC BY 4.0). Its 28 speakers are disjoint from the two-speaker test split
used by Noire's frozen 824-case evaluation. The feature generator recombines
clean utterances with independently selected residual noise, includes clean and
noise-only examples, emphasizes high-SNR cases, varies level and spectral
coloration, and uses a fixed seed. The paired files must be audited first:
VoiceBank mixtures do not always contain clean speech at unity gain, so blindly
subtracting `noisy - clean` leaks speech into the residual noise.

Estimate and audit each mixture's clean scale with:

```sh
docs/.venv-rnnoise/bin/python tools/rnnoise-training/audit_pairs.py \
  docs/.cache/rnnoise-training/data/clean_trainset_28spk_wav \
  docs/.cache/rnnoise-training/data/noisy_trainset_28spk_wav \
  docs/.cache/rnnoise-training/residual-scales.tsv \
  --report docs/.cache/rnnoise-training/pair-audit.json
```

Generate deterministic features with:

```sh
cargo run --release -p noire-model-rnnoise --features training-tools \
  --bin noire-rnnoise-features -- \
  --clean-dir docs/.cache/rnnoise-training/data/clean_trainset_28spk_wav \
  --noisy-dir docs/.cache/rnnoise-training/data/noisy_trainset_28spk_wav \
  --residual-scales docs/.cache/rnnoise-training/residual-scales.tsv \
  --output docs/.cache/rnnoise-training/features/train-v4-full-corrected.f32 \
  --frames 3000000 --seed 5642809475817688654 \
  --exclude-speakers p226,p287
```

Generate a smaller validation feature file from the two held-out training
speakers with the same command, changing the output, frame count, and final
filter to `--include-speakers p226,p287`. Use 200,000 validation frames and the
same residual-scale table. These speakers are for candidate selection; the
separate `p232`/`p257` test speakers remain untouched.

Fine-tune the frozen default model with a bounded CPU configuration:

```sh
docs/.venv-rnnoise/bin/python tools/rnnoise-training/train_candidate.py \
  docs/.cache/rnnoise-training/features/train-v4-full-corrected.f32 \
  docs/.cache/rnnoise-training/default.rnn \
  docs/.cache/rnnoise-training/candidate-v12-preserve50 \
  --validation-features docs/.cache/rnnoise-training/features/validation-v4-full-corrected.f32 \
  --epochs 4 --sequence-frames 400 --batch-size 64 --threads 4 \
  --learning-rate 0.00002 --trainable-profile denoise-output \
  --speech-gain-blend 0.50
```

For conservative fine-tuning, `--trainable-profile denoise-output` calibrates
only the final gain head while preserving every recurrent representation.
`denoise-tail`, `heads`, and `all` are progressively less conservative and
must be treated as separate experiments. `vad-output` isolates calibration of
the 25-parameter VAD head used by Noire's adaptive wet/dry policy.

`--speech-gain-blend` can conservatively bias supervised gain labels toward
speech preservation in proportion to the ground-truth VAD label. It does not
alter noise-only labels and is recorded in the training report.

Every epoch emits a separate `.rnn` file. Candidates must be screened on a
development split, then evaluated once on the frozen VoiceBank--DEMAND
and stress holdouts. The production model remains the fallback even after a
candidate passes automated tests, until blinded listening is complete.

Screen every checkpoint on a balanced sample from the two held-out training
speakers (the embedded default model is included automatically):

```sh
docs/.venv-quality/bin/python tools/rnnoise-training/screen_candidates.py \
  docs/.cache/rnnoise-training/candidate-v12-preserve50/candidate-epoch{01,02,03,04}.rnn \
  --limit 770 --workers 4
```

Epoch 3 became `rnnoise-quality-v1`. It improves median STOI on both the
development and frozen VoiceBank tests and slightly reduces clean-speech STOI
damage. It remains opt-in because the 952-case stress suite has improved median
metrics but a small negative mean STOI delta, concentrated in procedural music,
environmental audio, and state transitions. See the tracked qualification
manifests under `tests/quality/` for exact hashes, metrics, and promotion gates.
