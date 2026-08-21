# Enhancement hard-case qualification

This directory defines the post-1.0 enhancement gate. The existing RNNoise live
path remains the production baseline until a candidate passes this suite on the
same immutable corpus, host, ASR model, metric implementations, and listening
protocol.

Corpus audio is intentionally not committed. Populate the layout in
`hard-cases.toml` from sources whose redistribution and speaker consent have
been reviewed, record a SHA-256 for every file, and keep speakers and acoustic
sources disjoint across splits. Each case contains the degraded mixture, the
time-aligned target, an exact transcript, and provenance/mixing metadata.

Both runners must emit schema-v1 JSON containing every metric named in the
manifest for the aggregate and all ten buckets. WER components are normalized
by reference words. `input_l1` and `input_l2` compare processed output with the
latency-aligned input and are hard gates for the clean bucket. Performance
measurements use the warmed release callback on the same reference host.

Compare reports with:

```bash
python3 .github/scripts/compare_enhancement.py baseline.json candidate.json \
  --output qualification.json
```

The comparator requires meaningful gains in both overlapping speech and
reverberation, excludes SI-SDR from the promotion score, rejects clean-speech
regressions, and enforces zero allocations/clipping plus the CPU, p99, and
latency budgets. A passing automated report is necessary but does not replace
blinded listening for naturalness, consonant integrity, pumping, and target
speaker stability.
