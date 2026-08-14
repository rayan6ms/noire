# Noire

Noire is a native Linux microphone noise-reduction service. It captures a
physical microphone through PipeWire, processes it with RNNoise, and publishes
**Noire Microphone** for applications such as browsers, voice clients, and OBS.

The current stable release is [Noire 1.0.0](https://github.com/rayan6ms/noire/releases/tag/v1.0.0).
Its supported scope is PipeWire-only, x86_64, mono 48 kHz audio, a background
daemon, a command-line client, and an optional plain GTK4 interface. The release
notes document the accepted qualification limitations for 1.0.0.

## Install

Download the Debian/Ubuntu or Fedora packages and their signed checksum manifest
from [GitHub Releases](https://github.com/rayan6ms/noire/releases/latest), then
follow the [user guide](USER_GUIDE.md) to verify and install them.

## Workspace

- `noired` owns audio and daemon state.
- `noirectl` is the headless control client.
- `noire` is the optional GTK4 application.

The initial workspace can be checked with:

```bash
cargo check --workspace --all-targets --locked
```

Engineering setup and boundaries are documented in [DEVELOPMENT.md](DEVELOPMENT.md),
[ARCHITECTURE.md](ARCHITECTURE.md), and [CONTRIBUTING.md](CONTRIBUTING.md).

Operation is covered by the [user guide](USER_GUIDE.md), with separate
[troubleshooting](TROUBLESHOOTING.md) and [privacy](PRIVACY.md) notes.

Noire does not modify global PipeWire or WirePlumber configuration, upload audio,
or require a graphical session for daemon and CLI use.

Source code is licensed under [GPL-3.0-or-later](LICENSE). The original Noire
icon is licensed under [CC-BY-SA-4.0](icons/LICENSE).
