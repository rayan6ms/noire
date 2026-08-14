# Noire user guide

Noire is a per-user PipeWire service that publishes **Noire Microphone** for
browsers, voice clients, recorders, and other applications. Audio processing is
local. The daemon and command-line client do not require GTK or a graphical
session.

Noire 1.0 targets x86_64 Ubuntu 24.04 LTS or newer, Debian 13 or newer, and
Fedora 43 or newer. It requires a working per-user PipeWire and WirePlumber
session.

## Install

Install the full desktop package from one release-candidate directory:

```sh
sudo apt install ./noire-daemon_*_amd64.deb ./noire-ui_*_amd64.deb \
  ./noire_*_amd64.deb
```

On Fedora, use:

```sh
sudo dnf install ./noire-daemon-*.x86_64.rpm ./noire-ui-*.x86_64.rpm \
  ./noire-[0-9]*.x86_64.rpm
```

For a headless installation, install only `noire-daemon`. It contains `noired`,
`noirectl`, D-Bus activation, the systemd user unit, completions, man pages, and
this documentation without pulling in GTK.

Package files from different Noire versions must not be mixed. Verify release
checksums and signatures before installing a published release.

## First use

Open **Noire** from the desktop application menu, or run:

```sh
noire
```

The interface connects to the same per-user daemon used by the CLI. Closing the
window does not stop processing. The daemon is D-Bus activated when a client
first contacts it and remains disabled at login until you explicitly opt in.

For headless use:

```sh
noirectl status
noirectl devices
noirectl set input default
noirectl start
```

`default` follows the PipeWire session default. To pin a particular microphone,
copy its `stable_id` from `noirectl devices` and quote it if the shell requires:

```sh
noirectl set input 'the-stable-id'
```

After starting Noire, select **Noire Microphone** in the application that should
receive processed audio. Noire does not change application microphone choices or
global PipeWire/WirePlumber configuration.

## Controls

These CLI commands map to the GTK controls:

```sh
noirectl set strength 0.75
noirectl set enabled false
noirectl set enabled true
noirectl set latency-profile balanced
noirectl set latency-profile low
noirectl retry
noirectl stop
```

Suppression strength is from `0.0` through `1.0`. Disabling suppression publishes
latency-matched dry microphone audio; it does not mute. The `low` latency profile
is the default. Try `balanced` when the system is scheduling-sensitive or audio
breaks up under load.

Noire fails closed by default: an unsafe model failure fades output to silence.
The following explicit opt-in instead permits delayed dry microphone audio after
such a failure:

```sh
noirectl set fail-mode open
```

Use `noirectl set fail-mode closed` to restore the safer default. See
[PRIVACY.md](PRIVACY.md) before enabling fail-open behavior.

Use stable schema-versioned output in scripts:

```sh
noirectl --json status
noirectl --json devices
noirectl --json diagnostics
```

Successful mutations print the resulting authoritative daemon snapshot. Exit
status `0` means success; rejected or unavailable requests return `2`. Scripts
that coordinate several clients may pass `--revision NUMBER` to a mutation and
handle a `conflict` rather than overwriting newer state.

## Launch at login

Opt in or out through the daemon so the systemd state and Noire configuration
change transactionally:

```sh
noirectl set launch-at-login true
noirectl set launch-at-login false
```

At subsequent logins, systemd starts the service, while processing follows the
persisted `active` state. This does not create a system-wide service and never
requires root at runtime.

## Configuration and diagnostics

The daemon owns `$XDG_CONFIG_HOME/noire/config.toml`, falling back to
`~/.config/noire/config.toml`, and writes it with mode `0600`. The previous valid
configuration is retained as `config.toml.bak`. Do not edit either file while
the daemon is running. A format example is installed at
`/usr/share/doc/noire-daemon/config-v1.toml`.

Generate a bounded diagnostic report with:

```sh
noirectl diagnostics
journalctl --user-unit=noire.service --since=-15min
```

The first command contains versions, state, a stable selected-input identifier,
and the last error code. It contains no audio, raw device-property dump,
environment dump, or automatic upload. Inspect journal output before sharing it.

## Stop, uninstall, and rollback

Stop processing and disable login startup before uninstalling:

```sh
noirectl stop
noirectl set launch-at-login false
```

Then remove the native packages with the distribution package manager. Package
operations intentionally preserve per-user configuration and do not inspect
home directories or stop services belonging to logged-in users.

Rollback to an older package revision is supported when that daemon understands
the existing configuration schema. If the file was written by a newer,
incompatible daemon, the older daemon preserves it byte-for-byte, stays on
inactive safe defaults, publishes no microphone, and rejects configuration
changes with `config-newer-schema`. Reinstall a daemon that supports the schema;
do not replace the preserved file with defaults.

Only the user who owns the configuration should remove it explicitly:

```sh
rm -i -- "$HOME/.config/noire/config.toml" \
  "$HOME/.config/noire/config.toml.bak"
```

If `XDG_CONFIG_HOME` is set, use its `noire` directory instead. This removal is
irreversible unless the files were backed up.

For failures and recovery steps, see [TROUBLESHOOTING.md](TROUBLESHOOTING.md).
