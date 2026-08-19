# RNNoise model artifacts

`rnnoise-quality-v1.rnn` is a versioned, opt-in `nnnoiseless`-compatible
candidate. Its SHA-256 is
`2f0958c50378499cbd8869723b7dc214a65873304ec81856d3c084c08c0e9048`.

It was initialized from `nnnoiseless` 0.5.2's embedded RNNoise weights and
fine-tuned on the official VoiceBank--DEMAND 28-speaker training set. The model
changes only the final denoise-gain output head. The source implementation is
BSD-3-Clause and the VoiceBank--DEMAND data is CC BY 4.0; retain both notices
when redistributing this artifact.

This model is not the production default. It improves STOI and clean-speech
preservation on the frozen VoiceBank test, but has a small mean STOI regression
on the broader stress suite, concentrated in procedural music and environmental
audio. Use `RnnoiseCandidateFactory::quality_v1()` to select it explicitly.
The original embedded model remains available through `RnnoiseFactory`.

The complete data, training, and evaluation record is in
`tests/quality/rnnoise-training-v1.toml` and
`tests/quality/rnnoise-quality-v1.toml`.
