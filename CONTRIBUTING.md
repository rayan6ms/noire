# Contributing to Noire

Noire is early-stage systems software on a privacy-sensitive, real-time audio
path. Small, testable changes are preferred over broad rewrites. Before editing,
read [ARCHITECTURE.md](ARCHITECTURE.md), [DEVELOPMENT.md](DEVELOPMENT.md), and
[SECURITY.md](SECURITY.md) when changing dependencies or unsafe-code policy.

## Choose a bounded change

- Work on one task ID or one coherent bug fix at a time.
- Open or reference an issue before changing scope, architecture, a public
  contract, or a dependency. Architecture changes require an ADR in the same
  review.
- Keep domain behavior out of PipeWire, D-Bus, and GTK adapters.
- Add tests with behavior. Reproduce a bug with a failing test when practical.
- Keep requirement mappings and evidence templates synchronized with observable
  behavior; planned evidence is not a passing claim.
- Do not mix dependency updates, generated fixtures, or threshold changes into
  an unrelated change.

## Non-negotiable boundaries

PipeWire process callbacks must have bounded work. They may not allocate, lock,
log, perform I/O, call D-Bus, spawn processes, or wait. Construct models and
buffers before stream activation, and move control updates through atomics or
fixed-capacity queues.

Noire is fail-closed: faults must not expose newly captured, unprocessed audio.
Never upload, persist, or include recordings in the repository. Tests and
package scripts must not alter global PipeWire or WirePlumber configuration or
manage unrelated user services.

Unsafe Rust is currently forbidden. If an adapter eventually cannot avoid it,
the change must first update the allowlist in [SECURITY.md](SECURITY.md), limit
the unsafe code to a named platform or model-adapter module, document every
block with a `SAFETY` invariant, and add focused tests.

Runtime input paths must return typed errors rather than using `unwrap`,
`expect`, `panic!`, unchecked indexing, or ignored results.

## Verify the submitted state

Run the standard checks in [DEVELOPMENT.md](DEVELOPMENT.md). Add the native,
integration, benchmark, or soak checks appropriate to the files changed. Report
what actually ran and call out unavailable hardware, skipped tests, and known
limits; compilation alone is not evidence for audio quality or real-time
behavior.

Dependency changes must be focused, use exact workspace versions, review the
lockfile and build scripts, and pass license, source, and fresh advisory checks.
Native dependencies also require supported-distribution build evidence and an
unsafe/FFI review.

## Commits and reviews

Make each commit one logical change. Use an imperative subject and include the
task ID when the change belongs to the plan, for example:

```text
dsp: add fixed-frame assembler (P2-01)
```

A pull request should state the outcome first, then identify the task or issue,
verification commands and results, measurements when relevant, known limits,
and any documentation or ADR changes. Do not commit credentials, recordings,
downloaded model caches, build output, proprietary test material, or
machine-specific paths.
