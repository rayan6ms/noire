# Noire

Noire is a native Linux microphone noise-reduction service. It captures a
physical microphone through PipeWire, processes it with FastEnhancer-B 48 kHz, and publishes
**Noire Microphone** for applications such as browsers, voice clients, and OBS.

The current stable release is [Noire 1.0.0](https://github.com/rayan6ms/noire/releases/tag/v1.0.0).
Its supported scope is PipeWire-only, x86_64, mono 48 kHz audio, a background
daemon, a command-line client, and an optional plain GTK4 interface. The release
notes document the accepted qualification limitations for 1.0.0.

## Install

Download the Debian/Ubuntu or Fedora packages and their signed checksum manifest
from [GitHub Releases](https://github.com/rayan6ms/noire/releases/latest), then
follow the [user guide](USER_GUIDE.md) to verify and install them.

## Recommended quality setup

For the best general speech/noise balance, start with:

```sh
noirectl set strength 0.55
noirectl set latency-profile low
noirectl set fail-mode closed
```

`0.55` is the default and qualified everyday strength. Against the improved
RNNoise backup on 824 frozen utterances, this FastEnhancer-B mix gained about
`+0.0048` median STOI and `+1.95 dB` median SI-SDR while causing effectively
zero clean-speech damage. It also completed the 952-case stress suite without
new clipping or non-finite output. Use `1.0` only when maximum removal matters
more than naturalness; the fully wet model is substantially more aggressive.
The explicit endpoints `0.0` and `1.0` remain exact.

Keep the `low` latency profile when audio is stable. Switch to `balanced` only
if the system produces underruns, dropouts, or breakup under load; it adds
scheduling headroom without changing the denoising model. Set the physical
microphone gain high enough for clear speech while retaining headroom—peaks
below roughly -6 dBFS when measurable—and normally disable additional noise
suppression in the receiving application so the same signal is not processed
twice.

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
