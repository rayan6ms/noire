//! Custom adaptive GPUI desktop shell for Noire.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::TryRecvError,
    },
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, App, Application, Bounds, Context, Div, Entity, FontWeight, IntoElement,
    Render, ScrollHandle, SharedString, Styled as _, Subscription, Timer, TitlebarOptions, Window,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea,
    WindowDecorations, WindowOptions, div, img, prelude::*, px, relative, rgb, size, svg,
};
use noire_ipc::DiagnosticReport;

use crate::{
    assets::Assets,
    autostart,
    client::{self, Request, Response, WorkerChannels},
    preferences::{DesktopPreferences, ThemePreference},
    state::{UiState, UserError},
    tray::{TrayCommand, TrayRuntime},
};

const APPLICATION_ID: &str = "io.github.rayan6ms.Noire";
const RESPONSE_INTERVAL: Duration = Duration::from_millis(33);
const TOAST_LIFETIME: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Home,
    Settings,
}

#[derive(Clone, Copy)]
struct Palette {
    surface: u32,
    raised: u32,
    hover: u32,
    border: u32,
    border_soft: u32,
    text: u32,
    muted: u32,
    faint: u32,
    accent: u32,
    accent_soft: u32,
    success: u32,
    danger: u32,
    danger_soft: u32,
}

#[allow(clippy::unreadable_literal)]
impl Palette {
    const fn dark() -> Self {
        Self {
            surface: 0x111111,
            raised: 0x131313,
            hover: 0x171717,
            border: 0x1e1e1e,
            border_soft: 0x1e1e1e,
            text: 0xe8e8e8,
            muted: 0xa0a0a0,
            faint: 0x707070,
            accent: 0x969696,
            accent_soft: 0x1a1a1a,
            success: 0x52c795,
            danger: 0xf06f79,
            danger_soft: 0x2a161a,
        }
    }

    const fn light() -> Self {
        Self {
            surface: 0xf7f7f5,
            raised: 0xffffff,
            hover: 0xeeeeeb,
            border: 0xd4d4d0,
            border_soft: 0xe3e3df,
            text: 0x202020,
            muted: 0x60605c,
            faint: 0x85857f,
            accent: 0x343434,
            accent_soft: 0xe9e9e5,
            success: 0x17845c,
            danger: 0xb72f3d,
            danger_soft: 0xfbe8ea,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Toast {
    cause: String,
    recovery: String,
    retryable: bool,
}

#[derive(Clone, Copy)]
struct ProcessingTransition {
    from: bool,
    to: bool,
    started: Instant,
}

impl From<&UserError> for Toast {
    fn from(error: &UserError) -> Self {
        Self {
            cause: error.cause.clone(),
            recovery: error.recovery.clone(),
            retryable: error.retryable,
        }
    }
}

struct AppRuntime {
    tray: TrayRuntime,
    tray_controller: client::TrayController,
    window: Option<AnyWindowHandle>,
    close_to_tray: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
}

impl AppRuntime {
    fn new(
        tray: TrayRuntime,
        tray_controller: client::TrayController,
        close_to_tray: Arc<AtomicBool>,
        quit_requested: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tray,
            tray_controller,
            window: None,
            close_to_tray,
            quit_requested,
        }
    }

    fn tray_available(&self) -> bool {
        self.tray.available()
    }

    fn set_active(&mut self, active: bool) {
        self.tray.set_active(active);
    }

    fn drain_commands(&mut self, cx: &mut Context<Self>) {
        while let Ok(command) = self.tray.try_recv() {
            match command {
                TrayCommand::Show => {
                    self.show_window(cx);
                }
                TrayCommand::ToggleProcessing => {
                    self.tray_controller.toggle();
                }
                TrayCommand::Quit => {
                    self.quit_requested.store(true, Ordering::Relaxed);
                    cx.quit();
                }
            }
        }
    }

    fn show_window(&mut self, cx: &mut Context<Self>) -> Option<AnyWindowHandle> {
        if let Some(handle) = self.window
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return Some(handle);
        }

        let preferences = DesktopPreferences::load();
        self.close_to_tray.store(
            preferences.close_to_tray && self.tray.available(),
            Ordering::Relaxed,
        );
        let close_flag = Arc::clone(&self.close_to_tray);
        let runtime = cx.entity();
        let runtime_for_close = runtime.clone();
        let tray_available = self.tray.available();
        let initial_active = self.tray.active();
        let tray_controller = self.tray_controller.clone();
        let bounds = Bounds::centered(None, size(px(680.0), px(512.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Noire".into()),
                appears_transparent: true,
                ..Default::default()
            }),
            show: true,
            app_id: Some(APPLICATION_ID.to_owned()),
            window_min_size: Some(size(px(500.0), px(480.0))),
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };
        match cx.open_window(options, move |window, cx| {
            window.on_window_should_close(cx, move |window, cx| {
                if close_flag.load(Ordering::Relaxed) {
                    runtime_for_close.update(cx, |runtime, _| runtime.window = None);
                    window.remove_window();
                } else {
                    cx.quit();
                }
                false
            });
            cx.new(|cx| {
                NoireView::new(
                    preferences,
                    runtime,
                    tray_controller,
                    tray_available,
                    initial_active,
                    window,
                    cx,
                )
            })
        }) {
            Ok(handle) => {
                let handle = AnyWindowHandle::from(handle);
                self.window = Some(handle);
                cx.activate(true);
                Some(handle)
            }
            Err(error) => {
                eprintln!("Noire could not create its GPUI window: {error}");
                self.window = None;
                None
            }
        }
    }
}

struct NoireView {
    state: UiState,
    channels: WorkerChannels,
    outstanding: u32,
    page: Page,
    preferences: DesktopPreferences,
    runtime: Entity<AppRuntime>,
    tray_controller: client::TrayController,
    settings_scroll: ScrollHandle,
    diagnostics: Option<String>,
    toast: Option<Toast>,
    toast_expires: Option<Instant>,
    last_daemon_error: Option<String>,
    processing_transition: Option<ProcessingTransition>,
    optimistic_active: Option<bool>,
    start_with_noise_reduction: bool,
    processing_request_pending: bool,
    system_dark_theme: bool,
    _appearance_subscription: Subscription,
}

impl Drop for NoireView {
    fn drop(&mut self) {
        if self.processing_request_pending {
            self.tray_controller.finish_external_change();
        }
        let _ignored = self.channels.requests.try_send(Request::Shutdown);
    }
}

impl NoireView {
    fn new(
        preferences: DesktopPreferences,
        runtime: Entity<AppRuntime>,
        tray_controller: client::TrayController,
        tray_available: bool,
        initial_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let appearance_subscription = cx.observe_window_appearance(window, |view, window, cx| {
            view.system_dark_theme = appearance_is_dark(window.appearance());
            cx.notify();
        });
        cx.spawn(async move |view, cx| {
            loop {
                Timer::after(RESPONSE_INTERVAL).await;
                if view
                    .update(cx, |view, cx| {
                        view.drain_responses(cx);
                        view.expire_toast();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut this = Self {
            state: UiState::default(),
            channels: client::spawn(true),
            outstanding: 0,
            page: Page::Home,
            preferences,
            runtime,
            tray_controller,
            settings_scroll: ScrollHandle::new(),
            diagnostics: None,
            toast: None,
            toast_expires: None,
            last_daemon_error: None,
            processing_transition: None,
            optimistic_active: Some(initial_active),
            start_with_noise_reduction: false,
            processing_request_pending: false,
            system_dark_theme: appearance_is_dark(window.appearance()),
            _appearance_subscription: appearance_subscription,
        };
        if !tray_available {
            this.show_toast(Toast {
                cause: "This desktop does not expose a compatible system tray.".to_owned(),
                recovery: "Noire will remain visible and exit normally when closed.".to_owned(),
                retryable: false,
            });
        }
        this.send(Request::Refresh, false);
        this
    }

    fn palette(&self) -> Palette {
        if self.dark_theme() {
            Palette::dark()
        } else {
            Palette::light()
        }
    }

    fn dark_theme(&self) -> bool {
        self.preferences.theme.is_dark(self.system_dark_theme)
    }

    fn send(&mut self, request: Request, mutation: bool) -> bool {
        if mutation && self.state.request_pending() {
            return false;
        }
        match self.channels.requests.try_send(request) {
            Ok(()) => {
                self.outstanding = self.outstanding.saturating_add(1);
                if mutation {
                    self.state.set_request_pending(true);
                }
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                let error = UserError::new(
                    "ui-busy",
                    "Noire is still applying the previous change.",
                    "Wait a moment, then try again.",
                    true,
                );
                self.show_toast(Toast::from(&error));
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                let error = communication_stopped_error();
                self.show_toast(Toast::from(&error));
                self.state.reject(error, None);
                false
            }
        }
    }

    fn drain_responses(&mut self, cx: &mut Context<Self>) {
        loop {
            match self.channels.responses.try_recv() {
                Ok(Response::State {
                    snapshot,
                    inputs,
                    start_with_noise_reduction,
                    refresh,
                    request_complete,
                }) => {
                    let previous_active = self.state.snapshot().map(|snapshot| snapshot.active);
                    let active = snapshot.active;
                    let daemon_error = snapshot.has_error.then(|| Toast {
                        cause: snapshot.last_error.message.clone(),
                        recovery: snapshot.last_error.recovery.clone(),
                        retryable: snapshot.last_error.retryable,
                    });
                    let daemon_error_code = snapshot
                        .has_error
                        .then(|| snapshot.last_error.code.clone())
                        .filter(|code| !code.is_empty());
                    if daemon_error_code != self.last_daemon_error
                        && let Some(toast) = daemon_error
                    {
                        self.show_toast(toast);
                    }
                    self.last_daemon_error = daemon_error_code;
                    self.start_with_noise_reduction = start_with_noise_reduction;
                    if refresh {
                        self.state.refresh(snapshot, inputs);
                    } else {
                        self.state.converge(snapshot, inputs);
                    }
                    if self.optimistic_active == Some(active) {
                        self.optimistic_active = None;
                    } else if previous_active.is_some_and(|previous| previous != active) {
                        self.processing_transition = Some(ProcessingTransition {
                            from: !active,
                            to: active,
                            started: Instant::now(),
                        });
                    }
                    self.sync_runtime_active(active, cx);
                    self.complete_request(request_complete);
                }
                Ok(Response::Rejected {
                    error,
                    recovered,
                    request_complete,
                }) => {
                    let displayed_active = self.display_active();
                    let toast = Toast::from(&error);
                    if self
                        .last_daemon_error
                        .as_deref()
                        .is_none_or(|code| code != error.code)
                    {
                        self.show_toast(toast);
                    }
                    self.last_daemon_error = Some(error.code.clone());
                    self.state.reject(error, recovered);
                    let authoritative_active = self
                        .state
                        .snapshot()
                        .is_some_and(|snapshot| snapshot.active);
                    self.optimistic_active = None;
                    self.sync_runtime_active(authoritative_active, cx);
                    if displayed_active != authoritative_active {
                        self.processing_transition = Some(ProcessingTransition {
                            from: displayed_active,
                            to: authoritative_active,
                            started: Instant::now(),
                        });
                    }
                    self.complete_request(request_complete);
                }
                Ok(Response::Diagnostics(report)) => {
                    self.diagnostics = Some(diagnostic_report_text(&report));
                    self.outstanding = self.outstanding.saturating_sub(1);
                }
                Ok(Response::Meters(metrics)) => self.state.update_metrics(metrics),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.outstanding = 0;
                    let error = communication_stopped_error();
                    self.show_toast(Toast::from(&error));
                    self.state.reject(error, None);
                    break;
                }
            }
        }
        self.state.set_request_pending(self.outstanding > 0);
        if self
            .processing_transition
            .is_some_and(|transition| transition.started.elapsed() >= Duration::from_millis(160))
        {
            self.processing_transition = None;
        }
    }

    fn show_toast(&mut self, toast: Toast) {
        if self.toast.as_ref() == Some(&toast) {
            return;
        }
        self.toast = Some(toast);
        self.toast_expires = Some(Instant::now() + TOAST_LIFETIME);
    }

    fn expire_toast(&mut self) {
        if self
            .toast_expires
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.toast = None;
            self.toast_expires = None;
        }
    }

    fn persist_preferences(&mut self, cx: &mut Context<Self>) {
        let tray_available = self.runtime.read(cx).tray_available();
        self.runtime.read(cx).close_to_tray.store(
            self.preferences.close_to_tray && tray_available,
            Ordering::Relaxed,
        );
        if self.preferences.save().is_err() {
            self.show_toast(Toast {
                cause: "Noire could not save the desktop preference.".to_owned(),
                recovery: "Check the configuration directory permissions and free space."
                    .to_owned(),
                retryable: false,
            });
        }
    }

    fn display_active(&self) -> bool {
        self.optimistic_active.unwrap_or_else(|| {
            self.state
                .snapshot()
                .is_some_and(|snapshot| snapshot.active)
        })
    }

    fn sync_runtime_active(&self, active: bool, cx: &mut Context<Self>) {
        self.runtime
            .update(cx, |runtime, _| runtime.set_active(active));
    }

    fn finish_processing_request(&mut self) {
        if self.processing_request_pending {
            self.processing_request_pending = false;
            self.tray_controller.finish_external_change();
        }
    }

    fn complete_request(&mut self, request_complete: bool) {
        if request_complete {
            self.finish_processing_request();
            self.outstanding = self.outstanding.saturating_sub(1);
        }
    }

    fn begin_active_change(&mut self, target: bool) {
        if self.state.snapshot().is_none() || self.state.request_pending() {
            return;
        }
        let active = self.display_active();
        if active == target {
            return;
        }
        if !self.tray_controller.begin_external_change() {
            return;
        }
        if !self.send(Request::SetActive(target), true) {
            self.tray_controller.finish_external_change();
            return;
        }
        self.processing_request_pending = true;
        self.processing_transition = Some(ProcessingTransition {
            from: active,
            to: target,
            started: Instant::now(),
        });
    }

    fn toggle_active(&mut self) {
        self.begin_active_change(!self.display_active());
    }

    fn close_window(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.read(cx).close_to_tray.load(Ordering::Relaxed) {
            self.runtime.update(cx, |runtime, _| runtime.window = None);
            window.remove_window();
        } else {
            cx.quit();
        }
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        div()
            .relative()
            .flex()
            .h(px(46.0))
            .w_full()
            .items_center()
            .child(
                div()
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(gpui::MouseButton::Left, |_event, window, _cx| {
                        window.start_window_move();
                    })
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .pl_4()
                    .pr_3()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(p.faint))
                    .child(img("icons/noire-icon.svg").size(px(24.0)))
                    .child(
                        div()
                            .text_sm()
                            .font_family("Liberation Sans")
                            .font_weight(FontWeight::BOLD)
                            .child("NOIRE"),
                    ),
            )
            .child(window_button(
                "window-minimize",
                "icons/minimize.svg",
                p,
                WindowControlArea::Min,
                |_view, _, window, _cx| window.minimize_window(),
                cx,
            ))
            .child(
                div()
                    .absolute()
                    .left_2()
                    .right_2()
                    .bottom_0()
                    .h(px(1.0))
                    .bg(rgb(p.border_soft)),
            )
            .child(window_button(
                "window-close",
                "icons/close.svg",
                p,
                WindowControlArea::Close,
                |view, _, window, cx| view.close_window(window, cx),
                cx,
            ))
    }

    fn page_actions(&self, settings: bool, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let (nav_icon, nav_label) = if settings {
            ("icons/back.svg", "Back")
        } else {
            ("icons/settings.svg", "Settings")
        };

        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("theme")
                    .cursor_pointer()
                    .size(px(36.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(p.border))
                    .bg(rgb(p.raised))
                    .hover(move |style| style.bg(rgb(p.hover)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.preferences.theme = if view.dark_theme() {
                            ThemePreference::Light
                        } else {
                            ThemePreference::Dark
                        };
                        view.persist_preferences(cx);
                        cx.notify();
                    }))
                    .child(
                        img(if self.dark_theme() {
                            "icons/new-moon-emoji.svg"
                        } else {
                            "icons/full-moon-emoji.svg"
                        })
                        .size(px(20.0)),
                    ),
            )
            .child(
                div()
                    .id("settings-navigation")
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(36.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(p.border))
                    .bg(rgb(p.raised))
                    .px_3()
                    .hover(move |style| style.bg(rgb(p.hover)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.page = if view.page == Page::Home {
                            Page::Settings
                        } else {
                            Page::Home
                        };
                        cx.notify();
                    }))
                    .child(svg().path(nav_icon).size(px(16.0)).text_color(rgb(p.muted)))
                    .child(div().text_sm().child(nav_label)),
            )
    }

    #[allow(clippy::too_many_lines)]
    fn home(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let presentation = self.state.presentation();
        let snapshot = self.state.snapshot();
        let active = self.display_active();
        let voice = snapshot.map_or(0.0, |snapshot| meter_level(snapshot.metrics.rms));
        let peak = snapshot.map_or(0.0, |snapshot| meter_level(snapshot.metrics.peak));
        let model = snapshot.map_or("FastEnhancer-B", |snapshot| snapshot.model_id.as_str());
        let input = self.state.input_display_name();

        div().flex_1().min_h_0().w_full().px_5().pt_5().child(
            div()
                .w_full()
                .max_w(px(720.0))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .rounded_xl()
                        .border_1()
                        .border_color(rgb(if active { p.accent } else { p.border }))
                        .bg(rgb(p.surface))
                        .p_5()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_4()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_base()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child("Noire"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(p.muted))
                                                .child("Microphone cleanup, entirely local"),
                                        ),
                                )
                                .child(self.page_actions(false, cx)),
                        )
                        .child(
                            div()
                                .mt_4()
                                .pt_4()
                                .border_t_1()
                                .border_color(rgb(p.border_soft))
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_5()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .gap_4()
                                        .child(status_icon(active, p))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(processing_status(active, p))
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(rgb(p.muted))
                                                        .child(presentation.detail),
                                                ),
                                        ),
                                )
                                .child(self.processing_button(
                                    active,
                                    presentation.controls_enabled,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .mt_5()
                                .pt_5()
                                .border_t_1()
                                .border_color(rgb(p.border_soft))
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(signal_meter("Voice", voice, p.accent, p))
                                .child(signal_meter("Peak", peak, p.success, p)),
                        ),
                )
                .child(
                    div()
                        .rounded_xl()
                        .border_1()
                        .border_color(rgb(p.border_soft))
                        .bg(rgb(p.surface))
                        .p_4()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(p.faint))
                                .mb_3()
                                .child("SIGNAL PATH"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(path_node("icons/microphone.svg", &input, p))
                                .child(path_arrow(p))
                                .child(path_node("icons/waveform.svg", model, p))
                                .child(path_arrow(p))
                                .child(path_node("icons/shield.svg", "Noire Microphone ☾", p)),
                        ),
                )
                .child(
                    div()
                        .h(px(24.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_xs()
                        .text_color(rgb(p.faint))
                        .child(svg().path("icons/shield.svg").size(px(14.0)))
                        .child("48 kHz · local processing · no audio leaves this device"),
                ),
        )
    }

    fn processing_button(&self, active: bool, interactive: bool, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let pending = self.state.request_pending() || self.tray_controller.busy();
        let content = if let Some(transition) = self.processing_transition {
            let progress = (transition.started.elapsed().as_secs_f32() / 0.16).clamp(0.0, 1.0);
            div()
                .relative()
                .w(px(76.0))
                .h(px(20.0))
                .child(processing_action_content(
                    transition.from,
                    1.0 - progress,
                    p,
                ))
                .child(processing_action_content(transition.to, progress, p))
        } else {
            div()
                .relative()
                .w(px(76.0))
                .h(px(20.0))
                .child(processing_action_content(active, 1.0, p))
        };

        div().child(
            div()
                .id("primary-action")
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .h(px(40.0))
                .w(px(160.0))
                .rounded_lg()
                .border_1()
                .border_color(rgb(if active { p.success } else { p.border }))
                .bg(rgb(p.raised))
                .text_color(rgb(p.text))
                .font_weight(FontWeight::SEMIBOLD)
                .when(!interactive, |button| button.opacity(0.5))
                .when(interactive && !pending, |button| {
                    button
                        .hover(move |style| style.bg(rgb(p.hover)))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.toggle_active();
                            cx.notify();
                        }))
                })
                .child(content),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn settings_content(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let snapshot = self.state.snapshot();
        let controls_enabled = self.state.presentation().controls_enabled;
        let selected_input = snapshot.map_or("", |snapshot| snapshot.input_stable_id.as_str());
        let strength = snapshot.map_or(0.55, |snapshot| snapshot.strength);
        let latency = snapshot.map_or("low", |snapshot| snapshot.latency_profile.as_str());
        let fail_mode = snapshot.map_or("closed", |snapshot| snapshot.fail_mode.as_str());
        let suppression = snapshot.is_none_or(|snapshot| snapshot.suppression_enabled);
        let autostart = autostart::status();
        let autostart_description = if autostart.available {
            "Launch Noire in the tray when you sign in."
        } else {
            "Login startup is unavailable for this package or desktop session."
        };
        let tray_available = self.runtime.read(cx).tray_available();
        let choices = self.state.input_choices();

        div()
            .w_full()
            .max_w(px(720.0))
            .mx_auto()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(p.border))
                    .bg(rgb(p.surface))
                    .p_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Settings"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(p.muted))
                                    .child("Audio and desktop preferences"),
                            ),
                    )
                    .child(self.page_actions(true, cx)),
            )
            .child(section_title(
                "Audio",
                "The defaults are tuned for speech; change only what your microphone needs.",
                p,
            ))
            .child(
                settings_card(p)
                    .child(toggle_row(
                        "suppression-toggle",
                        "Noise suppression",
                        "Keep timing stable even when suppression is bypassed.",
                        suppression,
                        controls_enabled,
                        p,
                        cx.listener(|view, _, _, cx| {
                            let enabled = view
                                .state
                                .snapshot()
                                .is_none_or(|snapshot| snapshot.suppression_enabled);
                            view.send(Request::SetSuppressionEnabled(!enabled), true);
                            cx.notify();
                        }),
                    ))
                    .child(strength_row(strength, controls_enabled, p, cx))
                    .child(choice_row(
                        "Latency",
                        "Low minimizes delay; Balanced tolerates a busier system.",
                        [("Low", "low"), ("Balanced", "balanced")],
                        latency,
                        controls_enabled,
                        p,
                        |value| Request::SetLatencyProfile(value.to_owned()),
                        cx,
                    ))
                    .child(choice_row(
                        "Failure behavior",
                        "Closed prevents accidental raw audio when processing fails.",
                        [("Closed", "closed"), ("Open", "open")],
                        fail_mode,
                        controls_enabled,
                        p,
                        |value| Request::SetFailMode(value.to_owned()),
                        cx,
                    )),
            )
            .child(section_title(
                "Microphone",
                "Follow the session default or pin one physical input.",
                p,
            ))
            .child(
                settings_card(p).children(choices.into_iter().enumerate().map(
                    |(index, choice)| {
                        let selected = choice.stable_id == selected_input;
                        let stable_id = choice.stable_id;
                        div()
                            .id(SharedString::from(format!("input-{index}")))
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .rounded_lg()
                            .p_3()
                            .when(controls_enabled, |row| {
                                row.hover(move |style| style.bg(rgb(p.hover))).on_click(
                                    cx.listener(move |view, _, _, cx| {
                                        view.send(Request::SelectInput(stable_id.clone()), true);
                                        cx.notify();
                                    }),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        svg()
                                            .path("icons/microphone.svg")
                                            .size(px(17.0))
                                            .text_color(rgb(if selected {
                                                p.accent
                                            } else {
                                                p.muted
                                            })),
                                    )
                                    .child(div().text_sm().child(choice.label)),
                            )
                            .child(selection_mark(selected, p))
                    },
                )),
            )
            .child(section_title(
                "Startup & tray",
                "Control background behavior without cluttering the home screen.",
                p,
            ))
            .child(
                settings_card(p)
                    .child(theme_row(self.preferences.theme, p, cx))
                    .child(toggle_row(
                        "launch-at-login",
                        "Start at login",
                        autostart_description,
                        autostart.enabled,
                        autostart.available,
                        p,
                        cx.listener(|view, _, _, cx| {
                            let status = autostart::status();
                            if let Err(_error) = autostart::set_enabled(!status.enabled) {
                                view.show_toast(Toast {
                                    cause: "Noire could not update login startup.".to_owned(),
                                    recovery: "Check the desktop autostart directory permissions, then retry."
                                        .to_owned(),
                                    retryable: true,
                                });
                            }
                            cx.notify();
                        }),
                    ))
                    .child(toggle_row(
                        "start-with-noise-reduction",
                        "Start with noise reduction enabled",
                        "Apply noise reduction automatically when the background service starts.",
                        self.start_with_noise_reduction,
                        controls_enabled,
                        p,
                        cx.listener(|view, _, _, cx| {
                            view.send(
                                Request::SetStartWithNoiseReduction(
                                    !view.start_with_noise_reduction,
                                ),
                                true,
                            );
                            cx.notify();
                        }),
                    ))
                    .child(toggle_row(
                        "start-minimized",
                        "Start minimized",
                        "Open future controller sessions directly in the tray.",
                        self.preferences.start_minimized && tray_available,
                        tray_available,
                        p,
                        cx.listener(|view, _, _, cx| {
                            view.preferences.start_minimized = !view.preferences.start_minimized;
                            view.persist_preferences(cx);
                            cx.notify();
                        }),
                    ))
                    .child(toggle_row(
                        "close-to-tray",
                        "Close to tray",
                        "Keep the controller available when its window is closed.",
                        self.preferences.close_to_tray && tray_available,
                        tray_available,
                        p,
                        cx.listener(|view, _, _, cx| {
                            view.preferences.close_to_tray = !view.preferences.close_to_tray;
                            view.persist_preferences(cx);
                            cx.notify();
                        }),
                    )),
            )
            .child(section_title(
                "Support",
                "Generate a privacy-safe snapshot containing no recorded audio.",
                p,
            ))
            .child(
                settings_card(p)
                    .child(
                        div()
                            .id("diagnostics")
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_between()
                            .rounded_lg()
                            .p_3()
                            .hover(move |style| style.bg(rgb(p.hover)))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.send(Request::Diagnostics, false);
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        svg()
                                            .path("icons/shield.svg")
                                            .size(px(17.0))
                                            .text_color(rgb(p.muted)),
                                    )
                                    .child("Generate diagnostics"),
                            )
                            .child(
                                svg()
                                    .path("icons/chevron.svg")
                                    .size(px(16.0))
                                    .text_color(rgb(p.faint)),
                            ),
                    )
                    .when_some(self.diagnostics.clone(), |card, report| {
                        card.child(
                            div()
                                .rounded_lg()
                                .bg(rgb(p.raised))
                                .p_3()
                                .text_xs()
                                .text_color(rgb(p.muted))
                                .line_height(px(19.0))
                                .child(report),
                        )
                    }),
            )
    }

    fn settings_view(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                div()
                    .id("settings-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .scrollbar_width(px(12.0))
                    .track_scroll(&self.settings_scroll)
                    .pt_5()
                    .pb_5()
                    .pl_5()
                    .pr_2()
                    .child(self.settings_content(cx)),
            )
            .child(settings_scrollbar(&self.settings_scroll, p))
    }

    fn toast_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let toast = self.toast.as_ref()?;
        let p = self.palette();
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_5()
                .flex()
                .justify_center()
                .px_5()
                .child(
                    div()
                        .w_full()
                        .max_w(px(520.0))
                        .rounded_xl()
                        .border_1()
                        .border_color(rgb(p.danger))
                        .bg(rgb(p.danger_soft))
                        .shadow_lg()
                        .p_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(toast.cause.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(p.muted))
                                        .child(toast.recovery.clone()),
                                ),
                        )
                        .when(toast.retryable, |toast| {
                            toast.child(
                                div()
                                    .id("toast-retry")
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(rgb(p.border))
                                    .bg(rgb(p.surface))
                                    .px_3()
                                    .py_2()
                                    .text_sm()
                                    .hover(move |style| style.bg(rgb(p.hover)))
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.toast = None;
                                        view.toast_expires = None;
                                        view.last_daemon_error = None;
                                        view.send(Request::Retry, true);
                                        cx.notify();
                                    }))
                                    .child(
                                        svg()
                                            .path("icons/retry.svg")
                                            .size(px(15.0))
                                            .text_color(rgb(p.text)),
                                    )
                                    .child("Retry"),
                            )
                        }),
                ),
        )
    }
}

impl Render for NoireView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.system_dark_theme = appearance_is_dark(window.appearance());
        let p = self.palette();
        div()
            .relative()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .rounded(px(13.0))
            .text_color(rgb(p.text))
            .child(
                img(if self.dark_theme() {
                    "icons/window-dark.svg"
                } else {
                    "icons/window-light.svg"
                })
                .absolute()
                .inset_0()
                .size_full(),
            )
            .child(
                div()
                    .relative()
                    .size_full()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.title_bar(cx))
                    .child(match self.page {
                        Page::Home => self.home(cx),
                        Page::Settings => self.settings_view(cx),
                    })
                    .when_some(self.toast_view(cx), gpui::ParentElement::child),
            )
    }
}

/// Runs the desktop application. Command-line minimization overrides the saved preference.
pub(crate) fn run(start_minimized: bool) {
    let preferences = DesktopPreferences::load();
    let tray = TrayRuntime::start();
    let tray_available = tray.available();
    let hidden = should_start_hidden(start_minimized, preferences.start_minimized, tray_available);
    let (tray_controller, initialization) = client::TrayController::start(tray.clone());
    let _initialized = initialization.recv_timeout(Duration::from_secs(3));
    let close_to_tray = Arc::new(AtomicBool::new(preferences.close_to_tray && tray_available));
    let quit_requested = Arc::new(AtomicBool::new(false));
    let mut show_requested = !hidden;

    loop {
        if show_requested {
            run_window_session(
                tray.clone(),
                tray_controller.clone(),
                Arc::clone(&close_to_tray),
                Arc::clone(&quit_requested),
            );
            show_requested = false;
        }

        if quit_requested.load(Ordering::Relaxed)
            || !tray.available()
            || !close_to_tray.load(Ordering::Relaxed)
        {
            break;
        }

        match tray.recv() {
            Ok(TrayCommand::Show) => show_requested = true,
            Ok(TrayCommand::ToggleProcessing) => tray_controller.toggle(),
            Ok(TrayCommand::Quit) | Err(_) => break,
        }
    }

    if !tray_controller.stop_and_shutdown(Duration::from_secs(3)) {
        eprintln!("Noire could not confirm that noise reduction stopped before exit.");
    }
}

const fn should_start_hidden(
    command_line_minimized: bool,
    preference_minimized: bool,
    tray_available: bool,
) -> bool {
    (command_line_minimized || preference_minimized) && tray_available
}

fn run_window_session(
    tray: TrayRuntime,
    tray_controller: client::TrayController,
    close_to_tray: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
) {
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            let runtime =
                cx.new(|_| AppRuntime::new(tray, tray_controller, close_to_tray, quit_requested));
            let runtime_task = runtime.clone();
            cx.spawn(async move |cx| {
                loop {
                    Timer::after(RESPONSE_INTERVAL).await;
                    if runtime_task.update(cx, AppRuntime::drain_commands).is_err() {
                        break;
                    }
                }
            })
            .detach();
            runtime.update(cx, |runtime, cx| {
                runtime.show_window(cx);
            });
        });
}

fn window_button(
    id: &'static str,
    icon: &'static str,
    p: Palette,
    control: WindowControlArea,
    click: impl Fn(&mut NoireView, &gpui::ClickEvent, &mut Window, &mut Context<NoireView>) + 'static,
    cx: &mut Context<NoireView>,
) -> impl IntoElement {
    div()
        .id(id)
        .window_control_area(control)
        .cursor_pointer()
        .h(px(36.0))
        .w(px(40.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .when(control == WindowControlArea::Close, gpui::Styled::mr_3)
        .hover(move |style| {
            style.bg(rgb(if control == WindowControlArea::Close {
                p.danger_soft
            } else {
                p.hover
            }))
        })
        .on_click(cx.listener(click))
        .child(svg().path(icon).size(px(16.0)).text_color(rgb(p.muted)))
}

const fn appearance_is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

fn processing_status(active: bool, p: Palette) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_xl()
        .font_weight(FontWeight::SEMIBOLD)
        .child("Noise reduction is:")
        .child(
            div()
                .text_color(rgb(if active { p.success } else { p.danger }))
                .child(if active { "ON" } else { "OFF" }),
        )
}

fn processing_action_content(active: bool, opacity: f32, p: Palette) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .top_0()
        .bottom_0()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .opacity(opacity)
        .child(
            div().w(px(20.0)).flex().justify_center().child(
                svg()
                    .path(if active {
                        "icons/microphone-clean.svg"
                    } else {
                        "icons/microphone-noisy.svg"
                    })
                    .size(px(18.0))
                    .text_color(rgb(if active { p.success } else { p.danger })),
            ),
        )
        .child(
            div()
                .w(px(48.0))
                .text_center()
                .child(if active { "Stop" } else { "Start" }),
        )
}

fn status_icon(active: bool, p: Palette) -> Div {
    div()
        .relative()
        .size(px(48.0))
        .rounded_xl()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(if active { p.accent_soft } else { p.raised }))
        .border_1()
        .border_color(rgb(if active { p.accent } else { p.border }))
        .child(
            svg()
                .path("icons/waveform.svg")
                .size(px(23.0))
                .text_color(rgb(p.muted)),
        )
        .child(
            div()
                .absolute()
                .right(px(5.0))
                .bottom(px(5.0))
                .size(px(7.0))
                .rounded_full()
                .bg(rgb(if active { p.success } else { p.faint })),
        )
}

fn path_node(icon: &'static str, label: &str, p: Palette) -> Div {
    let icon_size = if icon == "icons/shield.svg" {
        px(18.0)
    } else {
        px(17.0)
    };
    div()
        .flex_1()
        .min_w_0()
        .h(px(54.0))
        .rounded_lg()
        .bg(rgb(p.raised))
        .border_1()
        .border_color(rgb(p.border_soft))
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .px_3()
        .child(svg().path(icon).size(icon_size).text_color(rgb(p.muted)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(rgb(p.text))
                .child(label.to_owned()),
        )
}

fn path_arrow(p: Palette) -> Div {
    div().child(
        svg()
            .path("icons/chevron.svg")
            .size(px(15.0))
            .text_color(rgb(p.faint)),
    )
}

fn settings_card(p: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .overflow_hidden()
        .rounded_xl()
        .border_1()
        .border_color(rgb(p.border_soft))
        .bg(rgb(p.surface))
        .p_2()
}

fn section_title(title: &'static str, subtitle: &'static str, p: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_sm().text_color(rgb(p.muted)).child(subtitle))
}

#[allow(clippy::too_many_arguments)]
fn toggle_row(
    id: &'static str,
    title: &'static str,
    subtitle: &'static str,
    enabled: bool,
    interactive: bool,
    p: Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .rounded_lg()
        .p_3()
        .cursor_pointer()
        .when(interactive, |row| {
            row.hover(move |style| style.bg(rgb(p.hover)))
                .on_click(on_click)
        })
        .when(!interactive, |row| row.opacity(0.45))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_xs().text_color(rgb(p.muted)).child(subtitle)),
        )
        .child(
            div()
                .w(px(40.0))
                .h(px(22.0))
                .p(px(3.0))
                .rounded_full()
                .bg(rgb(if enabled { p.accent } else { p.border }))
                .flex()
                .justify_end()
                .when(!enabled, gpui::Styled::justify_start)
                .child(div().size(px(16.0)).rounded_full().bg(rgb(if enabled {
                    p.text
                } else {
                    p.faint
                }))),
        )
}

fn strength_row(strength: f64, interactive: bool, p: Palette, cx: &mut Context<NoireView>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .rounded_lg()
        .p_3()
        .child(
            div()
                .flex()
                .justify_between()
                .child(div().font_weight(FontWeight::MEDIUM).child("Strength"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(p.muted))
                        .child(format!("{:.0}%", strength * 100.0)),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .children(
                    [0.35, 0.55, 0.75, 1.0]
                        .into_iter()
                        .enumerate()
                        .map(|(index, value)| {
                            let selected = (strength - value).abs() < 0.01;
                            div()
                                .id(SharedString::from(format!("strength-{index}")))
                                .flex_1()
                                .cursor_pointer()
                                .rounded_lg()
                                .border_1()
                                .border_color(rgb(if selected { p.accent } else { p.border }))
                                .bg(rgb(if selected { p.accent_soft } else { p.raised }))
                                .py_2()
                                .text_center()
                                .text_sm()
                                .when(interactive, |button| {
                                    button.hover(move |style| style.bg(rgb(p.hover))).on_click(
                                        cx.listener(move |view, _, _, cx| {
                                            view.send(Request::SetStrength(value), true);
                                            cx.notify();
                                        }),
                                    )
                                })
                                .child(format!("{:.0}%", value * 100.0))
                        }),
                ),
        )
}

fn theme_row(selected: ThemePreference, p: Palette, cx: &mut Context<NoireView>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .rounded_lg()
        .p_3()
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::MEDIUM).child("Theme"))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(p.muted))
                        .child("Follow the desktop or choose a fixed appearance."),
                ),
        )
        .child(
            div().flex().gap_2().children(
                [
                    ("System", ThemePreference::System),
                    ("Dark", ThemePreference::Dark),
                    ("Light", ThemePreference::Light),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (label, preference))| {
                    let is_selected = selected == preference;
                    div()
                        .id(SharedString::from(format!("theme-choice-{index}")))
                        .cursor_pointer()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(if is_selected { p.accent } else { p.border }))
                        .bg(rgb(if is_selected { p.accent_soft } else { p.raised }))
                        .px_3()
                        .py_2()
                        .text_sm()
                        .hover(move |style| style.bg(rgb(p.hover)))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.preferences.theme = preference;
                            view.persist_preferences(cx);
                            cx.notify();
                        }))
                        .child(label)
                }),
            ),
        )
}

#[allow(clippy::too_many_arguments)]
fn choice_row<const N: usize>(
    title: &'static str,
    subtitle: &'static str,
    choices: [(&'static str, &'static str); N],
    selected: &str,
    interactive: bool,
    p: Palette,
    request: impl Fn(&str) -> Request + Copy + 'static,
    cx: &mut Context<NoireView>,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .rounded_lg()
        .p_3()
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_xs().text_color(rgb(p.muted)).child(subtitle)),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .children(
                    choices
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, value))| {
                            let is_selected = selected == value;
                            div()
                                .id(SharedString::from(format!("choice-{title}-{index}")))
                                .cursor_pointer()
                                .rounded_lg()
                                .border_1()
                                .border_color(rgb(if is_selected { p.accent } else { p.border }))
                                .bg(rgb(if is_selected { p.accent_soft } else { p.raised }))
                                .px_3()
                                .py_2()
                                .text_sm()
                                .when(interactive, |button| {
                                    button.hover(move |style| style.bg(rgb(p.hover))).on_click(
                                        cx.listener(move |view, _, _, cx| {
                                            view.send(request(value), true);
                                            cx.notify();
                                        }),
                                    )
                                })
                                .child(label)
                        }),
                ),
        )
}

fn selection_mark(selected: bool, p: Palette) -> Div {
    div()
        .size(px(16.0))
        .rounded_full()
        .border_1()
        .border_color(rgb(if selected { p.accent } else { p.border }))
        .flex()
        .items_center()
        .justify_center()
        .when(selected, |mark| {
            mark.child(div().size(px(8.0)).rounded_full().bg(rgb(p.accent)))
        })
}

fn meter_level(linear: f64) -> f64 {
    if !linear.is_finite() || linear <= 0.0 {
        return 0.0;
    }
    ((20.0 * linear.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[allow(clippy::cast_possible_truncation)]
fn signal_meter(label: &'static str, value: f64, color: u32, p: Palette) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(42.0))
                .text_xs()
                .text_color(rgb(p.muted))
                .child(label),
        )
        .child(
            div()
                .h(px(6.0))
                .flex_1()
                .rounded_full()
                .overflow_hidden()
                .bg(rgb(p.raised))
                .child(
                    div()
                        .h_full()
                        .w(relative(value as f32))
                        .rounded_full()
                        .bg(rgb(color)),
                ),
        )
        .child(
            div()
                .w(px(34.0))
                .text_right()
                .text_xs()
                .text_color(rgb(p.faint))
                .child(format!("{:.0}%", value * 100.0)),
        )
}

fn settings_scrollbar(handle: &ScrollHandle, p: Palette) -> Div {
    let maximum = f32::from(handle.max_offset().height);
    let viewport = f32::from(handle.bounds().size.height);
    let offset = f32::from(handle.offset().y);
    let Some((thumb_top, thumb_height)) = scrollbar_thumb_geometry(maximum, viewport, offset)
    else {
        return div();
    };
    div()
        .absolute()
        .right(px(8.0))
        .top_3()
        .bottom_3()
        .w(px(5.0))
        .rounded_full()
        .bg(rgb(p.border_soft))
        .child(
            div()
                .absolute()
                .top(px(thumb_top))
                .h(px(thumb_height))
                .w_full()
                .rounded_full()
                .bg(rgb(p.faint)),
        )
}

fn scrollbar_thumb_geometry(maximum: f32, viewport: f32, offset: f32) -> Option<(f32, f32)> {
    const TRACK_INSET: f32 = 12.0;
    const MINIMUM_THUMB_HEIGHT: f32 = 36.0;

    let track_height = viewport - (TRACK_INSET * 2.0);
    if maximum <= 0.5 || viewport <= 1.0 || track_height <= 1.0 {
        return None;
    }

    let content = viewport + maximum;
    let minimum_thumb_height = MINIMUM_THUMB_HEIGHT.min(track_height);
    let thumb_height =
        (track_height * viewport / content).clamp(minimum_thumb_height, track_height);
    let progress = (-offset / maximum).clamp(0.0, 1.0);
    let thumb_top = progress * (track_height - thumb_height);
    Some((thumb_top, thumb_height))
}

fn communication_stopped_error() -> UserError {
    UserError::new(
        "ui-communication-stopped",
        "Daemon communication stopped unexpectedly.",
        "Restart Noire, then retry.",
        true,
    )
}

fn diagnostic_report_text(report: &DiagnosticReport) -> String {
    format!(
        "Noire {} · API {}\nState: {}\nVirtual source: {}\nSelected input: {}\nLast error: {}\n{}\nPrivacy: {}",
        report.build_version,
        report.api_version,
        report.state,
        report.source_node_name,
        if report.selected_input_id.is_empty() {
            "system default"
        } else {
            report.selected_input_id.as_str()
        },
        if report.last_error_code.is_empty() {
            "none"
        } else {
            report.last_error_code.as_str()
        },
        report.journal_hint,
        report.privacy,
    )
}

#[cfg(test)]
mod tests {
    use super::{meter_level, scrollbar_thumb_geometry, should_start_hidden};

    #[test]
    fn minimized_start_requires_a_tray_and_honors_both_entry_points() {
        assert!(should_start_hidden(true, false, true));
        assert!(should_start_hidden(false, true, true));
        assert!(!should_start_hidden(false, false, true));
        assert!(!should_start_hidden(true, true, false));
    }

    #[test]
    fn meter_level_maps_quiet_linear_audio_into_a_visible_range() {
        assert!(meter_level(0.0).abs() < f64::EPSILON);
        assert!(meter_level(f64::NAN).abs() < f64::EPSILON);
        assert!((meter_level(0.01) - (1.0 / 3.0)).abs() < 0.001);
        assert!((meter_level(0.1) - (2.0 / 3.0)).abs() < 0.001);
        assert!((meter_level(1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn settings_scrollbar_thumb_stays_inside_the_inset_track() {
        let maximum = 420.0;
        let viewport = 500.0;
        let track_height = viewport - 24.0;

        let top_geometry = scrollbar_thumb_geometry(maximum, viewport, 0.0);
        assert!(top_geometry.is_some());
        let (top, height) = top_geometry.unwrap_or_default();
        assert!(top.abs() < f32::EPSILON);
        assert!(height <= track_height);

        let bottom_geometry = scrollbar_thumb_geometry(maximum, viewport, -maximum);
        assert!(bottom_geometry.is_some());
        let (bottom_top, bottom_height) = bottom_geometry.unwrap_or_default();
        assert!((bottom_top + bottom_height - track_height).abs() < f32::EPSILON);
    }
}
