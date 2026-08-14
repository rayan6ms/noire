# Troubleshooting Noire

Start with the authoritative state and the privacy-bounded diagnostic report:

```sh
noirectl status
noirectl diagnostics
journalctl --user-unit=noire.service --since=-15min
```

The GTK interface shows the same daemon error code, cause, and recovery action.
Do not run `noired`, `noirectl`, or the user service with `sudo`.

## The daemon is unavailable

`noirectl status` normally starts the daemon through session D-Bus activation.
If it cannot connect, inspect the per-user service:

```sh
systemctl --user status noire.service
journalctl --user-unit=noire.service --since=-15min
systemctl --user restart noire.service
```

Confirm that the command runs inside a normal login session with a session D-Bus
and a user systemd manager. A headless SSH session may require distribution-
specific user-session setup; Noire does not create a system service as a
substitute.

## Noire Microphone is missing

Check that processing is active:

```sh
noirectl status
noirectl start
```

Then inspect the PipeWire graph with `wpctl status`. Noire is PipeWire-only and
does not create an ALSA loopback device. Select **Noire Microphone** inside the
target application; Noire deliberately does not rewrite application or global
session-manager settings.

If `status` reports `input-unavailable`, list inputs and follow the current
default or select an available stable ID:

```sh
noirectl devices
noirectl set input default
noirectl retry
```

## PipeWire is unavailable

For `pipewire-unavailable` or `audio-graph-unavailable`, inspect the user audio
services:

```sh
systemctl --user status pipewire.service wireplumber.service
```

Restore the user's PipeWire session, then run `noirectl retry`. Restarting
PipeWire interrupts every application using that session, so do it only when
that disruption is acceptable. Noire automatically performs bounded recovery
when PipeWire or the selected input returns.

## Audio is choppy, delayed, or sounds wrong

- Try `noirectl set latency-profile balanced` if the low-latency profile breaks
  up under system load.
- Reduce suppression with `noirectl set strength 0.5` and compare with the
  latency-matched dry path using `noirectl set enabled false`.
- Confirm the application selected **Noire Microphone**, not the physical input
  or a monitor source.
- Avoid feeding speaker output back into the microphone. Noire 1.0 performs
  noise suppression, not acoustic echo cancellation, automatic gain control,
  dereverberation, or repair of a clipped/defective microphone signal.

`enabled false` is bypass, not mute. Restore processing with
`noirectl set enabled true`.

## Configuration warnings

`config-recovered` means the primary configuration was malformed. Noire
preserves it and loads a valid `config.toml.bak` or inactive safe defaults. Stop
the daemon before inspecting the files, back up both, and correct the primary
file without discarding the preserved copy.

`config-newer-schema` means the installed daemon is older than the configuration
format. Noire keeps the file byte-for-byte, stays inactive, emits no microphone,
and refuses mutations. Install a daemon version that supports that schema. Do
not copy the schema-v1 example over the newer file.

`config-persistence` usually indicates configuration-directory permissions or
insufficient free storage. The directory belongs to the current user; do not fix
it by running Noire as root.

## Other stable error codes

| Code | Recovery |
| --- | --- |
| `conflict` | Refresh state and retry; another client committed a newer revision. |
| `invalid-argument` | Correct the rejected setting and retry. |
| `audio-command-busy` | Wait briefly and retry. |
| `audio-command-timeout` | Retry, then restart the user service if it persists. |
| `audio-thread-unavailable` | Free process resources and restart the user service. |
| `audio-thread-stopped` | Restart the user service. |
| `audio-stream-failed` | Allow recovery; restore PipeWire if it persists. |
| `audio-backend-unavailable` | Install the native packaged daemon rather than a headless development build. |
| `model-initialization-failed` | Restart Noire; reinstall the matching package if it persists. |
| `launch-manager-unavailable` | Verify the user systemd session, then retry login-start control. |

## A process or node remains during removal

Package managers do not enumerate users or stop their services. Before removal,
run as the affected user:

```sh
noirectl stop
noirectl set launch-at-login false
systemctl --user stop noire.service
```

After uninstalling, `wpctl status` should not show **Noire Microphone**. Log out
and back in if the session D-Bus activation cache still refers to a removed
service file.

## Reporting a problem

Include the distribution/version, desktop session, package version, affected
application, error code, and `noirectl diagnostics` output. Journal excerpts can
contain local usernames, paths, or device identifiers even though Noire does not
log or retain audio; review and redact them before sharing. Never attach private
recordings unless you deliberately created and approved them for the report.
