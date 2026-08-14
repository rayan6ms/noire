# Noire privacy and data handling

Noire processes microphone audio locally in the current user's PipeWire session.
Noire 1.0 has no account, telemetry, update checker, cloud inference, model
download, or other network feature. It does not record audio in normal operation.

## Audio path

The per-user `noired` process captures the selected physical microphone,
processes bounded audio blocks in memory, and publishes **Noire Microphone**.
Audio is not written to configuration, logs, diagnostics, or the network.
Internal frame and ring storage is cleared on stop, input-generation reset, and
relevant failure transitions so stale samples are not replayed.

Noire does not prevent an application selected by the user from recording or
transmitting **Noire Microphone**. The privacy policy of that application still
applies.

## Failure and bypass behavior

The default `closed` failure mode fades output to silence after an unsafe model
failure. It does not silently substitute newly captured raw audio.

Two explicit settings can expose unsuppressed microphone content:

- `noirectl set enabled false` selects latency-matched dry audio during normal
  operation;
- `noirectl set fail-mode open` permits latency-matched dry audio after a model
  failure.

Neither setting is mute. Use `enabled true` and `fail-mode closed` for the default
privacy posture. Noise suppression reduces supported background noise; it is not
a guarantee that speech or other sensitive sound will be removed.

## Persisted data

Noire stores configuration at `$XDG_CONFIG_HOME/noire/config.toml`, or
`~/.config/noire/config.toml` when `XDG_CONFIG_HOME` is unset. It includes the
selected stable input identifier, processing settings, active state, and
launch-at-login choice. The packaged daemon writes the file and its
last-known-good `config.toml.bak` with mode `0600` inside a mode-`0700` service
configuration directory.

Native package removal preserves these per-user files. Package scripts do not
scan home directories. The owning user may delete them explicitly by following
the removal section of [USER_GUIDE.md](USER_GUIDE.md).

## Diagnostics and logs

`noirectl diagnostics` returns only bounded operational data: Noire/API versions,
lifecycle state, the fixed virtual-source name, the selected stable input ID,
and the last error code. It contains no audio, raw PipeWire property dump,
environment dump, or automatic upload.

The systemd user journal contains structured lifecycle and rate-limited error
events. Local system components may add usernames, paths, process metadata, or
device information around those events. Inspect and redact journal excerpts
before sharing them.

## Security boundary

Noire runs without root and does not modify global PipeWire or WirePlumber
configuration. Its D-Bus API is on the per-user session bus. That bus coordinates
applications belonging to the same user; it is not a security boundary against
other processes already running as that user.

The packaged service restricts filesystem writes to its per-user configuration
directory and limits communication to local Unix sockets. Noire does not provide
arbitrary-path diagnostic or export operations.

Report a suspected privacy or security defect through the private process in
[SECURITY.md](SECURITY.md), not a public issue.
