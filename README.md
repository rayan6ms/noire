# Noire

Noire removes background noise from a microphone on Linux. It processes audio
locally and publishes **Noire Microphone ☾**, which can be selected in browsers,
voice chat, recording, and streaming apps.

- Native PipeWire audio with low-latency and balanced modes
- Adjustable suppression strength and live microphone level
- System, dark, and light themes
- Optional launch at login and tray controls
- No uploads and no changes to global PipeWire or WirePlumber configuration

## Install

Noire currently supports x86_64 Linux desktops running PipeWire. Download the
newest files from [GitHub Releases](https://github.com/rayan6ms/noire/releases/latest).

Choose one format:

- **Debian or Ubuntu:** download the three `.deb` files and install them together:

  ```sh
  sudo apt install ./noire-daemon_*_amd64.deb ./noire-ui_*_amd64.deb ./noire_*_amd64.deb
  ```

- **Fedora:** download the three `.rpm` files and install them together:

  ```sh
  sudo dnf install ./noire-*.rpm
  ```

- **Flatpak:**

  ```sh
  flatpak install --user ./Noire-*-x86_64.flatpak
  ```

- **AppImage:** make the file executable, then run it:

  ```sh
  chmod +x Noire-*-x86_64.AppImage
  ./Noire-*-x86_64.AppImage
  ```

  On a normal launch, Noire installs its small launcher and icon metadata in
  your user data directory so Wayland taskbars can resolve the application
  icon. It defers to an existing AppImage-manager integration and does not
  replace unrelated launchers; `--help` and `--version` remain read-only.

## Use

1. Open Noire and choose a microphone in **Settings**, or follow the system
   default.
2. Select a suppression strength and press **Start**.
3. Select **Noire Microphone ☾** as the input in the receiving app.

Disable noise suppression in the receiving app to avoid processing the voice
twice. With **Close to tray** enabled, closing the window hides it and keeps
noise reduction running. **Quit Noire** stops noise reduction before exiting.
New sessions start with noise reduction off unless **Start with noise reduction
enabled** is explicitly enabled. **Start at login** launches Noire minimized.

## Troubleshooting

- If Noire cannot start, check that PipeWire is running and that the selected
  microphone is connected, then press **Retry**.
- If the virtual microphone is missing, start Noire before opening the receiving
  app, or refresh that app's device list.
- If an AppImage cannot use FUSE, run it with
  `APPIMAGE_EXTRACT_AND_RUN=1 ./Noire-*-x86_64.AppImage`.
- For native packages, recent daemon logs are available with
  `journalctl --user-unit=noire.service --since=-15min`.

The optional `noirectl` command can inspect and control Noire from a terminal;
run `noirectl --help` for its commands.

Report problems through [GitHub Issues](https://github.com/rayan6ms/noire/issues).

Source code is licensed under [GPL-3.0-or-later](LICENSE). The original Noire
icon is licensed under [CC-BY-SA-4.0](icons/LICENSE).
