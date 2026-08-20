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
