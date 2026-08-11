# Noire architecture

This document describes the target 1.0 boundaries. The repository contains the
platform-independent domain scaffold, foundational DSP/model pipeline, and the
feature-gated native PipeWire registry/capture adapter. The virtual-source stream
and most composed daemon behavior are not implemented yet.

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

`noire-model-rnnoise` provides the sole 1.0 adapter behind its explicit
`rnnoise` feature. It uses `nnnoiseless`'s embedded default weights, converts
between normalized audio and the model's signed-16 numeric scale, reports VAD as
telemetry, and declares the measured one-frame (480-sample) startup/history
delay. Factory creation warms lazy model/FFT state and returns a clean instance;
the daemon must create it on the eventual processing thread before activation.
This implementation remains subject to the Phase-2 quality, provenance,
allocation, and timing gates. Its P2-06 automated selection evidence passes the
frozen-corpus objective thresholds and the pinned C-reference comparison; the
independent QG-004 listening result remains explicitly outstanding.
After same-thread warm-up, allocator instrumentation covers the production DSP
stages and model call; P2-07 records zero steady-state allocation calls and a
reference-host model p99 below the 0.75 ms gate.

`noire-pipewire` owns all thread-affine native objects. `PipewireConnection`
connects one main loop, context, core, and registry; copies core failures/runtime
version into control-plane state; and binds node/default-metadata listeners.
Registry globals become immutable owned descriptors. Candidate filtering rejects
monitor, virtual, unavailable, non-source, and Noire-owned nodes; persisted
selection uses stable device/node properties rather than transient global IDs.
Add/change/remove/default events are coalesced for 50 ms before an immutable
snapshot is published.

The capture stream requests native-endian interleaved mono `f32` at 48 kHz and
targets the resolved stable node name. Its process callback drains every available
mapped buffer, validates chunk flags/stride/alignment/range/quantum, copies into
fixed scratch storage, sanitizes and meters samples, and relies on the safe
buffer guard to requeue on every exit path. It does not log. Negotiated formats,
stream state, failures, and atomic counters are consumed by the control plane.
Allocator instrumentation records zero calls in warmed portable callback
processing; a disposable native session exercises the same boundary against a
deterministic 44.1 kHz source and verifies PipeWire presents canonical 48 kHz.

Every selected-input lifecycle has a monotonic `InputGeneration`. A generation
advance is an atomic callback command; before the next sample is delivered, the
processor clears scratch, meter, peak telemetry, and sink-owned queued state.
This prevents samples accumulated for a removed device from entering the next
device lifecycle. Registry and native-session tests exercise 50 reidentified
add/remove cycles and require stable selectors plus an empty final registry.

Overflow/underflow policy uses a fixed 5 ms equal-power gain transition. Missing
processed input fades from only the last published scalar and then holds silence;
it never replays a stale buffer. Recovery requires an explicit fresh-generation
signal. Phase-2 tests set the transition click limit to 0.01 full scale above
the source's own adjacent-sample step (approximately -40 dBFS).

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
