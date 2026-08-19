# Noire architecture

This document describes the target 1.0 boundaries. The repository contains the
platform-independent domain scaffold, foundational DSP/model pipeline, the
feature-gated native PipeWire capture and live-model graph, and the daemon,
configuration, session D-Bus, CLI control plane, and the feature-gated GTK
settings/status client. Native package staging and Debian/Fedora builders are
implemented; signed clean-VM release qualification remains outstanding.

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

The GTK client renders only daemon-owned snapshots. Its pure presentation model
derives control availability and plain-language healthy, degraded, reconnecting,
and disconnected states without importing GTK. A dedicated Tokio thread owns
the reusable session-bus connection and performs every D-Bus call; GTK exchanges
bounded low-rate requests and replies with that worker and never performs daemon
I/O on its main thread. After a rejected mutation, the worker rereads the full
snapshot before the UI renders again, so an optimistic control value is never
presented as committed daemon state. The daemon's 100 ms control-plane monitor
publishes state/device signals independently of the UI; the client does not poll
full snapshots. Live meters use an explicit per-D-Bus-client subscription, are
emitted no faster than 10 Hz, stop immediately on normal UI shutdown, and prune
abandoned client identities. Daemon ownership loss enters a capped 250 ms to
4 s exponential reconnect path without blocking GTK.

## Workspace ownership

| Crate | Responsibility |
| --- | --- |
| `noire-core` | Platform-independent domain types, state, and policy |
| `noire-dsp` | Allocation-free framing and signal processing |
| `noire-model` | Real-time denoising model contracts |
| `noire-model-fastenhancer` | The qualified FastEnhancer-B 48 kHz adapter |
| `noire-model-fastenhancer-sys` | Private native runtime and FFI boundary |
| `noire-model-rnnoise` | Retained RNNoise backup and experiments |
| `noire-pipewire` | Registry, stream, graph, and fixed-ring platform adapter |
| `noire-config` | Versioned schema, migration, and atomic persistence |
| `noire-ipc` | Shared D-Bus data types plus service/client adapters |
| `noired` | Daemon composition, lifecycle, and process boundary |
| `noirectl` | Headless command-line client |
| `noire-ui` | Optional GTK4 client binary named `noire` |
| `noire-test-support` | Shared fakes, fixtures, and test harnesses |

Dependencies point toward contracts and domain types. Core, DSP, and model
contracts never import GTK, D-Bus, Tokio, or PipeWire. The live pipeline sees
the model contract, not a concrete implementation. Tokio is restricted to the
daemon control plane and IPC; GTK is restricted to `noire-ui`.

The `AudioBackend` port in `noire-core` carries only ordered lifecycle commands
and compact state/fault events. It carries no samples and imposes no `Send` or
`Sync` bound, so callback audio and thread-affine objects stay inside the native
adapter. `noire-test-support` implements this port and separately scripts owned
capture buffers and source requests for deterministic tests.

## Audio and control planes

PipeWire owns two process callbacks: capture produces processed audio and the
virtual source consumes it. FastEnhancer-B runs inline in capture on exact mono,
48-kHz, 512-sample frames. PipeWire is asked to provide 48 kHz; an in-process
resampler is not part of 1.0 unless supported-system evidence and a new ADR
justify one.

`noire-dsp` defines that canonical domain and keeps its processors independent
of PipeWire, the denoising model, and the UI. The implemented boundary utilities
sanitize non-finite values and flush subnormals, prefer declared mono/front-
center channels or use a headroom-preserving equal-contribution downmix, assemble
arbitrary bounded quanta into exact
512-sample frames, smooth wet/dry strength over at least 20 ms, accumulate bounded
peak/RMS meter windows, and delay unfiltered dry audio by an exact configured
sample count. Processing is allocation-free after construction; only the fixed
dry-delay storage is heap-allocated before activation.

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

`noire-model-fastenhancer` is the sole production adapter. It embeds the
FastEnhancer-B 48 kHz weights behind a small safe Rust adapter and confines the
vendored MIT C runtime and unsafe FFI to `noire-model-fastenhancer-sys`. It
declares a 512-sample hop and history delay, emits finite normalized audio, and
provides bounded output-energy activity telemetry. The qualified default is a
fixed 55% wet mix; the fully wet endpoint remains available but is deliberately
not the default because stress evaluation found it too aggressive. Allocator
instrumentation records zero steady-state allocation calls, while the reference
host measured 2.23 ms typical inference and 2.75 ms p99 against a 4 ms gate.

`noire-model-rnnoise` retains the former adapter, quality-v1 weights, and a
separate `experimental-enhancement` factory with causal late-tail prediction.
It declares the same 480-sample delay and zero lookahead, and is neither selected
by daemon composition nor eligible for promotion without the frozen hard-case,
clean-speech, listening, allocation, clipping, CPU, percentile, and latency
gates in `tests/quality/enhancement`. The generic and future personalized paths
use confidence-weighted sample-smooth blending; enrollment and embedding work
remain outside callback execution.

`noire-pipewire` owns all thread-affine native objects. `PipewireConnection`
connects one main loop, context, core, and registry; copies core failures/runtime
version into control-plane state; and binds node/default-metadata listeners.
Registry globals become immutable owned descriptors. Candidate filtering rejects
monitor, virtual, unavailable, non-source, and Noire-owned nodes; persisted
selection uses stable device/node properties rather than transient global IDs.
Add/change/remove/default events are coalesced for 50 ms before an immutable
snapshot is published.

The capture stream requests native-endian interleaved mono `f32` at 48 kHz and
resolves the persisted stable node name to a transient global ID for each live
graph. The graph pins that ID with reconnect disabled so session policy cannot
retarget capture to the virtual source. Its process callback drains every
available mapped buffer, validates chunk flags/stride/alignment/range/quantum,
copies into fixed scratch storage, sanitizes and meters samples, and relies on
the safe buffer guard to requeue on every exit path. It does not log. Negotiated
formats, stream state, failures, and atomic counters are consumed by the control
plane. Allocator instrumentation records zero calls in warmed capture and bypass
callbacks; a disposable native session exercises the same boundary against a
deterministic 44.1 kHz source and verifies PipeWire presents canonical 48 kHz.

The bypass uses one generation-tagged 9,216-sample SPSC ring. Capture
writes only complete callback blocks; overload drops the new block, advances the
generation, and forces bounded resynchronization. The source holds deliberate
silence until it has exactly one 512-sample model-frame lead plus three current
graph quanta, then applies the shared 5 ms recovery ramp. The low profile uses a
256-frame quantum; the balanced profile uses 512 frames for extra scheduling
headroom. Unexpected shortages
advance generation and fade to silence without replaying stale samples. Queue,
fault, boundary, and high-water counters are atomic snapshots outside callbacks.

The live graph injects a preconstructed `Denoiser` trait object rather than
importing a concrete adapter into the PipeWire crate. Capture sanitizes and
assembles bounded chunks into exact 512-sample frames, preserves
an unfiltered latency-aligned dry frame and runs the model inline. A minimum-20-ms
user ramp feeds a linear
correlated-signal mix and transparent ceiling before metering and complete-frame transport. Model
creation, destruction, and reset remain deactivated control-plane work.

Strength, enable, explicit fail mode, and diagnostic timing use a cache-line-
aligned atomic epoch snapshot read only at frame boundaries. Default model
failure is fail-closed: the producer stops, already-processed ring audio drains,
and the source's bounded fault ramp reaches silence. Fail-open delayed dry audio
requires an explicit control choice. Five sampled 4-ms model deadline misses
inside ten seconds expose `DegradedPerformance`; suppression is never silently
disabled. The output-energy activity proxy, peak, RMS, model/callback timing
histograms, deadline misses,
hard-ceiling events, model errors/resets, and transport high water are fixed
atomics read without locking the callback.

Live startup does not advance the published timeline while the first processed
frame is unavailable. Once ready, the source emits exactly one negotiated
quantum of leading silence and begins draining. This retains the model's complete
frame reserve without adding another model frame. Native-session latency is
requalified for every release candidate.

`VirtualSourceStream` publishes exactly one non-lingering `Audio/Source` named
`io.github.rayan6ms.Noire.Microphone`, described as **Noire Microphone**, in
native-endian mono `f32` at 48 kHz. The source stream's running state is the
consumer-demand signal: first demand activates pinned physical capture and a
fresh generation; last-consumer loss keeps capture warm for a 500 ms debounce
while securely draining pending ring audio, then pauses capture and clears all
source-owned storage. Chrome WebRTC, Electron, OBS, native PipeWire, and
pipewire-pulse fixtures exercise selection and recording in an isolated session.

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

`noired` owns one authoritative state machine on a Tokio current-thread runtime.
Every mutation carries an expected revision, validates a complete candidate,
applies it through a fixed 16-entry daemon-to-audio queue, persists it, and only
then advances the revision. Stale clients receive `Conflict`; failed persistence
rolls the audio intent back. Strength, enable, and fail-mode mutations retain the
live graph and use its Phase-5 atomics. Input and latency changes rebuild on the
native owner thread. A failed rebuild attempts to restore the prior graph.

Schema-v1 configuration lives at `$XDG_CONFIG_HOME/noire/config.toml` (or the
standard home fallback). The daemon accepts a pure schema-v0 migration, rejects
unknown fields and invalid full paths, writes mode-0600 same-directory temporary
files, flushes and atomically renames them, synchronizes the directory, and
retains one valid backup. A malformed primary is byte-preserved while a valid
backup or safe defaults keep D-Bus usable. A newer schema is byte-preserved and
read-only; even direct store saves refuse to downgrade it.

The versioned `io.github.rayan6ms.Noire.Noire1` session service owns the path
`/io/github/rayan6ms/Noire/Noire1`. Its committed introspection XML and shared
Rust wire types cover complete snapshots, devices, lifecycle, settings, retry,
launch-at-login, diagnostics, revision properties, and state/device/error
signals. A second daemon requests the name without queueing or replacement and
cannot touch audio state. `noirectl` is only a generated D-Bus proxy; omitted
revisions are fetched immediately before mutation and explicit stale revisions
remain observable to scripts through typed D-Bus errors and versioned JSON.

Launch-at-login uses a fakeable asynchronous adapter for the per-user
`org.freedesktop.systemd1.Manager`. It calls `EnableUnitFiles` or
`DisableUnitFiles` plus `Reload`, never a subprocess. Configuration changes only
after the manager succeeds; persistence failure triggers the inverse manager
operation. Structured lifecycle logging is captured by journald under the user
service. Repeated public errors are limited per stable event name. Diagnostics
contain versions, stable IDs, state, error codes, and a journal command, but no
audio, raw device-property dump, environment dump, network path, or upload.

Audio sends only fixed atomics and bounded state back to control code. D-Bus,
filesystem, logging, and systemd calls remain outside every process callback.

## Installation boundary

Both native package families consume the same staged filesystem contract.
`noire-daemon` owns the daemon, CLI, user unit, D-Bus activation/interface files,
configuration documentation, completions, and man pages without GTK. `noire-ui`
owns the GTK binary and desktop/AppStream/icon assets and depends on the matching
daemon package. `noire` is an empty convenience package. Package operations do
not enter home directories, mutate per-user configuration, or enumerate and
restart logged-in user services.

Upgrade and rollback compatibility is enforced by the unprivileged daemon, not
root package scripts. A daemon that finds a newer unsupported configuration
schema preserves the file byte-for-byte, uses inactive safe defaults, rejects
mutations as read-only, and creates no audio graph. A compatible older revision
starts normally. This keeps package-manager rollback available without granting
install scripts authority to inspect every user's configuration. Users explicitly
stop processing before removal; uninstall preserves configuration by default.

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
