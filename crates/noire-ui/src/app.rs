//! Dark GPUI desktop shell for Noire.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::TryRecvError,
    },
    time::Duration,
};

use gpui::{
    App, Application, Bounds, Context, Div, IntoElement, Render, SharedString, Timer, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, div, prelude::*, px, relative, rgb,
    size,
};
use noire_ipc::DiagnosticReport;

use crate::{
    client::{self, Request, Response, WorkerChannels},
    preferences::DesktopPreferences,
    state::{UiState, UserError},
    tray::{TrayCommand, TrayRuntime},
};

const APPLICATION_ID: &str = "io.github.rayan6ms.Noire";
const RESPONSE_INTERVAL: Duration = Duration::from_millis(33);
const BACKGROUND: u32 = 0x0009_0b10;
const SURFACE: u32 = 0x0011_151d;
const SURFACE_RAISED: u32 = 0x0018_1e29;
const BORDER: u32 = 0x0029_3140;
const TEXT: u32 = 0x00f3_f5f7;
const MUTED: u32 = 0x0099_a3b3;
const ACCENT: u32 = 0x008b_5cf6;
const ACCENT_HOVER: u32 = 0x009f_7aea;
const SUCCESS: u32 = 0x0039_d98a;
const DANGER: u32 = 0x00ff_6b7a;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Home,
    Settings,
}

struct NoireView {
    state: UiState,
    channels: WorkerChannels,
    outstanding: u32,
    page: Page,
    preferences: DesktopPreferences,
    close_to_tray: Arc<AtomicBool>,
    tray: TrayRuntime,
    diagnostics: Option<String>,
    local_notice: Option<String>,
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
            diagnostics: None,
            local_notice: (!tray_available).then(|| {
                "The desktop has no system tray; Noire will stay visible and exit when closed."
                    .to_owned()
            }),
        };
        this.send(Request::Refresh, false);
        this
    }

    fn send(&mut self, request: Request, mutation: bool) {
        match self.channels.requests.try_send(request) {
            Ok(()) => {
                self.outstanding = self.outstanding.saturating_add(1);
                if mutation {
                    self.state.set_request_pending(true);
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => self.state.reject(
                UserError::new(
                    "ui-busy",
                    "Noire is still applying the previous change.",
                    "Wait a moment, then try again.",
                    true,
                ),
                None,
            ),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.state.reject(communication_stopped_error(), None);
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
                    self.state.reject(communication_stopped_error(), None);
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
                TrayCommand::ToggleProcessing => {
                    let active = self
                        .state
                        .snapshot()
                        .is_some_and(|snapshot| snapshot.active);
                    self.send(Request::SetActive(!active), true);
                }
                TrayCommand::Quit => {
                    let _ignored = self.channels.requests.try_send(Request::Shutdown);
                    cx.quit();
                }
            }
        }
    }

    fn persist_preferences(&mut self) {
        self.close_to_tray
            .store(self.preferences.close_to_tray, Ordering::Relaxed);
        self.local_notice = self
            .preferences
            .save()
            .err()
            .map(|_| "Noire could not save the desktop preference.".to_owned());
    }

    fn toggle_active(&mut self) {
        let active = self
            .state
            .snapshot()
            .is_some_and(|snapshot| snapshot.active);
        self.send(Request::SetActive(!active), true);
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let settings_label = if self.page == Page::Settings {
            "Back"
        } else {
            "Settings"
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .pb_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().size_3().rounded_full().bg(rgb(ACCENT)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("Noire"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child("Private, local microphone cleanup"),
                            ),
                    ),
            )
            .child(
                div()
                    .id("settings-navigation")
                    .cursor_pointer()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .px_4()
                    .py_2()
                    .text_sm()
                    .hover(|style| style.bg(rgb(SURFACE_RAISED)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.page = if view.page == Page::Home {
                            Page::Settings
                        } else {
                            Page::Home
                        };
                        cx.notify();
                    }))
                    .child(settings_label),
            )
    }

    #[allow(clippy::too_many_lines)]
    fn home(&self, cx: &mut Context<Self>) -> Div {
        let presentation = self.state.presentation();
        let snapshot = self.state.snapshot();
        let active = snapshot.is_some_and(|snapshot| snapshot.active);
        let controls_enabled = presentation.controls_enabled;
        let rms = snapshot.map_or(0.0, |snapshot| snapshot.metrics.rms.clamp(0.0, 1.0));
        let peak = snapshot.map_or(0.0, |snapshot| snapshot.metrics.peak.clamp(0.0, 1.0));
        let model = snapshot.map_or("FastEnhancer-B 48 kHz", |snapshot| {
            snapshot.model_id.as_str()
        });

        let mut root = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_4()
                .rounded_xl()
                .border_1()
                .border_color(if active { rgb(ACCENT) } else { rgb(BORDER) })
                .bg(rgb(SURFACE))
                .p_6()
                .child(
                    div()
                        .size(px(86.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(if active {
                            rgb(ACCENT)
                        } else {
                            rgb(SURFACE_RAISED)
                        })
                        .border_1()
                        .border_color(if active {
                            rgb(ACCENT_HOVER)
                        } else {
                            rgb(BORDER)
                        })
                        .text_3xl()
                        .child(if active { "●" } else { "○" }),
                )
                .child(
                    div()
                        .text_2xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(presentation.status),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .text_center()
                        .child(presentation.detail),
                )
                .child(
                    div()
                        .id("primary-action")
                        .cursor_pointer()
                        .rounded_lg()
                        .bg(if active {
                            rgb(SURFACE_RAISED)
                        } else {
                            rgb(ACCENT)
                        })
                        .border_1()
                        .border_color(if active { rgb(BORDER) } else { rgb(ACCENT) })
                        .px_6()
                        .py_3()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .when(!controls_enabled, |button| button.opacity(0.45))
                        .when(controls_enabled, |button| {
                            button
                                .hover(|style| style.bg(rgb(ACCENT_HOVER)))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.toggle_active();
                                    cx.notify();
                                }))
                        })
                        .child(if self.state.request_pending() {
                            "Applying…".to_owned()
                        } else {
                            presentation.primary_action
                        }),
                ),
        );

        if let Some(error) = presentation.error_message {
            root = root.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(DANGER))
                    .bg(rgb(0x0021_1218))
                    .p_4()
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(error))
                    .when_some(presentation.recovery, |card, recovery| {
                        card.child(div().text_sm().text_color(rgb(MUTED)).child(recovery))
                    })
                    .when(presentation.retryable, |card| {
                        card.child(
                            div()
                                .id("retry")
                                .cursor_pointer()
                                .text_sm()
                                .text_color(rgb(ACCENT_HOVER))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.send(Request::Retry, true);
                                    cx.notify();
                                }))
                                .child("Retry"),
                        )
                    }),
            );
        }

        root.child(
            div()
                .flex()
                .flex_col()
                .gap_4()
                .rounded_xl()
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(SURFACE))
                .p_5()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Live input"),
                        )
                        .child(div().text_xs().text_color(rgb(SUCCESS)).child(if active {
                            "MONITORING"
                        } else {
                            "IDLE"
                        })),
                )
                .child(meter("Voice", rms, ACCENT))
                .child(meter("Peak", peak, SUCCESS))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .pt_2()
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("Engine")
                        .child(model.to_owned()),
                ),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn settings(&self, cx: &mut Context<Self>) -> Div {
        let snapshot = self.state.snapshot();
        let controls_enabled = self.state.presentation().controls_enabled;
        let selected_input = snapshot.map_or("", |snapshot| snapshot.input_stable_id.as_str());
        let strength = snapshot.map_or(0.75, |snapshot| snapshot.strength);
        let latency = snapshot.map_or("low", |snapshot| snapshot.latency_profile.as_str());
        let fail_mode = snapshot.map_or("closed", |snapshot| snapshot.fail_mode.as_str());
        let suppression = snapshot.is_none_or(|snapshot| snapshot.suppression_enabled);
        let launch_at_login = snapshot.is_some_and(|snapshot| snapshot.launch_at_login);
        let tray_available = self.tray.available();

        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(section_title(
                "Audio",
                "Sensible defaults; tune only what your microphone needs.",
            ))
            .child(
                settings_card()
                    .child(toggle_row(
                        "suppression-toggle",
                        "Noise suppression",
                        "Keep delayed dry audio when disabled so timing stays stable.",
                        suppression,
                        controls_enabled,
                        cx.listener(|view, _, _, cx| {
                            let enabled = view
                                .state
                                .snapshot()
                                .is_none_or(|snapshot| snapshot.suppression_enabled);
                            view.send(Request::SetSuppressionEnabled(!enabled), true);
                            cx.notify();
                        }),
                    ))
                    .child(strength_row(strength, controls_enabled, cx))
                    .child(choice_row(
                        "Latency",
                        "Low minimizes delay; Balanced adds resilience on busy systems.",
                        [("Low", "low"), ("Balanced", "balanced")],
                        latency,
                        controls_enabled,
                        |value| Request::SetLatencyProfile(value.to_owned()),
                        cx,
                    ))
                    .child(choice_row(
                        "Failure behavior",
                        "Fail closed prevents accidental unsuppressed audio during faults.",
                        [("Closed", "closed"), ("Open", "open")],
                        fail_mode,
                        controls_enabled,
                        |value| Request::SetFailMode(value.to_owned()),
                        cx,
                    )),
            )
            .child(section_title(
                "Microphone",
                "Choose a physical input or follow the system default.",
            ))
            .child(
                settings_card().children(self.state.input_choices().into_iter().enumerate().map(
                    |(index, choice)| {
                        let selected = choice.stable_id == selected_input;
                        let stable_id = choice.stable_id;
                        div()
                            .id(SharedString::from(format!("input-{index}")))
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_between()
                            .p_4()
                            .when(index > 0, |row| row.border_t_1().border_color(rgb(BORDER)))
                            .when(controls_enabled, |row| {
                                row.hover(|style| style.bg(rgb(SURFACE_RAISED))).on_click(
                                    cx.listener(move |view, _, _, cx| {
                                        view.send(Request::SelectInput(stable_id.clone()), true);
                                        cx.notify();
                                    }),
                                )
                            })
                            .child(div().text_sm().child(choice.label))
                            .child(if selected { "●" } else { "○" })
                    },
                )),
            )
            .child(section_title(
                "Startup & tray",
                "Desktop behavior stays separate from audio settings.",
            ))
            .child(
                settings_card()
                    .child(toggle_row(
                        "launch-at-login",
                        "Start at login",
                        "Start the Noire background service with your desktop session.",
                        launch_at_login,
                        controls_enabled,
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
                        "Hide the window on future launches and keep Noire in the tray.",
                        self.preferences.start_minimized && tray_available,
                        tray_available,
                        cx.listener(|view, _, _, cx| {
                            view.preferences.start_minimized = !view.preferences.start_minimized;
                            view.persist_preferences();
                            cx.notify();
                        }),
                    ))
                    .child(toggle_row(
                        "close-to-tray",
                        "Close to tray",
                        "Closing the window keeps the lightweight controller available.",
                        self.preferences.close_to_tray && tray_available,
                        tray_available,
                        cx.listener(|view, _, _, cx| {
                            view.preferences.close_to_tray = !view.preferences.close_to_tray;
                            view.persist_preferences();
                            cx.notify();
                        }),
                    )),
            )
            .child(section_title(
                "Support",
                "A privacy-safe snapshot for troubleshooting.",
            ))
            .child(
                settings_card()
                    .child(
                        div()
                            .id("diagnostics")
                            .cursor_pointer()
                            .p_4()
                            .hover(|style| style.bg(rgb(SURFACE_RAISED)))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.send(Request::Diagnostics, false);
                                cx.notify();
                            }))
                            .child("Generate diagnostics"),
                    )
                    .when_some(self.diagnostics.clone(), |card, report| {
                        card.child(
                            div()
                                .border_t_1()
                                .border_color(rgb(BORDER))
                                .p_4()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .line_height(px(19.0))
                                .child(report),
                        )
                    }),
            )
            .when_some(self.local_notice.clone(), |root, notice| {
                root.child(div().text_sm().text_color(rgb(DANGER)).child(notice))
            })
    }
}

impl Render for NoireView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .id("application-scroll")
            .overflow_y_scroll()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .w_full()
                    .max_w(px(680.0))
                    .mx_auto()
                    .p_6()
                    .child(self.header(cx))
                    .child(match self.page {
                        Page::Home => self.home(cx),
                        Page::Settings => self.settings(cx),
                    })
                    .child(
                        div()
                            .pt_6()
                            .text_center()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!(
                                "Noire {} · local processing only",
                                env!("CARGO_PKG_VERSION")
                            )),
                    ),
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

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.0), px(760.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            show: !hidden,
            app_id: Some(APPLICATION_ID.to_owned()),
            window_min_size: Some(size(px(440.0), px(560.0))),
            window_background: WindowBackgroundAppearance::Opaque,
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
            cx.new(|cx| NoireView::new(preferences.clone(), Arc::clone(&close_to_tray), tray, cx))
        });
        if window_result.is_err() {
            eprintln!("Noire could not create its GPUI window.");
            cx.quit();
        } else if !hidden {
            cx.activate(true);
        }
    });
}

fn settings_card() -> Div {
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded_xl()
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(SURFACE))
}

fn section_title(title: &'static str, subtitle: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child(subtitle))
}

fn toggle_row(
    id: &'static str,
    title: &'static str,
    subtitle: &'static str,
    enabled: bool,
    interactive: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .p_4()
        .cursor_pointer()
        .when(interactive, |row| {
            row.hover(|style| style.bg(rgb(SURFACE_RAISED)))
                .on_click(on_click)
        })
        .when(!interactive, |row| row.opacity(0.45))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(gpui::FontWeight::MEDIUM).child(title))
                .child(div().text_xs().text_color(rgb(MUTED)).child(subtitle)),
        )
        .child(
            div()
                .w(px(42.0))
                .h(px(24.0))
                .p(px(3.0))
                .rounded_full()
                .bg(if enabled { rgb(ACCENT) } else { rgb(BORDER) })
                .flex()
                .justify_end()
                .when(!enabled, gpui::Styled::justify_start)
                .child(div().size(px(18.0)).rounded_full().bg(rgb(TEXT))),
        )
}

fn strength_row(strength: f64, interactive: bool, cx: &mut Context<NoireView>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .border_t_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .flex()
                .justify_between()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child("Strength"),
                )
                .child(format!("{:.0}%", strength * 100.0)),
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
                                .rounded_md()
                                .border_1()
                                .border_color(if selected { rgb(ACCENT) } else { rgb(BORDER) })
                                .bg(if selected {
                                    rgb(0x002b_2145)
                                } else {
                                    rgb(SURFACE_RAISED)
                                })
                                .py_2()
                                .text_center()
                                .text_sm()
                                .when(interactive, |button| {
                                    button.on_click(cx.listener(move |view, _, _, cx| {
                                        view.send(Request::SetStrength(value), true);
                                        cx.notify();
                                    }))
                                })
                                .child(format!("{:.0}%", value * 100.0))
                        }),
                ),
        )
}

fn choice_row<const N: usize>(
    title: &'static str,
    subtitle: &'static str,
    choices: [(&'static str, &'static str); N],
    selected: &str,
    interactive: bool,
    request: impl Fn(&str) -> Request + Copy + 'static,
    cx: &mut Context<NoireView>,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .p_4()
        .border_t_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(gpui::FontWeight::MEDIUM).child(title))
                .child(div().text_xs().text_color(rgb(MUTED)).child(subtitle)),
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
                                .rounded_md()
                                .border_1()
                                .border_color(if is_selected {
                                    rgb(ACCENT)
                                } else {
                                    rgb(BORDER)
                                })
                                .bg(if is_selected {
                                    rgb(0x002b_2145)
                                } else {
                                    rgb(SURFACE_RAISED)
                                })
                                .px_3()
                                .py_2()
                                .text_sm()
                                .when(interactive, |button| {
                                    button.on_click(cx.listener(move |view, _, _, cx| {
                                        view.send(request(value), true);
                                        cx.notify();
                                    }))
                                })
                                .child(label)
                        }),
                ),
        )
}

#[allow(clippy::cast_possible_truncation)]
fn meter(label: &'static str, value: f64, color: u32) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .justify_between()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(label)
                .child(format!("{:.0}%", value * 100.0)),
        )
        .child(
            div()
                .h(px(7.0))
                .w_full()
                .rounded_full()
                .overflow_hidden()
                .bg(rgb(SURFACE_RAISED))
                .child(
                    div()
                        .h_full()
                        .w(relative(value as f32))
                        .rounded_full()
                        .bg(rgb(color)),
                ),
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
