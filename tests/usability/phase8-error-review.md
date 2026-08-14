# Phase 8 error-usability review

This is the human-review half of `MX-ERROR-USABILITY`. Run the automated checks
first, then review the GTK client on both GNOME Wayland and KDE Wayland. Do not
mark the evidence template active until every row has been reviewed and the
record contains the tested commit and package version.

## Record

- Commit:
- Noire version:
- Reviewer:
- Date:
- Desktop and version:
- Result: pass / fail
- Defect or waiver links:

## Procedure

1. Run `cargo test --package noired --locked every_production_public_error_code_has_catalog_copy`.
2. Run `cargo test --package noire-ui --locked every_catalog_error_has_complete_operable_ui_presentation`.
3. Run `.github/scripts/run_phase8_ui_smoke.sh`.
4. Present each catalog entry below in the GTK error card. Confirm that its code,
   cause, and recovery are understandable without logs, and that settings remain
   operable whenever the daemon is connected.
5. Repeat the disconnected-daemon and no-input scenarios. Confirm Retry remains
   keyboard-operable and the window never blocks or exits.

## Catalog review

| Error code | Cause clear | Recovery clear | UI operable | Notes |
|---|---|---|---|---|
| `conflict` | | | | |
| `invalid-argument` | | | | |
| `config-persistence` | | | | |
| `config-newer-schema` | | | | |
| `config-recovered` | | | | |
| `input-unavailable` | | | | |
| `pipewire-unavailable` | | | | |
| `audio-backend-unavailable` | | | | |
| `audio-thread-unavailable` | | | | |
| `audio-command-busy` | | | | |
| `audio-command-timeout` | | | | |
| `audio-thread-stopped` | | | | |
| `audio-stream-failed` | | | | |
| `audio-graph-unavailable` | | | | |
| `model-initialization-failed` | | | | |
| `launch-manager-unavailable` | | | | |

## Scenario review

| Scenario | Cause clear | Recovery clear | UI remains operable | Notes |
|---|---|---|---|---|
| PipeWire unavailable | | | | |
| Selected input unavailable | | | | |
| No input devices listed | | | | |
| Daemon disconnected | | | | |
| Rejected concurrent change | | | | |
| Configuration cannot be saved | | | | |
| Launch-at-login manager unavailable | | | | |
