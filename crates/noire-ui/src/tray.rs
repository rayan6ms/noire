//! Freedesktop `StatusNotifierItem` bridge for the GPUI application.

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::Duration,
};

use ksni::blocking::{Handle, TrayMethods};

/// Commands sent from tray callbacks to the GPUI main thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrayCommand {
    Show,
    ToggleProcessing,
    Quit,
    HostOnline,
    HostOffline,
}

struct NoireTray {
    active: bool,
    busy: bool,
    commands: Sender<TrayCommand>,
    host_available: Arc<AtomicBool>,
}

impl ksni::Tray for NoireTray {
    fn id(&self) -> String {
        "io.github.rayan6ms.Noire".to_owned()
    }

    fn title(&self) -> String {
        if self.busy {
            "Noire — changing noise reduction state"
        } else if self.active {
            "Noire — noise reduction active"
        } else {
            "Noire — noise reduction off"
        }
        .to_owned()
    }

    fn icon_name(&self) -> String {
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [16, 22, 24, 32, 48, 64]
            .into_iter()
            .map(|size| tray_icon(size, self.active))
            .collect()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ignored = self.commands.send(TrayCommand::Show);
    }

    fn watcher_online(&self) {
        self.host_available.store(true, Ordering::Release);
        let _ignored = self.commands.send(TrayCommand::HostOnline);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.host_available.store(false, Ordering::Release);
        let _ignored = self.commands.send(TrayCommand::HostOffline);
        true
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
                label: if self.busy {
                    "Changing noise reduction…"
                } else if self.active {
                    "Stop noise reduction"
                } else {
                    "Start noise reduction"
                }
                .to_owned(),
                enabled: !self.busy,
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
    const SAMPLES: u16 = 4;
    let dimension = u16::try_from(requested_size).unwrap_or(32);
    let pixel_count = usize::from(dimension).pow(2);
    let scale = f32::from(dimension);
    let mut data = Vec::with_capacity(pixel_count * 4);
    let sample_count = u32::from(SAMPLES).pow(2);
    for y in 0..dimension {
        for x in 0..dimension {
            let mut neutral_samples = 0_u32;
            let mut state_samples = 0_u32;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let dx =
                        (f32::from(x) + (f32::from(sx) + 0.5) / f32::from(SAMPLES)) / scale - 0.5;
                    let dy =
                        (f32::from(y) + (f32::from(sy) + 0.5) / f32::from(SAMPLES)) / scale - 0.5;
                    let (neutral, state) = tray_icon_sample(dx, dy, active);
                    neutral_samples += u32::from(neutral);
                    state_samples += u32::from(state);
                }
            }
            let visible_samples = neutral_samples.max(state_samples);
            let alpha = u8::try_from(visible_samples * 255 / sample_count).unwrap_or(255);
            let (red, green, blue) = if state_samples > 0 {
                if active {
                    (82, 199, 149)
                } else {
                    (240, 111, 121)
                }
            } else {
                (224, 226, 230)
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

fn tray_icon_sample(x: f32, y: f32, active: bool) -> (bool, bool) {
    let microphone = point_segment_distance(x, y, 0.0, -0.17, 0.0, 0.04) <= 0.115;
    let arch_radius = x.mul_add(x, (y - 0.035) * (y - 0.035)).sqrt();
    let arch = y >= 0.025 && (arch_radius - 0.245).abs() <= 0.032;
    let arch_sides = point_segment_distance(x, y, -0.245, -0.015, -0.245, 0.06) <= 0.032
        || point_segment_distance(x, y, 0.245, -0.015, 0.245, 0.06) <= 0.032;
    let stem = point_segment_distance(x, y, 0.0, 0.275, 0.0, 0.385) <= 0.032;
    let base = point_segment_distance(x, y, -0.17, 0.405, 0.17, 0.405) <= 0.032;

    let state = if active {
        let inner = (x.mul_add(x, (y + 0.13) * (y + 0.13)).sqrt() - 0.205).abs() <= 0.028;
        let outer = (x.mul_add(x, (y + 0.13) * (y + 0.13)).sqrt() - 0.305).abs() <= 0.028;
        y < -0.17 && (inner || outer)
    } else {
        let left = point_segment_distance(x, y, -0.34, -0.28, -0.25, -0.22) <= 0.025
            || point_segment_distance(x, y, -0.25, -0.22, -0.33, -0.14) <= 0.025
            || point_segment_distance(x, y, -0.33, -0.14, -0.22, -0.08) <= 0.025;
        let right = point_segment_distance(x, y, 0.34, -0.28, 0.25, -0.22) <= 0.025
            || point_segment_distance(x, y, 0.25, -0.22, 0.33, -0.14) <= 0.025
            || point_segment_distance(x, y, 0.33, -0.14, 0.22, -0.08) <= 0.025;
        left || right
    };

    (microphone || arch || arch_sides || stem || base, state)
}

#[allow(clippy::too_many_arguments)]
fn point_segment_distance(
    x: f32,
    y: f32,
    start_x: f32,
    start_y: f32,
    end_x: f32,
    end_y: f32,
) -> f32 {
    let segment_x = end_x - start_x;
    let segment_y = end_y - start_y;
    let length_squared = segment_x.mul_add(segment_x, segment_y * segment_y);
    let projection = ((x - start_x).mul_add(segment_x, (y - start_y) * segment_y) / length_squared)
        .clamp(0.0, 1.0);
    let closest_x = segment_x.mul_add(projection, start_x);
    let closest_y = segment_y.mul_add(projection, start_y);
    (x - closest_x)
        .mul_add(x - closest_x, (y - closest_y) * (y - closest_y))
        .sqrt()
}

/// Owns the tray service and its receiving side for the application's lifetime.
#[derive(Clone)]
pub(crate) struct TrayRuntime {
    inner: Arc<TrayInner>,
}

struct TrayInner {
    commands: Mutex<Receiver<TrayCommand>>,
    handle: Option<Handle<NoireTray>>,
    _activation: Option<ActivationListener>,
    host_available: Arc<AtomicBool>,
    active: AtomicBool,
    busy: AtomicBool,
}

struct ActivationListener {
    path: PathBuf,
    stop: Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ActivationListener {
    fn start(commands: Sender<TrayCommand>) -> Option<Self> {
        let path = activation_path()?;
        let (stop, stop_receiver) = mpsc::channel();
        let listener_path = path.clone();
        let thread = thread::Builder::new()
            .name("noire-activation".to_owned())
            .spawn(move || {
                while matches!(
                    stop_receiver.recv_timeout(Duration::from_millis(50)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let is_small_file = fs::symlink_metadata(&listener_path)
                        .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= 64);
                    if !is_small_file {
                        continue;
                    }
                    let request = fs::read_to_string(&listener_path);
                    let _ignored = fs::remove_file(&listener_path);
                    if request.is_ok_and(|request| request.trim() == "show")
                        && commands.send(TrayCommand::Show).is_err()
                    {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self {
            path,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ActivationListener {
    fn drop(&mut self) {
        let _ignored = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ignored = thread.join();
        }
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| metadata.is_file()) {
            let _ignored = fs::remove_file(&self.path);
        }
    }
}

fn activation_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)?;
    Some(runtime.join("noire-controller.activate"))
}

impl TrayRuntime {
    /// Starts the system tray. The application remains usable if the desktop has no tray host.
    pub fn start() -> Self {
        let (sender, commands) = mpsc::channel();
        let activation = ActivationListener::start(sender.clone());
        let host_available = Arc::new(AtomicBool::new(true));
        let tray = NoireTray {
            active: false,
            busy: false,
            commands: sender,
            host_available: Arc::clone(&host_available),
        };
        let sandboxed = std::env::var_os("FLATPAK_ID").is_some();
        let handle = match tray.disable_dbus_name(sandboxed).spawn() {
            Ok(handle) => Some(handle),
            Err(error) => {
                eprintln!("Noire could not register its system tray item: {error}");
                host_available.store(false, Ordering::Release);
                None
            }
        };
        Self {
            inner: Arc::new(TrayInner {
                commands: Mutex::new(commands),
                handle,
                _activation: activation,
                host_available,
                active: AtomicBool::new(false),
                busy: AtomicBool::new(false),
            }),
        }
    }

    /// Whether a `StatusNotifierItem` host accepted the tray service.
    #[must_use]
    pub fn available(&self) -> bool {
        self.inner
            .handle
            .as_ref()
            .is_some_and(|handle| !handle.is_closed())
            && self.inner.host_available.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.inner.active.load(Ordering::Relaxed)
    }

    pub fn try_recv(&self) -> Result<TrayCommand, TryRecvError> {
        self.inner
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_recv()
    }

    /// Keeps the tray label/action synchronized with authoritative daemon state.
    pub fn set_active(&self, active: bool) {
        self.inner.active.store(active, Ordering::Relaxed);
        if let Some(handle) = &self.inner.handle {
            let _ignored = handle.update(|tray| {
                tray.active = active;
            });
        }
    }

    /// Prevents repeated tray mutations while one authoritative change is pending.
    pub fn set_busy(&self, busy: bool) {
        self.inner.busy.store(busy, Ordering::Release);
        if let Some(handle) = &self.inner.handle {
            let _ignored = handle.update(|tray| {
                tray.busy = busy;
            });
        }
    }
}

impl Drop for TrayInner {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::Ordering, mpsc};

    use ksni::Tray as _;

    use super::{NoireTray, TrayCommand, tray_icon};

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

    #[test]
    fn embedded_tray_icon_edges_are_antialiased() {
        assert!(
            tray_icon(22, false)
                .data
                .chunks_exact(4)
                .any(|pixel| (1..255).contains(&pixel[0]))
        );
    }

    #[test]
    fn tray_host_transitions_are_observable_without_stopping_reregistration() {
        let (commands, received) = mpsc::channel();
        let host_available = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let tray = NoireTray {
            active: false,
            busy: false,
            commands,
            host_available: Arc::clone(&host_available),
        };

        assert!(tray.watcher_offline(ksni::OfflineReason::No));
        assert!(!host_available.load(Ordering::Acquire));
        assert_eq!(received.try_recv(), Ok(TrayCommand::HostOffline));

        tray.watcher_online();
        assert!(host_available.load(Ordering::Acquire));
        assert_eq!(received.try_recv(), Ok(TrayCommand::HostOnline));
    }
}
