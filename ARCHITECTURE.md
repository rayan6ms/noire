# Noire architecture

This document describes the target 1.0 boundaries. The repository contains the
platform-independent domain scaffold and foundational DSP primitives;
feature-gated native adapters and most composed runtime behavior are not
implemented yet.

## System shape

Noire is a per-user service. One daemon owns the microphone pipeline and
publishes one virtual source. The UI and CLI are clients of the daemon, not
alternative implementations.

```text
physical microphone
        |
        v
 PipeWire capture -> frame/DSP/model -> bounded SPSC ring -> PipeWire source
        ^                                                       |
        |                                                       v
 bounded commands                                      Noire Microphone
        ^                                                       |
        |                                                       v
     noired state <--- session D-Bus <--- noire / noirectl   applications
```

| Process | Owns | Must not do |
| --- | --- | --- |
| `noired` | PipeWire graph, DSP/model pipeline, lifecycle, config, D-Bus service, metrics | Render UI, depend on a display server, perform network I/O |
| `noire` | Optional plain GTK4 presentation and D-Bus client | Open audio streams, load a model, write daemon config directly |
| `noirectl` | Scriptable D-Bus client and diagnostics | Bypass daemon validation or become a second service |

The daemon and CLI remain usable without GTK or a graphical session. There is
no privileged helper, cloud inference, telemetry, account, or network path in
the 1.0 design.

## Workspace ownership

| Crate | Responsibility |
| --- | --- |
| `noire-core` | Platform-independent domain types, state, and policy |
| `noire-dsp` | Allocation-free framing and signal processing |
| `noire-model` | Real-time denoising model contracts |
| `noire-model-rnnoise` | The RNNoise implementation of those contracts |
| `noire-pipewire` | Registry, stream, graph, and fixed-ring platform adapter |
| `noire-config` | Versioned schema, migration, and atomic persistence |
| `noire-ipc` | Shared D-Bus data types plus service/client adapters |
| `noired` | Daemon composition, lifecycle, and process boundary |
| `noirectl` | Headless command-line client |
| `noire-ui` | Optional GTK4 client binary named `noire` |
| `noire-test-support` | Shared fakes, fixtures, and test harnesses |

Dependencies point toward contracts and domain types. Core, DSP, and model
contracts never import GTK, D-Bus, Tokio, or PipeWire. The RNNoise pipeline sees
the model contract, not a concrete implementation. Tokio is restricted to the
daemon control plane and IPC; GTK is restricted to `noire-ui`.

The `AudioBackend` port in `noire-core` carries only ordered lifecycle commands
and compact state/fault events. It carries no samples and imposes no `Send` or
`Sync` bound, so callback audio and thread-affine objects stay inside the native
adapter. `noire-test-support` implements this port and separately scripts owned
capture buffers and source requests for deterministic tests.

## Audio and control planes

PipeWire owns two process callbacks: capture produces processed audio and the
virtual source consumes it. RNNoise runs inline in capture on exact mono,
48-kHz, 480-sample frames. PipeWire is asked to provide 48 kHz; an in-process
resampler is not part of 1.0 unless supported-system evidence and a new ADR
justify one.

`noire-dsp` defines that canonical domain and keeps its processors independent
of PipeWire, the denoising model, and the UI. The implemented boundary utilities
sanitize non-finite values and flush subnormals, prefer declared mono/front-
center channels or use a headroom-preserving equal-contribution downmix, block
DC near 20 Hz, assemble arbitrary bounded quanta into exact 480-sample frames,
smooth wet/dry strength over at least 20 ms, accumulate bounded peak/RMS meter
windows, and delay dry audio by an exact configured sample count. Processing is
allocation-free after construction; only the fixed dry-delay storage is heap-
allocated before activation.

All callback capacities are fixed before stream activation. Process callbacks
may use bounded slice operations, arithmetic, model inference with a measured
bound, SPSC operations, and atomics. They may not allocate, lock, log, perform
I/O, call control-plane APIs, wait, or run work proportional to untrusted input.
The model instance and owned buffers are created and destroyed outside callback
execution.

`noire-model` is the dependency-free boundary between that pipeline and a
concrete inference adapter. It validates immutable sample-rate/channel/frame/hop,
lookahead/delay, identity/version, and SPDX license metadata. Model instances are
`Send` trait objects with synchronous descriptor, reset, and exact-frame process
operations; factories are control-plane objects whose `create` operation may
allocate. Processing failures use fieldless copyable enums, require silent
output, and require reset before reuse. Shared boundary helpers clear output
before inference, reject malformed/non-finite input, reject non-finite output,
and flush output subnormals without allocation.

Descriptor access and steady-state frame processing are allocation-free. Reset
is deterministic and synchronous but may rebuild adapter-owned state, so it runs
only after processing is deactivated and never inside a callback.

D-Bus, configuration, lifecycle, retries, and metrics snapshots run in the
non-real-time control plane. Scalar changes cross into audio through atomic
snapshots; compound changes use a fixed-capacity command queue with a fixed
per-callback drain limit. Audio sends only atomic counters and bounded state
back to control code.

## Safety and failure policy

The default policy is fail-closed. A malformed buffer, model fault, queue reset,
or lost input emits only already-processed queued audio followed by ramped
silence. Recovery must never replay stale samples or silently switch to raw
microphone audio.

Noire never changes global PipeWire or WirePlumber configuration. Its graph
objects are scoped to the user daemon and do not linger after it exits. Stable
device selectors use descriptive properties rather than persisting transient
PipeWire global IDs.

Measurable gates and detailed failure behavior belong beside the code and tests
that enforce them. A change that contradicts these public boundaries requires a
reviewed design note or ADR in the same change.
