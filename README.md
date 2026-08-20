# Noire

Noire is a native Linux microphone noise-reduction application. It captures a
physical microphone through PipeWire, processes speech locally with
FastEnhancer-B at 48 kHz, and publishes **Noire Microphone** for browsers, voice
clients, recorders, and streaming tools.

The application consists of a persistent per-user daemon, the `noirectl`
command-line client, and a dark Rust [GPUI](https://www.gpui.rs/) desktop
interface. Audio is never uploaded and Noire does not rewrite global PipeWire or
WirePlumber configuration.

## Current model

FastEnhancer-B 48 kHz is the production engine. Against the improved RNNoise
backup on the frozen 824-utterance evaluation set, the selected mix improved
median STOI by about `0.0048` and median SI-SDR by about `1.95 dB`, with
effectively no clean-speech damage. It also passed the 952-case stress set
without new clipping or non-finite output.

The improved RNNoise implementation remains in
`crates/noire-model-rnnoise/` for experiments and future study, but it is not
included in the production daemon dependency graph.

## Desktop application

The home screen contains only the live state, start/stop action, input meters,
and active engine. A separate settings view provides:

- physical microphone selection;
- suppression strength and latency profile;
- fail-closed or explicitly selected fail-open behavior;
- start at login through the systemd user service;
- start minimized and close to tray preferences;
- privacy-safe diagnostics.

Closing the window keeps the controller in the freedesktop system tray by
default. It does not stop microphone processing. Desktop-only preferences are
stored in `~/.config/noire/ui.toml`; audio configuration remains in
`~/.config/noire/config.toml`.

## Build and run

Noire currently targets x86_64 Linux with PipeWire and mono 48 kHz audio. Rust
1.97 or newer, a C compiler, Clang, PipeWire development headers, Vulkan, and
the normal Wayland/X11 GPUI development libraries are required.

```sh
cargo build --workspace --release --locked
./target/release/noired &
./target/release/noire
```

Useful CLI operations:

```sh
noirectl status
noirectl devices
noirectl start
noirectl set strength 0.55
noirectl set latency-profile low
noirectl set fail-mode closed
```

`0.55` is the recommended everyday strength. Keep the low-latency profile when
audio is stable; use Balanced only when a busy system produces underruns or
dropouts. Normally disable additional noise suppression in the receiving app to
avoid processing the same voice twice.

## Packaging

Packaging implementations are provided for Debian/Ubuntu, Fedora RPM, Flatpak,
and AppImage:

```sh
packaging/build-local.sh deb
packaging/build-local.sh rpm
packaging/build-local.sh flatpak
NOIRE_APPIMAGETOOL=/path/to/appimagetool packaging/build-local.sh appimage
```

Artifacts are written below `dist/`. Native packages install the daemon as a
systemd user service with D-Bus activation. The AppImage and Flatpak wrappers
start their bundled daemon when no installed daemon is already available.

No new public release is created by these scripts.

## Workspace

- `crates/noired` — daemon, state, and production FastEnhancer integration
- `crates/noire-ui` — GPUI desktop application and tray
- `crates/noirectl` — headless D-Bus control client
- `crates/noire-model-fastenhancer*` — production model adapter
- `crates/noire-model-rnnoise` — retained experimental backup
- `packaging` — DEB, RPM, Flatpak, and AppImage builders
- `tools` — model evaluation and training utilities

Run the main verification suite with:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Source code is licensed under [GPL-3.0-or-later](LICENSE). The original Noire
icon is licensed under [CC-BY-SA-4.0](icons/LICENSE).
