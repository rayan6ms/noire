# Phase 8 accessibility review

This is the human-review half of `MX-ACCESSIBILITY`. Run the automated GTK
checks first, then complete the same keyboard and screen-reader path on GNOME
Wayland and KDE Wayland. Do not mark the evidence template active until both
desktop records are complete on the tested commit.

## Record

- Commit:
- Noire version:
- Reviewer:
- Date:
- Desktop and version:
- Screen reader and version:
- Result: pass / fail
- Defect or waiver links:

## Procedure

1. Run `.github/scripts/run_phase8_ui_smoke.sh`.
2. Launch Noire with the daemon healthy. Starting at the window, use only
   `Tab`, `Shift+Tab`, arrows, `Space`, `Enter`, and `Escape` to visit and operate
   Start/Stop, Microphone, Noise suppression, Strength, Latency, Failure
   behavior, and Launch at login.
3. Repeat with an unavailable input, a retryable daemon error, and a disconnected
   daemon. Confirm Retry becomes reachable and unavailable controls are announced
   as disabled.
4. With the desktop screen reader enabled, confirm every control has the expected
   name, role, current value/state, and useful description.
5. Confirm healthy, reconnecting, degraded, and disconnected states are
   understandable with a monochrome display or color filter; the text must be
   sufficient without icons or color.

## Keyboard path

| Control | Forward path | Reverse path | Operates | Visible focus | Notes |
|---|---|---|---|---|---|
| Start/Stop | | | | | |
| Microphone | | | | | |
| Noise suppression | | | | | |
| Strength | | | | | |
| Latency | | | | | |
| Failure behavior | | | | | |
| Launch at login | | | | | |
| Retry during error | | | | | |

## Screen-reader and color-independent state

| State/control | Spoken name | Role/value/state correct | Text sufficient without color | Notes |
|---|---|---|---|---|
| Healthy status | | | | |
| Reconnecting status | | | | |
| Degraded status and recovery | | | | |
| Disconnected status and Retry | | | | |
| Microphone | | | | |
| Noise suppression | | | | |
| Strength | | | | |
| Latency | | | | |
| Failure behavior | | | | |
| Launch at login | | | | |
