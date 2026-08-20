//! Freedesktop `StatusNotifierItem` bridge for the GPUI application.

use std::sync::mpsc::{self, Receiver, Sender};

use ksni::blocking::{Handle, TrayMethods};

/// Commands sent from tray callbacks to the GPUI main thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrayCommand {
    Show,
    ToggleProcessing,
    Quit,
}

struct NoireTray {
    active: bool,
    commands: Sender<TrayCommand>,
}

impl ksni::Tray for NoireTray {
    fn id(&self) -> String {
        "io.github.rayan6ms.Noire".to_owned()
    }

    fn title(&self) -> String {
        if self.active {
            "Noire — noise reduction active"
        } else {
            "Noire — noise reduction off"
        }
        .to_owned()
    }

    fn icon_name(&self) -> String {
        "io.github.rayan6ms.Noire".to_owned()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [22, 32, 64]
            .into_iter()
            .map(|size| tray_icon(size, self.active))
            .collect()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ignored = self.commands.send(TrayCommand::Show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;

        vec![
            StandardItem {
                label: "Open Noire".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    let _ignored = tray.commands.send(TrayCommand::Show);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.active {
                    "Stop noise reduction"
                } else {
                    "Start noise reduction"
                }
                .to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    let _ignored = tray.commands.send(TrayCommand::ToggleProcessing);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit Noire".to_owned(),
                icon_name: "application-exit".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    let _ignored = tray.commands.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn tray_icon(requested_size: i32, active: bool) -> ksni::Icon {
    let dimension = u16::try_from(requested_size).unwrap_or(32);
    let pixel_count = usize::from(dimension).pow(2);
    let scale = f32::from(dimension);
    let mut data = Vec::with_capacity(pixel_count * 4);
    let center = (scale - 1.0) / 2.0;
    let radius = scale * 0.47;
    for y in 0..dimension {
        for x in 0..dimension {
            let dx = f32::from(x) - center;
            let dy = f32::from(y) - center;
            let inside_badge = dx.mul_add(dx, dy * dy) <= radius * radius;
            let microphone = dx.abs() <= scale * 0.115 && dy >= -scale * 0.27 && dy <= scale * 0.12;
            let stem = dx.abs() <= scale * 0.035 && dy >= scale * 0.12 && dy <= scale * 0.29;
            let base = dy >= scale * 0.27 && dy <= scale * 0.33 && dx.abs() <= scale * 0.16;
            let wave = (dy.abs() <= scale * 0.035 && dx.abs() >= scale * 0.2)
                || ((dy - dx.abs() * 0.23).abs() <= scale * 0.045
                    && dx.abs() >= scale * 0.15
                    && dx.abs() <= scale * 0.34);
            let (alpha, red, green, blue) = if microphone || stem || base {
                (255, 14, 16, 20)
            } else if wave && inside_badge {
                if active {
                    (255, 103, 170, 249)
                } else {
                    (255, 32, 36, 43)
                }
            } else if inside_badge {
                (255, 135, 140, 148)
            } else {
                (0, 0, 0, 0)
            };
            data.extend_from_slice(&[alpha, red, green, blue]);
        }
    }
    ksni::Icon {
        width: i32::from(dimension),
        height: i32::from(dimension),
        data,
    }
}

/// Owns the tray service and its receiving side for the application's lifetime.
pub(crate) struct TrayRuntime {
    pub commands: Receiver<TrayCommand>,
    handle: Option<Handle<NoireTray>>,
}

impl TrayRuntime {
    /// Starts the system tray. The application remains usable if the desktop has no tray host.
    pub fn start() -> Self {
        let (sender, commands) = mpsc::channel();
        let tray = NoireTray {
            active: false,
            commands: sender,
        };
        let sandboxed = std::env::var_os("FLATPAK_ID").is_some();
        let handle = tray.disable_dbus_name(sandboxed).spawn().ok();
        Self { commands, handle }
    }

    /// Whether a `StatusNotifierItem` host accepted the tray service.
    #[must_use]
    pub fn available(&self) -> bool {
        self.handle.is_some()
    }

    /// Keeps the tray label/action synchronized with authoritative daemon state.
    pub fn set_active(&self, active: bool) {
        if let Some(handle) = &self.handle {
            let _ignored = handle.update(|tray| {
                tray.active = active;
            });
        }
    }
}

impl Drop for TrayRuntime {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tray_icon;

    #[test]
    fn embedded_tray_icon_has_valid_argb_pixels() {
        let icon = tray_icon(32, false);

        assert_eq!((icon.width, icon.height), (32, 32));
        assert_eq!(icon.data.len(), 32 * 32 * 4);
        assert!(icon.data.chunks_exact(4).any(|pixel| pixel[0] != 0));
    }

    #[test]
    fn active_tray_icon_has_a_distinct_accent() {
        assert_ne!(tray_icon(32, false).data, tray_icon(32, true).data);
    }
}
