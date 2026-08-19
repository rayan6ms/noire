# Retained RNNoise enhancement experiments

RNNoise quality-v1 is the frozen backup baseline after FastEnhancer-B won the
production comparison. Experimental code must
remain opt-in until it wins the hard-case qualification suite without regressing
clean speech, clipping, callback allocation, CPU, timing, or latency.

## Current prototype

`EnhancedRnnoiseFactory` is a separate model factory behind the
`experimental-enhancement` feature. It retains RNNoise and its declared
480-sample delay, then applies a causal multi-frame late-tail predictor. The
predictor uses audio at least 40 ms old, so it does not target the direct path or
useful early reflections. It disengages in 5 ms on speech and engages over
120 ms only when non-speech and positive late-path correlation agree. It adds no
lookahead. The existing `RnnoiseFactory` remains available for offline work;
production composition uses the separate FastEnhancer-B adapter.

`AdaptiveSuppressionController` is likewise opt-in. It reduces suppression
quickly on speech, requires three confident non-speech frames before restoring
pause attenuation, and then recovers over 150 ms. The shipped
`SpeechPreservingStrength` remains available for exact offline baseline A/B runs.

This is an infrastructure and dereverberation prototype, not a claim that the
overlapping-speaker goal has been met. A generic single-channel denoiser has no
reliable identity cue for choosing between two humans. The promotion suite
therefore requires measured overlapping-speech gains, and the current factory
must remain experimental until a learned core provides them.

## Learned multi-frame core

The next candidate should use a causal 48 kHz analysis/synthesis front end with
a 10 ms hop and past-frame complex filtering. A compact recurrent or temporal-
convolution encoder should predict:

- complex multi-frame filters for noise and late reverberation;
- a target-salience mask trained on near/primary-speaker continuity rather than
  an unsafe assumption that all speech is wanted;
- VAD, late-reverberation confidence, and target confidence for the controller;
- a clean-input identity residual that can collapse processing toward unity.

Start with zero lookahead. Permit at most one additional hop only if the
overlapping-speech and reverberation gains survive the latency gate. Quantize
and reduce feature width before growing the network. DeepFilterNet is a useful
reference for low-complexity 48 kHz multi-frame filtering and its public
training framework separates speech, noise, and RIR datasets, but a dependency
or pretrained model is not accepted on reputation alone; it must pass Noire's
same-host suite and license/provenance review. See the
[official project](https://github.com/Rikorose/DeepFilterNet).

## Training data and losses

Build mixtures from speaker-disjoint clean speech, environmental noise,
interfering speakers, measured and simulated RIRs, microphone responses, gain
and AGC variation, compression, codec artifacts, packet-loss concealment, and
mild nonlinear distortion. At least 25% of batches must contain clean or nearly
clean speech. Include whisper and shout level distributions rather than fixing
them with normalization.

Use a loss portfolio rather than optimizing SI-SDR alone:

- multi-resolution complex spectral and waveform reconstruction;
- intelligibility/ASR loss with explicit substitution, insertion, and deletion
  tracking;
- clean identity and phase-consistency penalties;
- consonant/transient preservation and target over-suppression penalties;
- late-tail energy loss beginning after the protected early-reflection window;
- modest SI-SDR weighting as one diagnostic term.

Curriculum order is clean identity, simple noise, reverberation, overlapping
speakers, then compound microphone/codec/nonlinear degradations. Expand mixture
diversity before model width or depth.

## Optional target-speaker mode

Personalization starts only after the generic path is mature. Enrollment and
embedding computation run outside the callback; the normalized embedding is
cached in fixed storage before stream activation. A conditioned enhancement
head receives that embedding and emits target confidence. The callback blends
generic and personalized outputs through `ConfidenceWeightedBlend`: engagement
is gradual, low confidence requires hysteresis, and fallback to generic is
faster but never discontinuous.

This direction matches the deployment constraints identified by
[VoiceFilter-Lite](https://google.github.io/speaker-id/publications/VoiceFilter-Lite/):
streaming target separation must improve overlap while not harming other
conditions, and adaptive suppression is part of the solution. Evaluation must
also track target-speaker over-suppression, as emphasized by
[Microsoft's personalized speech-enhancement evaluation](https://www.microsoft.com/en-us/research/publication/personalized-speech-enhancement-new-models-and-comprehensive-evaluation/).

## Promotion order

1. Freeze baseline executable, configuration, corpus hashes, ASR, and metrics.
2. Pass unit, finite-output, reset, chunking, allocation, clipping, and release
   timing gates.
3. Pass the clean-speech hard gate.
4. Show meaningful gains in both overlapping-speaker and reverberant buckets.
5. Show no aggregate or per-bucket intelligibility/naturalness regression.
6. Pass blinded listening for consonant loss, pumping, metallic artifacts,
   target stability, and reverberant tails.
7. Only then compare the enhanced factory again with the shipped FastEnhancer-B
   path; keep RNNoise repository-only unless it wins the complete suite.
