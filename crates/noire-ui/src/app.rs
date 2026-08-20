//! Custom dark GPUI desktop shell for Noire.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::TryRecvError,
    },
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt, App, Application, Bounds, Context, Div, FontWeight, IntoElement,
    Render, ScrollHandle, SharedString, Styled as _, Timer, TitlebarOptions, Transformation,
    Window, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowDecorations,
    WindowOptions, div, img, percentage, prelude::*, pulsating_between, px, relative, rgb, size,
    svg,
};
use noire_ipc::DiagnosticReport;

use crate::{
    assets::Assets,
    client::{self, Request, Response, WorkerChannels},
    preferences::DesktopPreferences,
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
    canvas: u32,
    chrome: u32,
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
            canvas: 0x090b0e,
            chrome: 0x0c0f13,
            surface: 0x10141a,
            raised: 0x151a21,
            hover: 0x1b222b,
            border: 0x2b3440,
            border_soft: 0x202832,
            text: 0xe7ebf0,
            muted: 0x8d98a7,
            faint: 0x626d7b,
            accent: 0x67aaf9,
            accent_soft: 0x14263a,
            success: 0x52c795,
            danger: 0xf06f79,
            danger_soft: 0x2a161a,
        }
    }

    const fn light() -> Self {
        Self {
            canvas: 0xe9edf2,
            chrome: 0xf8fafc,
            surface: 0xffffff,
            raised: 0xf1f4f7,
            hover: 0xe8edf3,
            border: 0xcbd3dd,
            border_soft: 0xdfe5ec,
            text: 0x151a21,
            muted: 0x5f6b79,
            faint: 0x8792a0,
            accent: 0x2479d8,
            accent_soft: 0xdcecff,
            success: 0x16845d,
            danger: 0xc43d49,
            danger_soft: 0xffe8ea,
        }
    }
}

#[derive(Clone)]
struct Toast {
    cause: String,
    recovery: String,
    retryable: bool,
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

struct NoireView {
    state: UiState,
    channels: WorkerChannels,
    outstanding: u32,
    page: Page,
    preferences: DesktopPreferences,
    close_to_tray: Arc<AtomicBool>,
    tray: TrayRuntime,
    settings_scroll: ScrollHandle,
    diagnostics: Option<String>,
    toast: Option<Toast>,
    toast_expires: Option<Instant>,
    last_daemon_error: Option<String>,
}

impl NoireView {
    fn new(
        preferences: DesktopPreferences,
        close_to_tray: Arc<AtomicBool>,
        tray: TrayRuntime,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn(async move |view, cx| {
            loop {
                Timer::after(RESPONSE_INTERVAL).await;
                if view
                    .update(cx, |view, cx| {
                        view.drain_responses();
                        view.drain_tray(cx);
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

        let tray_available = tray.available();
        let mut this = Self {
            state: UiState::default(),
            channels: client::spawn(),
            outstanding: 0,
            page: Page::Home,
            preferences,
            close_to_tray,
            tray,
            settings_scroll: ScrollHandle::new(),
            diagnostics: None,
            toast: None,
            toast_expires: None,
            last_daemon_error: None,
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
        if self.preferences.dark_theme {
            Palette::dark()
        } else {
            Palette::light()
        }
    }

    fn send(&mut self, request: Request, mutation: bool) {
        match self.channels.requests.try_send(request) {
            Ok(()) => {
                self.outstanding = self.outstanding.saturating_add(1);
                if mutation {
                    self.state.set_request_pending(true);
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                let error = UserError::new(
                    "ui-busy",
                    "Noire is still applying the previous change.",
                    "Wait a moment, then try again.",
                    true,
                );
                self.show_toast(Toast::from(&error));
                self.state.reject(error, None);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                let error = communication_stopped_error();
                self.show_toast(Toast::from(&error));
                self.state.reject(error, None);
            }
        }
    }

    fn drain_responses(&mut self) {
        loop {
            match self.channels.responses.try_recv() {
                Ok(Response::State {
                    snapshot,
                    inputs,
                    refresh,
                    request_complete,
                }) => {
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
                    if refresh {
                        self.state.refresh(snapshot, inputs);
                    } else {
                        self.state.converge(snapshot, inputs);
                    }
                    self.tray.set_active(active);
                    if request_complete {
                        self.outstanding = self.outstanding.saturating_sub(1);
                    }
                }
                Ok(Response::Rejected {
                    error,
                    recovered,
                    request_complete,
                }) => {
                    self.show_toast(Toast::from(&error));
                    self.state.reject(error, recovered);
                    if request_complete {
                        self.outstanding = self.outstanding.saturating_sub(1);
                    }
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
    }

    fn drain_tray(&mut self, cx: &mut Context<Self>) {
        while let Ok(command) = self.tray.commands.try_recv() {
            match command {
                TrayCommand::Show => cx.activate(true),
                TrayCommand::ToggleProcessing => self.toggle_active(),
                TrayCommand::Quit => {
                    let _ignored = self.channels.requests.try_send(Request::Shutdown);
                    cx.quit();
                }
            }
        }
    }

    fn show_toast(&mut self, toast: Toast) {
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

    fn persist_preferences(&mut self) {
        self.close_to_tray.store(
            self.preferences.close_to_tray && self.tray.available(),
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

    fn toggle_active(&mut self) {
        let active = self
            .state
            .snapshot()
            .is_some_and(|snapshot| snapshot.active);
        self.send(Request::SetActive(!active), true);
    }

    fn close_window(&self, cx: &mut Context<Self>) {
        if self.close_to_tray.load(Ordering::Relaxed) {
            cx.hide();
        } else {
            cx.quit();
        }
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        div()
            .flex()
            .h(px(38.0))
            .w_full()
            .items_center()
            .border_b_1()
            .border_color(rgb(p.border_soft))
            .bg(rgb(p.chrome))
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
                    .px_4()
                    .text_xs()
                    .text_color(rgb(p.faint))
                    .child("NOIRE"),
            )
            .child(window_button(
                "window-minimize",
                "icons/minimize.svg",
                p,
                WindowControlArea::Min,
                |_view, _, window, _cx| window.minimize_window(),
                cx,
            ))
            .child(window_button(
                "window-close",
                "icons/close.svg",
                p,
                WindowControlArea::Close,
                |view, _, _window, cx| view.close_window(cx),
                cx,
            ))
    }

    fn fixed_header(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let settings = self.page == Page::Settings;
        let nav_icon = if settings {
            "icons/back.svg"
        } else {
            "icons/settings.svg"
        };
        let nav_label = if settings { "Back" } else { "Settings" };
        let theme_icon = if self.preferences.dark_theme {
            "icons/sun.svg"
        } else {
            "icons/moon.svg"
        };
        let theme_label = if self.preferences.dark_theme {
            "Use light theme"
        } else {
            "Use dark theme"
        };

        div()
            .flex()
            .h(px(76.0))
            .w_full()
            .items_center()
            .justify_between()
            .px_5()
            .border_b_1()
            .border_color(rgb(p.border_soft))
            .bg(rgb(p.chrome))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(42.0))
                            .rounded_lg()
                            .overflow_hidden()
                            .border_1()
                            .border_color(rgb(p.border))
                            .child(img("icons/noire.svg").size_full()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_lg().font_weight(FontWeight::BOLD).child("Noire"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(p.muted))
                                    .child("Microphone cleanup, entirely local"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon_button(
                        "theme",
                        theme_icon,
                        theme_label,
                        p,
                        cx.listener(|view, _, _, cx| {
                            view.preferences.dark_theme = !view.preferences.dark_theme;
                            view.persist_preferences();
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .id("settings-navigation")
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(38.0))
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(p.border))
                            .bg(rgb(p.surface))
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
                            .child(svg().path(nav_icon).size(px(17.0)).text_color(rgb(p.muted)))
                            .child(div().text_sm().child(nav_label)),
                    ),
            )
    }

    #[allow(clippy::too_many_lines)]
    fn home(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let presentation = self.state.presentation();
        let snapshot = self.state.snapshot();
        let active = snapshot.is_some_and(|snapshot| snapshot.active);
        let rms = snapshot.map_or(0.0, |snapshot| snapshot.metrics.rms.clamp(0.0, 1.0));
        let peak = snapshot.map_or(0.0, |snapshot| snapshot.metrics.peak.clamp(0.0, 1.0));
        let model = snapshot.map_or("FastEnhancer-B", |snapshot| snapshot.model_id.as_str());
        let input = snapshot.map_or("System default", |snapshot| {
            if snapshot.input_display_name.is_empty() {
                "System default"
            } else {
                snapshot.input_display_name.as_str()
            }
        });

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(rgb(p.canvas))
            .p_5()
            .child(
                div()
                    .w_full()
                    .max_w(px(720.0))
                    .mx_auto()
                    .flex()
                    .flex_col()
                    .gap_4()
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
                                    .gap_5()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_4()
                                            .child(status_icon(active, p))
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xl()
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .child(presentation.status),
                                                    )
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
                                    .child(signal_meter("Voice", rms, p.accent, p))
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
                                    .child(path_node("icons/microphone.svg", input, p))
                                    .child(path_arrow(p))
                                    .child(path_node("icons/waveform.svg", model, p))
                                    .child(path_arrow(p))
                                    .child(path_node("icons/shield.svg", "Noire Microphone", p)),
                            ),
                    )
                    .child(
                        div()
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
        let pending = self.state.request_pending();
        let label = if pending {
            "Applying"
        } else if active {
            "Stop"
        } else {
            "Start"
        };
        let icon = if pending {
            svg()
                .path("icons/spinner.svg")
                .size(px(18.0))
                .with_animation(
                    "processing-spinner",
                    Animation::new(Duration::from_millis(850)).repeat(),
                    |icon, delta| {
                        icon.with_transformation(Transformation::rotate(percentage(delta)))
                    },
                )
                .into_any_element()
        } else {
            svg()
                .path("icons/microphone.svg")
                .size(px(18.0))
                .into_any_element()
        };

        div()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .when(active && !pending, |container| {
                container.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded_lg()
                        .bg(rgb(p.accent))
                        .with_animation(
                            "processing-pulse",
                            Animation::new(Duration::from_millis(1500))
                                .repeat()
                                .with_easing(pulsating_between(0.04, 0.18)),
                            gpui::Styled::opacity,
                        ),
                )
            })
            .child(
                div()
                    .id("primary-action")
                    .relative()
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .h(px(46.0))
                    .min_w(px(122.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(if active { p.border } else { p.accent }))
                    .bg(rgb(if active { p.raised } else { p.accent }))
                    .text_color(rgb(if active { p.text } else { 0x0007_111d }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(!interactive, |button| button.opacity(0.5))
                    .when(interactive, |button| {
                        button
                            .hover(move |style| {
                                style.bg(rgb(if active { p.hover } else { 0x0082_bbfc }))
                            })
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.toggle_active();
                                cx.notify();
                            }))
                    })
                    .child(icon)
                    .child(label),
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
        let launch_at_login = snapshot.is_some_and(|snapshot| snapshot.launch_at_login);
        let tray_available = self.tray.available();
        let choices = self.state.input_choices();

        div()
            .w_full()
            .max_w(px(720.0))
            .mx_auto()
            .flex()
            .flex_col()
            .gap_5()
            .pb_8()
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
                    .child(toggle_row(
                        "launch-at-login",
                        "Start at login",
                        "Start the background audio service with your desktop session.",
                        launch_at_login,
                        controls_enabled,
                        p,
                        cx.listener(|view, _, _, cx| {
                            let enabled = view
                                .state
                                .snapshot()
                                .is_some_and(|snapshot| snapshot.launch_at_login);
                            view.send(Request::SetLaunchAtLogin(!enabled), true);
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
                            view.persist_preferences();
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
                            view.persist_preferences();
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
            .bg(rgb(p.canvas))
            .child(
                div()
                    .id("settings-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .scrollbar_width(px(12.0))
                    .track_scroll(&self.settings_scroll)
                    .p_5()
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
                                        view.send(Request::Retry, true);
                                        cx.notify();
                                    }))
                                    .child(svg().path("icons/retry.svg").size(px(15.0)))
                                    .child("Retry"),
                            )
                        }),
                ),
        )
    }
}

impl Render for NoireView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        div()
            .size_full()
            .p(px(1.0))
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .child(
                div()
                    .relative()
                    .size_full()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(px(13.0))
                    .border_1()
                    .border_color(rgb(p.border))
                    .bg(rgb(p.canvas))
                    .child(self.title_bar(cx))
                    .child(self.fixed_header(cx))
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
    let hidden = (start_minimized || preferences.start_minimized) && tray_available;
    let close_to_tray = Arc::new(AtomicBool::new(preferences.close_to_tray && tray_available));
    let close_flag = Arc::clone(&close_to_tray);

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(680.0), px(720.0)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Noire".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                show: !hidden,
                app_id: Some(APPLICATION_ID.to_owned()),
                window_min_size: Some(size(px(500.0), px(580.0))),
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            };
            let window_result = cx.open_window(options, move |window, cx| {
                let close_flag = Arc::clone(&close_flag);
                window.on_window_should_close(cx, move |_, cx| {
                    if close_flag.load(Ordering::Relaxed) {
                        cx.hide();
                    } else {
                        cx.quit();
                    }
                    false
                });
                cx.new(|cx| {
                    NoireView::new(preferences.clone(), Arc::clone(&close_to_tray), tray, cx)
                })
            });
            if window_result.is_err() {
                eprintln!("Noire could not create its GPUI window.");
                cx.quit();
            } else if !hidden {
                cx.activate(true);
            }
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
        .h_full()
        .w(px(44.0))
        .flex()
        .items_center()
        .justify_center()
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

fn icon_button(
    id: &'static str,
    icon: &'static str,
    _label: &'static str,
    p: Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .cursor_pointer()
        .h(px(38.0))
        .w(px(38.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .border_1()
        .border_color(rgb(p.border))
        .bg(rgb(p.surface))
        .hover(move |style| style.bg(rgb(p.hover)))
        .on_click(on_click)
        .child(svg().path(icon).size(px(18.0)).text_color(rgb(p.muted)))
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
                .text_color(rgb(if active { p.accent } else { p.muted })),
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
        .child(svg().path(icon).size(px(17.0)).text_color(rgb(p.muted)))
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
                    0x00f7_fbff
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
    if maximum <= 0.5 || viewport <= 1.0 {
        return div();
    }
    let content = viewport + maximum;
    let thumb_height = (viewport * viewport / content).clamp(36.0, viewport);
    let progress = (-f32::from(handle.offset().y) / maximum).clamp(0.0, 1.0);
    let thumb_top = progress * (viewport - thumb_height);
    div()
        .absolute()
        .right(px(4.0))
        .top_2()
        .bottom_2()
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
