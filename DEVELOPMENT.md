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
PipeWire bindings:

```bash
cargo check --workspace --all-targets --locked
cargo run -p noired -- --help
cargo run -p noirectl -- --help
```

Most runtime behavior is still being implemented, so these binary invocations
currently verify their command interfaces rather than an audio pipeline.

The development-only offline adapter runner accepts mono 48 kHz signed 16-bit
PCM or 32-bit float WAV input and writes latency-compensated 32-bit float WAV:

```bash
cargo run -p noire-model-rnnoise --features offline-wav \
  --bin noire-denoise-wav -- input.wav output.wav
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
cargo check -p noire-ui --features gtk-ui --locked
cargo check --workspace --all-targets --all-features --locked
```

Missing `libpipewire-0.3.pc`, `libspa-0.2.pc`, or `gtk4.pc` errors indicate a
system development-package or `PKG_CONFIG_PATH` problem. A bindgen error that
cannot find `libclang.so` indicates a libclang installation or `LIBCLANG_PATH`
problem; do not vendor machine-specific paths into the repository.

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

When native packages are available, repeat check, Clippy, and test with
`--all-features`. Feature-specific integration, latency, quality, and soak tests
are additional evidence; they are not replaced by the standard sequence.

CI pins `cargo-deny` 0.20.2 and `cargo-audit` 0.22.2. With those versions
available locally, run:

```bash
cargo-deny --locked check
cargo-audit audit --deny warnings --file Cargo.lock
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for task, safety, commit, and review rules
and [ARCHITECTURE.md](ARCHITECTURE.md) for dependency and process boundaries.
