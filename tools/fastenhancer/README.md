# FastEnhancer comparison tooling

`render_onnx.py` renders directories through the official waveform-input
FastEnhancer ONNX graphs. It implements the upstream streaming-cache loop and
compensates the documented `n_fft - hop_size` startup delay. Every worker owns
one sequential, single-threaded ONNX Runtime session; the CLI rejects more than
four workers to keep ONNX comparison runs bounded on development machines.
The native renderer permits at most eight single-thread workers for long
offline qualification corpora; shipped inference remains single-threaded.

The release comparison pins FastEnhancer-B 48 kHz from upstream tag
`onnx-48khz-v1`, commit `8d2d41931b1de316f10da15583f431761bf903ad`, with
SHA-256 `70e23bba3d41e80d30ebc5eba39d9df64f0e0315f31c772022bb17576c4d96bf`.
The upstream implementation and model are distributed under the MIT license.

Cached models, rendered audio, and reports belong under
`docs/.cache/fastenhancer/` and are not release artifacts.
