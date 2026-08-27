# Vendored dependency patches

Noire carries the two PipeWire 0.10.0 sys crates that require a build fix on
Ubuntu 24.04, plus focused GPUI 0.2.2 Linux event-loop and window-icon patches.
The
unmodified PipeWire sources are from the crates.io archives:

- `libspa-sys` 0.10.0, crates.io SHA-256
  `69ad52764fca54818486f3cf75afec844d1f1a1568c24dcee25d41b1ab007dda`;
- `pipewire-sys` 0.10.0, crates.io SHA-256
  `f2089f245b548723e60325773c27f586b7a2372c79ea941b246cd0d654706adc`.
- `gpui` 0.2.2, with the Linux tray-only and window-icon fixes described below.

Patch inventory: upstream pipewire-rs commit
`9b54509e848e53ffa971ace15d7adf4908c09358` adds
`clang_macro_fallback_build_dir(&out_path)` to `libspa-sys/build.rs` and moves
the existing `OUT_DIR` initialization before bindgen plus applies the same
setting in `pipewire-sys/build.rs`. No other upstream change is carried.
`checksums.toml` binds every vendored file name and SHA-256 digest; the candidate
freeze audit rejects an unrecorded edit or extra file.

The GPUI patch keeps the Linux event loop alive after the last platform window
is removed. This lets Noire remain responsive to tray events while
close-to-tray is active, without creating a duplicate taskbar or window entry.
It also adds explicit X11 `_NET_WM_ICON` and Wayland
`xdg-toplevel-icon-v1` pixel icons so supporting desktops can use the embedded
icon directly. AppRun supplies managed user metadata as a fallback for older
Wayland compositors. These changes are kept as local source patches because
GPUI 0.2.2 stops the event loop when its last window is removed and does not
expose Linux window-icon metadata.

Text storage adds a POSIX final newline to the upstream `pipewire-sys` 0.10.0
`README.md` and `wrapper.h`, which are the only archive files that lacked one.
This is a byte-level normalization only; the two `build.rs` files are the only
files with semantic changes.

The patch makes bindgen's generated macro-fallback C input land under Cargo's
build output rather than beside immutable registry source. It is required for
cast-style constants such as `SPA_ID_INVALID` and `PW_ID_ANY` with Ubuntu
24.04's PipeWire 1.0 headers. Remove both path patches and this directory once
crates.io publishes a reviewed PipeWire release containing that upstream fix.
