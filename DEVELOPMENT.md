# Developing Noire

Noire targets x86_64 Linux. The supported 1.0 build matrix is Ubuntu 24.04 LTS
or newer, Debian 13 or newer, and Fedora 43 or newer. The checked-in toolchain is
Rust 1.97.0; the minimum supported Rust version (MSRV) is 1.92.0.

## Rust setup

Install Rust with `rustup`, then install the two project toolchains:

```bash
rustup toolchain install 1.97.0 --profile minimal --component clippy,rustfmt
rustup toolchain install 1.92.0 --profile minimal
```

Commands run from the repository select 1.97.0 through
`rust-toolchain.toml`. Dependencies are pinned in `Cargo.lock`; keep `--locked`
on verification and release commands.

The default workspace is deliberately headless and does not compile GTK or the
PipeWire bindings. It does compile the session D-Bus daemon and CLI:

```bash
cargo check --workspace --all-targets --locked
cargo run -p noired -- --help
cargo run -p noirectl -- --help
```

Run the control plane on a user session bus, then query it from another shell:

```bash
cargo run -p noired
cargo run -p noirectl -- --json status
cargo run -p noirectl -- devices
cargo run -p noirectl -- set strength 0.75
```

With GTK 4.10 or newer installed, run the optional settings and status client
against the same daemon:

```bash
cargo run -p noire-ui --features gtk-ui
```

The UI does not start or embed the daemon. It remains usable when the daemon or
audio backend is unavailable, and all D-Bus work runs off the GTK main thread.
GTK-visible translated strings use GLib's `noire` gettext domain. The deterministic
template update and reviewed-catalog layout are documented in `po/README.md`.
The reproducible Ubuntu 24.04 GTK test environment is defined by
`packaging/validation/Containerfile.ui-ubuntu` for hosts without development
headers.

The installed-package boundary is covered separately by the gated, bounded
`.github/scripts/run_phase8_packaged_ui_vm.sh` harness. In a disposable Ubuntu
24.04, Debian 13, or Fedora 44 container it proves that `noire-daemon` remains
GTK-free, exercises daemon/CLI D-Bus operation, installs the optional UI, checks
GTK 4.10+, requires a clear no-display failure, runs the real window under Xvfb
for three seconds, and removes the UI without breaking headless operation.
The default AppStream image is an actual running-state window from this packaged
Ubuntu environment. Refresh it only in a disposable container with
`.github/scripts/capture_phase8_appstream_screenshot.sh`, then run
`.github/scripts/validate_appstream_screenshots.py` to prove the HTTPS URL maps
to the committed 1280×720 PNG and matches its declared dimensions.

The default daemon build remains controllable but reports an actionable error if
asked to start audio. Add `--features pipewire-backend` for the native live graph.
Configuration is owned by the daemon at `$XDG_CONFIG_HOME/noire/config.toml` or
the standard home fallback; do not edit it concurrently with D-Bus mutations.

The development-only offline adapter runner accepts mono 48 kHz signed 16-bit
PCM or 32-bit float WAV input and writes latency-compensated 32-bit float WAV:

```bash
cargo run -p noire-model-rnnoise --features offline-wav \
  --bin noire-denoise-wav -- input.wav output.wav
```

Pass `--live` before the input path to include the production live path's DC
blocker. This is the runner used for Phase-5 frozen-corpus evidence:

```bash
cargo run --release -p noire-model-rnnoise --features offline-wav \
  --bin noire-denoise-wav -- --live input.wav output.wav
```

This feature is for deterministic offline testing only. WAV I/O is not linked
into the daemon or exposed through the runtime model contract.

## Native development packages

The `pipewire-backend` feature uses `pkg-config`, PipeWire and SPA development
headers, a C compiler, and libclang for generated bindings. The `gtk-ui` feature
also requires GTK 4.10 or newer.

Ubuntu 24.04 LTS and Debian 13:

```bash
sudo apt install build-essential pkg-config libclang-dev \
  libpipewire-0.3-dev libspa-0.2-dev libgtk-4-dev
```

Fedora 43:

```bash
sudo dnf install gcc pkgconf-pkg-config clang-devel pipewire-devel gtk4-devel
```

Confirm what the build will resolve:

```bash
pkg-config --modversion libpipewire-0.3
pkg-config --modversion libspa-0.2
pkg-config --atleast-version=4.10 gtk4
```

Build feature boundaries independently before checking the complete native
workspace:

```bash
cargo check -p noire-pipewire --features pipewire-backend --locked
cargo check -p noired --features pipewire-backend --locked
cargo check -p noire-ui --features gtk-ui --locked
cargo check --workspace --all-targets --all-features --locked
```

## Native package development

Package generation is intentionally separate from installation. Build the three
native release binaries, then use the distro-family builder documented in
[`packaging/README.md`](packaging/README.md). The daemon package remains GTK-free;
the UI and an empty convenience package are separate artifacts.

Validate the canonical payload, desktop/AppStream metadata, package split, and
every package format supported by the current host without installing anything:

```bash
.github/scripts/run_phase9_package_smoke.sh
```

The Debian and RPM builders write only beneath the requested output directory.
Run install, upgrade, removal, and service lifecycle checks only in the gated
disposable environment; package scripts must never enter user home directories
or restart every logged-in user's service.

For a short package-manager lifecycle using real baseline and upgrade packages,
run this as root inside a disposable Ubuntu/Debian or Fedora environment:

```bash
NOIRE_PHASE9_DISPOSABLE_VM=1 \
  .github/scripts/run_phase9_package_manager_vm.sh deb dist/deb-1 dist/deb-2
```

Substitute `rpm` and RPM artifact directories on Fedora. This bounded check
covers headless/full install, runtime dependencies, upgrade, actual package
downgrade, remove, reinstall, and byte-preservation of an incompatible future
config schema. It deliberately does not replace the final signed-candidate login
and PipeWire graph-node qualification.

The complementary installed-service harness is
`.github/scripts/run_phase9_packaged_service_vm.sh`. Run it only in a disposable
systemd container built from `packaging/validation/Containerfile.ubuntu`,
`Containerfile.debian`, or `Containerfile.fedora`; exact Podman commands are in
`packaging/README.md`. It checks concurrent D-Bus activation, opt-in login, one
live Noire PipeWire source, zero sources after stop, future-schema
safe-default/read-only refusal, uninstall, and configuration preservation.

For the shorter packaged GTK/no-GTK boundary, use
`.github/scripts/run_phase8_packaged_ui_vm.sh` in the same disposable images;
it does not require systemd as PID 1. Exact commands are in
`packaging/README.md`.

Run the short, offline release-metadata smoke to prove clean-source enforcement,
byte-for-byte reproducibility, checksums, SPDX 2.3 content, embedded-model
identity, SLSA provenance, and tamper rejection:

```bash
.github/scripts/run_phase9_release_metadata_smoke.sh
```

The real-artifact generation and verification commands are documented in
[`packaging/README.md`](packaging/README.md). Metadata signing and publication
remain frozen-release-candidate operations, not development checks.

Before creating a candidate, run the bounded audit against the intended 1.0.0
package set. It checks the version closure, clean source state, pinned toolchain,
dependency source policy, traceability, AppStream screenshot, exact package
filenames, and that both UI packages contain the current AppStream metadata:

```bash
python3 .github/scripts/audit_release_candidate.py \
  --expected-version 1.0.0 --package-release 1 \
  --deb-dir dist/deb --rpm-dir dist/rpm \
  --source-dir dist/source --metadata-dir dist/release-metadata
```

The exact unsigned candidate contents and the distinction between freezing and
release qualification are defined in
[`packaging/release-candidate-freeze-v1.md`](packaging/release-candidate-freeze-v1.md).
`--report-only` is diagnostic and never records a freeze pass.

Missing `libpipewire-0.3.pc`, `libspa-0.2.pc`, or `gtk4.pc` errors indicate a
system development-package or `PKG_CONFIG_PATH` problem. A bindgen error that
cannot find `libclang.so` indicates a libclang installation or `LIBCLANG_PATH`
problem; do not vendor machine-specific paths into the repository.

The two local PipeWire 0.10.0 sys-crate patches are the upstream,
source-independent bindgen output-directory fix documented in
[`vendor/README.md`](vendor/README.md). They keep Ubuntu 24.04 builds compatible
while `deny.toml` continues to reject all Git dependency sources.

## PipeWire sessions

Native runtime work uses the current user's existing PipeWire session. Inspect
it without changing host configuration:

```bash
systemctl --user --no-pager status pipewire.service wireplumber.service
pw-cli info 0
pw-dump | less
```

`systemctl` is diagnostic here; development and test commands must not start,
stop, enable, or rewrite host audio services. Never edit global PipeWire or
WirePlumber configuration for Noire. Inspect captured graph data for user or
device identifiers before sharing it, and never commit a host graph dump.

Hardware, destructive-fault, packaging, and service-lifecycle tests belong in
an approved disposable VM or purpose-built runner. Unit tests must work without
a microphone, a display server, or changes to the host session.

## Standard verification

Run this sequence on the exact state being submitted:

```bash
python3 .github/scripts/validate_traceability.py --self-test
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
RUSTFLAGS="-D warnings" cargo +1.92.0 check --workspace --all-targets --locked
cargo build --workspace --release --locked
```

When behavior, scope, or release evidence changes, update
[`tests/requirements.toml`](tests/requirements.toml) and the relevant
[`tests/evidence/`](tests/evidence/) template in the same change. A template being
`planned` records its future evidence contract; only set it to `active` when its
cited automated sources exist and run.

Phase 8 error-copy automation covers every production public error code. Its
remaining human clarity and operability review is recorded in
[`tests/usability/phase8-error-review.md`](tests/usability/phase8-error-review.md);
keep `MX-ERROR-USABILITY` planned until that signed GNOME/KDE review is complete.

The corresponding keyboard, accessibility-tree, screen-reader, and
color-independence procedure is in
[`tests/usability/phase8-accessibility-review.md`](tests/usability/phase8-accessibility-review.md).
Keep `MX-ACCESSIBILITY` planned until both desktop records are signed.

The packaged opt-in user unit is
[`data/systemd/user/noire.service`](data/systemd/user/noire.service). Validate its
syntax and policy without touching the current user manager with
`.github/scripts/run_phase9_user_service.sh`. The complete enable, login,
failure-restart, stop, and disable harness is intentionally gated behind
`NOIRE_PHASE9_DISPOSABLE_VM=1` in `.github/scripts/run_phase9_service_vm.sh`.
The matching D-Bus activation record is
[`data/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service`](data/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service);
the package smoke checks that both files name the same service and bus owner.

When native packages are available, repeat check, Clippy, and test with
`--all-features`. Feature-specific integration, latency, quality, and soak tests
are additional evidence; they are not replaced by the standard sequence.

The disposable Phase-4/5 integration session additionally requires
`pipewire-pulse` and `pulseaudio-utils` (`pactl`/`parec`). It runs every ignored
native test serially and rejects PipeWire, WirePlumber, or compatibility-server
xrun diagnostics:

```bash
NOIRE_PHASE4_SOAK_SECONDS=1800 \
  dbus-run-session -- .github/scripts/run_pipewire_session.sh
```

Real Chrome, Electron, and OBS fixtures are an explicit local application smoke
matrix, not a CI dependency; enable them only in the prepared disposable image
with `NOIRE_PHASE4_APP_SMOKE=1`.

Phase-5 release allocation and reference-host performance gates are explicit
ignored tests because debug inference timing is not a release metric:

```bash
cargo test --release -p noire-pipewire --test capture_allocation \
  ten_million_live_callback_invocations_have_zero_allocator_calls \
  --locked -- --ignored --nocapture
cargo test --release -p noire-pipewire --test phase5_pipeline \
  live_rnnoise_meets_cpu_deadline_callback_and_rss_gates \
  --locked -- --ignored --nocapture
```

The native session itself also runs in release mode, starts an isolated session
bus automatically when needed, and covers the live graph alongside retained
Phase-3/4 acceptance tests. Phase-6 D-Bus and CLI contracts run independently in
a private session bus; the native runner also proves the daemon engine creates,
controls, and removes the real live graph:

```bash
dbus-run-session -- .github/scripts/run_phase6_session.sh
dbus-run-session -- .github/scripts/run_pipewire_session.sh
```

The wall-clock Phase-7 8-hour and 15-hour soaks are release-candidate gates, not
feature-development prerequisites. Run them only after the application and
packaging have reached a frozen release-candidate state; use the bounded standard,
native-session, and accelerated checks while implementation is still changing.

Phase-2 offline allocation and timing checks use the release profile:

```bash
cargo test -p noire-model-rnnoise --test allocation --features rnnoise --release --locked
cargo bench -p noire-dsp --bench dsp_stages --locked
cargo bench -p noire-model-rnnoise --bench model_frame --features rnnoise --locked
```

The model benchmark enforces the recorded reference-host p99 gate before it
runs Criterion. Benchmark build output and local Criterion reports are not
committed.

CI pins `cargo-deny` 0.20.2 and `cargo-audit` 0.22.2. With those versions
available locally, run:

```bash
cargo-deny --locked check
cargo-audit audit --deny warnings --file Cargo.lock
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for task, safety, commit, and review rules
and [ARCHITECTURE.md](ARCHITECTURE.md) for dependency and process boundaries.
