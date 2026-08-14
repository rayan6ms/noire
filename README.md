# Noire

Noire is a native Linux microphone noise-reduction service. It will capture a
physical microphone through PipeWire, process it with RNNoise, and publish
**Noire Microphone** for applications such as browsers, voice clients, and OBS.

The project is in early development. Its 1.0 scope is PipeWire-only, x86_64,
mono 48 kHz audio, a background daemon, a command-line client, and an optional
plain GTK4 interface.

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

Installation and operation are covered by the [user guide](USER_GUIDE.md), with
separate [troubleshooting](TROUBLESHOOTING.md) and [privacy](PRIVACY.md) notes.

Noire does not modify global PipeWire or WirePlumber configuration, upload audio,
or require a graphical session for daemon and CLI use.

Source code is licensed under [GPL-3.0-or-later](LICENSE). The original Noire
icon is licensed under [CC-BY-SA-4.0](icons/LICENSE).
