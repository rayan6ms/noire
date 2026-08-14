//! GTK widgets and main-thread state rendering.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc::TryRecvError,
    time::Duration,
};

use gtk::{Application, ApplicationWindow, accessible::Property, glib, prelude::*};
use gtk4 as gtk;
use noire_ipc::DiagnosticReport;

use crate::{
    client::{self, Request, Response},
    i18n::tr,
    state::{UiState, UserError},
};

const APPLICATION_ID: &str = "io.github.rayan6ms.Noire";
const RESPONSE_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone)]
struct Widgets {
    root: gtk::ScrolledWindow,
    status: gtk::Label,
    detail: gtk::Label,
    spinner: gtk::Spinner,
    primary: gtk::Button,
    error_revealer: gtk::Revealer,
    error_text: gtk::Label,
    retry: gtk::Button,
    input_model: gtk::StringList,
    input: gtk::DropDown,
    suppression: gtk::Switch,
    strength: gtk::Scale,
    strength_value: gtk::Label,
    latency: gtk::DropDown,
    fail_mode: gtk::DropDown,
    launch_at_login: gtk::Switch,
    rms: gtk::LevelBar,
    peak: gtk::LevelBar,
    runtime: gtk::Label,
    diagnostics: gtk::Button,
    diagnostic_revealer: gtk::Revealer,
    diagnostic_text: gtk::Label,
    #[cfg(test)]
    user_guide: gtk::LinkButton,
    #[cfg(test)]
    troubleshooting: gtk::LinkButton,
    #[cfg(test)]
    privacy: gtk::LinkButton,
    about: gtk::Button,
}

pub(crate) fn run() {
    let application = Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    application.connect_activate(build_ui);
    application.run();
}

fn build_ui(application: &Application) {
    let channels = client::spawn();
    let state = Rc::new(RefCell::new(UiState::default()));
    let updating = Rc::new(Cell::new(false));
    let outstanding = Rc::new(Cell::new(0_u32));
    let widgets = build_widgets();

    let title = gtk::Label::new(Some("Noire"));
    title.add_css_class("title");
    let header = gtk::HeaderBar::builder().title_widget(&title).build();
    let window = ApplicationWindow::builder()
        .application(application)
        .title("Noire")
        .default_width(640)
        .default_height(760)
        .width_request(420)
        .child(&widgets.root)
        .build();
    window.set_titlebar(Some(&header));
    wire_actions(
        &widgets,
        &window,
        &channels.requests,
        &state,
        &updating,
        &outstanding,
    );
    render(&widgets, &state.borrow(), &updating);

    queue(
        Request::Refresh,
        &channels.requests,
        &state,
        &widgets,
        &updating,
        &outstanding,
        false,
    );
    receive_responses(
        channels.responses,
        &state,
        &widgets,
        &updating,
        &outstanding,
    );

    let shutdown = channels.requests.clone();
    window.connect_close_request(move |_| {
        let _ignored = shutdown.try_send(Request::Shutdown);
        glib::Propagation::Proceed
    });
    window.present();
}

// Keeping the declarative widget hierarchy together makes its reading and
// keyboard order directly auditable and lets the same widgets be verified with
// deterministic daemon-state fixtures.
#[allow(clippy::too_many_lines)]
fn build_widgets() -> Widgets {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let (status_card, status, detail, spinner, primary) = status_card();
    content.append(&status_card);
    let (error_revealer, error_text, retry) = error_card();
    content.append(&error_revealer);
    content.append(&section_heading("Settings"));
    let settings = gtk::Box::new(gtk::Orientation::Vertical, 1);
    settings.add_css_class("boxed-list");

    let input_model = gtk::StringList::new(&["Follow system default"]);
    let input = gtk::DropDown::builder()
        .model(&input_model)
        .enable_search(true)
        .hexpand(true)
        .focusable(true)
        .build();
    label_accessible(
        &input,
        "Microphone",
        "Physical microphone processed by Noire",
    );
    settings.append(&setting_row(
        "Microphone",
        "Choose a physical input or follow the system default.",
        &input,
    ));

    let suppression = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .focusable(true)
        .build();
    label_accessible(
        &suppression,
        "Noise suppression",
        "Enable RNNoise processing while retaining matched latency",
    );
    settings.append(&setting_row(
        "Noise suppression",
        "Turn processing off for latency-matched dry audio.",
        &suppression,
    ));

    let strength = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    strength.set_hexpand(true);
    strength.set_focusable(true);
    strength.set_draw_value(false);
    strength.set_accessible_role(gtk::AccessibleRole::Slider);
    label_accessible(
        &strength,
        "Suppression strength",
        "Amount of processed signal, from zero through one hundred percent",
    );
    let strength_value = gtk::Label::new(Some("0%"));
    strength_value.set_width_chars(5);
    let strength_control = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    strength_control.set_size_request(220, -1);
    strength_control.append(&strength);
    strength_control.append(&strength_value);
    settings.append(&setting_row(
        "Strength",
        "Blend smoothly between delayed dry and processed audio.",
        &strength_control,
    ));

    let latency = gtk::DropDown::from_strings(&["Low", "Balanced"]);
    latency.set_focusable(true);
    label_accessible(&latency, "Latency profile", "Audio buffering profile");
    settings.append(&setting_row(
        "Latency",
        "Use Balanced if the system cannot sustain the Low profile.",
        &latency,
    ));

    let fail_mode = gtk::DropDown::from_strings(&["Fail closed (recommended)", "Fail open"]);
    fail_mode.set_focusable(true);
    label_accessible(
        &fail_mode,
        "Failure behavior",
        "Whether failures produce silence or explicitly allow delayed dry audio",
    );
    settings.append(&setting_row(
        "Failure behavior",
        "Fail open can expose unsuppressed microphone audio during a fault.",
        &fail_mode,
    ));

    let launch_at_login = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .focusable(true)
        .build();
    label_accessible(
        &launch_at_login,
        "Launch at login",
        "Start the Noire user service automatically after login",
    );
    settings.append(&setting_row(
        "Launch at login",
        "Enable Noire through the per-user systemd manager.",
        &launch_at_login,
    ));
    content.append(&settings);

    content.append(&section_heading("Live status"));
    let rms = level_row(
        &content,
        "Signal level",
        "Current root-mean-square output level",
    );
    let peak = level_row(&content, "Peak level", "Current peak output level");
    let runtime = gtk::Label::new(Some("Waiting for daemon details…"));
    runtime.set_xalign(0.0);
    runtime.set_wrap(true);
    runtime.add_css_class("dim-label");
    runtime.set_accessible_role(gtk::AccessibleRole::Status);
    runtime.update_property(&[Property::Label("Noire runtime details")]);
    content.append(&runtime);

    content.append(&section_heading(&tr("Information")));
    let information = gtk::Box::new(gtk::Orientation::Vertical, 1);
    information.add_css_class("boxed-list");
    let diagnostics = gtk::Button::with_label(&tr("Load diagnostics"));
    diagnostics.set_focusable(true);
    label_accessible(
        &diagnostics,
        &tr("Load diagnostics"),
        &tr("Read a sanitized local report containing no audio"),
    );
    information.append(&setting_row(
        &tr("Diagnostics"),
        &tr("Read versions, state, and recovery details without collecting audio."),
        &diagnostics,
    ));
    let diagnostic_text = gtk::Label::new(None);
    diagnostic_text.set_xalign(0.0);
    diagnostic_text.set_wrap(true);
    diagnostic_text.set_selectable(true);
    diagnostic_text.add_css_class("monospace");
    diagnostic_text.set_accessible_role(gtk::AccessibleRole::Status);
    diagnostic_text.update_property(&[Property::Label(&tr("Noire diagnostics"))]);
    let diagnostic_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&diagnostic_text)
        .build();
    information.append(&diagnostic_revealer);

    let help = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let user_guide = local_help_link("USER_GUIDE.md", &tr("User guide"));
    let troubleshooting = local_help_link("TROUBLESHOOTING.md", &tr("Troubleshooting"));
    let privacy = local_help_link("PRIVACY.md", &tr("Privacy"));
    help.append(&user_guide);
    help.append(&troubleshooting);
    help.append(&privacy);
    information.append(&setting_row(
        &tr("Help"),
        &tr("Open the documentation installed with Noire."),
        &help,
    ));
    let about = gtk::Button::with_label(&tr("About Noire"));
    about.set_focusable(true);
    label_accessible(
        &about,
        &tr("About Noire"),
        &tr("Show version, authorship, license, and project website"),
    );
    information.append(&setting_row(
        &tr("About"),
        &tr("Version, credits, license, and project website."),
        &about,
    ));
    content.append(&information);

    let root = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content)
        .build();
    root.set_accessible_role(gtk::AccessibleRole::Main);
    Widgets {
        root,
        status,
        detail,
        spinner,
        primary,
        error_revealer,
        error_text,
        retry,
        input_model,
        input,
        suppression,
        strength,
        strength_value,
        latency,
        fail_mode,
        launch_at_login,
        rms,
        peak,
        runtime,
        diagnostics,
        diagnostic_revealer,
        diagnostic_text,
        #[cfg(test)]
        user_guide,
        #[cfg(test)]
        troubleshooting,
        #[cfg(test)]
        privacy,
        about,
    }
}

fn local_help_link(file: &str, label: &str) -> gtk::LinkButton {
    let link = gtk::LinkButton::builder()
        .label(label)
        .uri(format!("file:///usr/share/doc/noire-daemon/{file}"))
        .focusable(true)
        .build();
    label_accessible(
        &link,
        label,
        &tr("Open the locally installed Noire documentation"),
    );
    link
}

fn status_card() -> (
    gtk::Frame,
    gtk::Label,
    gtk::Label,
    gtk::Spinner,
    gtk::Button,
) {
    let status = gtk::Label::new(Some("Connecting to the daemon…"));
    status.set_xalign(0.0);
    status.add_css_class("title-2");
    status.set_accessible_role(gtk::AccessibleRole::Status);
    status.update_property(&[Property::Label("Noire status")]);
    let detail = gtk::Label::new(Some("Reading current state."));
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    detail.add_css_class("dim-label");
    detail.update_property(&[Property::Label("Noire status detail")]);
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
    labels.set_hexpand(true);
    labels.append(&status);
    labels.append(&detail);
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    let primary = gtk::Button::with_label("Start");
    primary.add_css_class("suggested-action");
    primary.set_tooltip_text(Some("Start or stop daemon-owned noise reduction"));
    primary.update_property(&[Property::Label("Start noise reduction")]);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_margin_top(18);
    row.set_margin_bottom(18);
    row.set_margin_start(18);
    row.set_margin_end(18);
    row.append(&labels);
    row.append(&spinner);
    row.append(&primary);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&row));
    (frame, status, detail, spinner, primary)
}

fn error_card() -> (gtk::Revealer, gtk::Label, gtk::Button) {
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    let text = gtk::Label::new(None);
    text.set_xalign(0.0);
    text.set_wrap(true);
    text.set_hexpand(true);
    text.set_accessible_role(gtk::AccessibleRole::Alert);
    text.update_property(&[Property::Label("Noire error and recovery")]);
    let retry = gtk::Button::with_label("Retry");
    retry.set_tooltip_text(Some("Retry recovery using current daemon settings"));
    retry.update_property(&[Property::Label("Retry daemon recovery")]);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_margin_top(12);
    row.set_margin_bottom(12);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.append(&icon);
    row.append(&text);
    row.append(&retry);
    let frame = gtk::Frame::new(None);
    frame.add_css_class("error");
    frame.set_child(Some(&row));
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&frame)
        .build();
    (revealer, text, retry)
}

fn section_heading(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("heading");
    label
}

fn setting_row(title: &str, description: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let title = gtk::Label::new(Some(title));
    title.set_xalign(0.0);
    let description = gtk::Label::new(Some(description));
    description.set_xalign(0.0);
    description.set_wrap(true);
    description.add_css_class("dim-label");
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    labels.set_hexpand(true);
    labels.append(&title);
    labels.append(&description);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    row.set_margin_top(12);
    row.set_margin_bottom(12);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.append(&labels);
    row.append(control);
    row
}

fn level_row(content: &gtk::Box, title: &str, description: &str) -> gtk::LevelBar {
    let level = gtk::LevelBar::for_interval(0.0, 1.0);
    level.set_hexpand(true);
    level.set_accessible_role(gtk::AccessibleRole::Meter);
    level.update_property(&[Property::Label(title), Property::Description(description)]);
    let row = setting_row(title, description, &level);
    content.append(&row);
    level
}

fn label_accessible(
    widget: &(impl IsA<gtk::Accessible> + IsA<gtk::Widget>),
    label: &str,
    description: &str,
) {
    widget.update_property(&[Property::Label(label), Property::Description(description)]);
    widget.set_tooltip_text(Some(description));
}

// Signal wiring mirrors the visible controls in one place so every mutation
// shares the same asynchronous queue and convergence behavior.
#[allow(clippy::too_many_lines)]
fn wire_actions(
    widgets: &Widgets,
    window: &ApplicationWindow,
    sender: &tokio::sync::mpsc::Sender<Request>,
    state: &Rc<RefCell<UiState>>,
    updating: &Rc<Cell<bool>>,
    outstanding: &Rc<Cell<u32>>,
) {
    let sender_clone = sender.clone();
    let state_clone = Rc::clone(state);
    let widgets_clone = widgets.clone();
    let updating_clone = Rc::clone(updating);
    let outstanding_clone = Rc::clone(outstanding);
    widgets.primary.connect_clicked(move |_| {
        let active = state_clone
            .borrow()
            .snapshot()
            .is_some_and(|snapshot| snapshot.active);
        queue(
            Request::SetActive(!active),
            &sender_clone,
            &state_clone,
            &widgets_clone,
            &updating_clone,
            &outstanding_clone,
            true,
        );
    });

    connect_switch(
        &widgets.suppression,
        sender,
        state,
        widgets,
        updating,
        outstanding,
        Request::SetSuppressionEnabled,
    );
    connect_switch(
        &widgets.launch_at_login,
        sender,
        state,
        widgets,
        updating,
        outstanding,
        Request::SetLaunchAtLogin,
    );

    let sender_clone = sender.clone();
    let state_clone = Rc::clone(state);
    let widgets_clone = widgets.clone();
    let updating_clone = Rc::clone(updating);
    let outstanding_clone = Rc::clone(outstanding);
    widgets.strength.connect_value_changed(move |scale| {
        widgets_clone
            .strength_value
            .set_text(&format!("{:.0}%", scale.value() * 100.0));
        if !updating_clone.get() {
            queue(
                Request::SetStrength(scale.value()),
                &sender_clone,
                &state_clone,
                &widgets_clone,
                &updating_clone,
                &outstanding_clone,
                true,
            );
        }
    });

    connect_dropdown(
        &widgets.latency,
        sender,
        state,
        widgets,
        updating,
        outstanding,
        |selected| {
            Request::SetLatencyProfile(if selected == 0 { "low" } else { "balanced" }.to_owned())
        },
    );
    connect_dropdown(
        &widgets.fail_mode,
        sender,
        state,
        widgets,
        updating,
        outstanding,
        |selected| Request::SetFailMode(if selected == 0 { "closed" } else { "open" }.to_owned()),
    );

    let sender_clone = sender.clone();
    let state_clone = Rc::clone(state);
    let widgets_clone = widgets.clone();
    let updating_clone = Rc::clone(updating);
    let outstanding_clone = Rc::clone(outstanding);
    widgets.input.connect_selected_notify(move |dropdown| {
        if updating_clone.get() {
            return;
        }
        let choices = state_clone.borrow().input_choices();
        if let Some(choice) = choices.get(dropdown.selected() as usize) {
            queue(
                Request::SelectInput(choice.stable_id.clone()),
                &sender_clone,
                &state_clone,
                &widgets_clone,
                &updating_clone,
                &outstanding_clone,
                true,
            );
        }
    });

    let sender_clone = sender.clone();
    let state_clone = Rc::clone(state);
    let widgets_clone = widgets.clone();
    let updating_clone = Rc::clone(updating);
    let outstanding_clone = Rc::clone(outstanding);
    widgets.retry.connect_clicked(move |_| {
        let request = if state_clone.borrow().snapshot().is_some() {
            Request::Retry
        } else {
            Request::Refresh
        };
        queue(
            request,
            &sender_clone,
            &state_clone,
            &widgets_clone,
            &updating_clone,
            &outstanding_clone,
            true,
        );
    });

    let sender_clone = sender.clone();
    let state_clone = Rc::clone(state);
    let widgets_clone = widgets.clone();
    let updating_clone = Rc::clone(updating);
    let outstanding_clone = Rc::clone(outstanding);
    widgets.diagnostics.connect_clicked(move |_| {
        queue(
            Request::Diagnostics,
            &sender_clone,
            &state_clone,
            &widgets_clone,
            &updating_clone,
            &outstanding_clone,
            true,
        );
    });

    let parent = window.clone();
    widgets.about.connect_clicked(move |_| show_about(&parent));
}

fn about_dialog() -> gtk::AboutDialog {
    gtk::AboutDialog::builder()
        .modal(true)
        .program_name("Noire")
        .version(env!("CARGO_PKG_VERSION"))
        .comments(tr("Native microphone noise suppression for PipeWire"))
        .authors(["rayan6ms"])
        .copyright("Copyright © 2026 rayan6ms")
        .license_type(gtk::License::Gpl30)
        .logo_icon_name(APPLICATION_ID)
        .website(env!("CARGO_PKG_HOMEPAGE"))
        .website_label(tr("Noire project website"))
        .build()
}

fn show_about(parent: &ApplicationWindow) {
    let dialog = about_dialog();
    dialog.set_transient_for(Some(parent));
    dialog.present();
}

fn connect_switch(
    switch: &gtk::Switch,
    sender: &tokio::sync::mpsc::Sender<Request>,
    state: &Rc<RefCell<UiState>>,
    widgets: &Widgets,
    updating: &Rc<Cell<bool>>,
    outstanding: &Rc<Cell<u32>>,
    request: fn(bool) -> Request,
) {
    let sender = sender.clone();
    let state = Rc::clone(state);
    let widgets = widgets.clone();
    let updating = Rc::clone(updating);
    let outstanding = Rc::clone(outstanding);
    switch.connect_active_notify(move |switch| {
        if !updating.get() {
            queue(
                request(switch.is_active()),
                &sender,
                &state,
                &widgets,
                &updating,
                &outstanding,
                true,
            );
        }
    });
}

fn connect_dropdown(
    dropdown: &gtk::DropDown,
    sender: &tokio::sync::mpsc::Sender<Request>,
    state: &Rc<RefCell<UiState>>,
    widgets: &Widgets,
    updating: &Rc<Cell<bool>>,
    outstanding: &Rc<Cell<u32>>,
    request: impl Fn(u32) -> Request + 'static,
) {
    let sender = sender.clone();
    let state = Rc::clone(state);
    let widgets = widgets.clone();
    let updating = Rc::clone(updating);
    let outstanding = Rc::clone(outstanding);
    dropdown.connect_selected_notify(move |dropdown| {
        if !updating.get() {
            queue(
                request(dropdown.selected()),
                &sender,
                &state,
                &widgets,
                &updating,
                &outstanding,
                true,
            );
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn queue(
    request: Request,
    sender: &tokio::sync::mpsc::Sender<Request>,
    state: &Rc<RefCell<UiState>>,
    widgets: &Widgets,
    updating: &Rc<Cell<bool>>,
    outstanding: &Rc<Cell<u32>>,
    mutation: bool,
) {
    match sender.try_send(request) {
        Ok(()) => {
            outstanding.set(outstanding.get().saturating_add(1));
            if mutation {
                state.borrow_mut().set_request_pending(true);
                render(widgets, &state.borrow(), updating);
            }
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            state.borrow_mut().reject(
                UserError::new(
                    "ui-request-queue-busy",
                    "Noire is still handling the previous request.",
                    "Wait for the current request to finish, then retry.",
                    true,
                ),
                None,
            );
            render(widgets, &state.borrow(), updating);
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            state
                .borrow_mut()
                .reject(communication_stopped_error(), None);
            render(widgets, &state.borrow(), updating);
        }
    }
}

fn receive_responses(
    responses: std::sync::mpsc::Receiver<Response>,
    state: &Rc<RefCell<UiState>>,
    widgets: &Widgets,
    updating: &Rc<Cell<bool>>,
    outstanding: &Rc<Cell<u32>>,
) {
    let state_clone = Rc::clone(state);
    let widgets_clone = widgets.clone();
    let updating_clone = Rc::clone(updating);
    let outstanding_clone = Rc::clone(outstanding);
    glib::timeout_add_local(RESPONSE_INTERVAL, move || {
        let mut disconnected = false;
        loop {
            match responses.try_recv() {
                Ok(Response::State {
                    snapshot,
                    inputs,
                    refresh,
                    request_complete,
                }) => {
                    if refresh {
                        state_clone.borrow_mut().refresh(snapshot, inputs);
                    } else {
                        state_clone.borrow_mut().converge(snapshot, inputs);
                    }
                    if request_complete {
                        outstanding_clone.set(outstanding_clone.get().saturating_sub(1));
                    }
                }
                Ok(Response::Rejected {
                    error,
                    recovered,
                    request_complete,
                }) => {
                    state_clone.borrow_mut().reject(error, recovered);
                    if request_complete {
                        outstanding_clone.set(outstanding_clone.get().saturating_sub(1));
                    }
                }
                Ok(Response::Diagnostics(report)) => {
                    widgets_clone
                        .diagnostic_text
                        .set_text(&diagnostic_report_text(&report));
                    widgets_clone.diagnostic_revealer.set_reveal_child(true);
                    outstanding_clone.set(outstanding_clone.get().saturating_sub(1));
                }
                Ok(Response::Meters(metrics)) => {
                    state_clone.borrow_mut().update_metrics(metrics);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    state_clone
                        .borrow_mut()
                        .reject(communication_stopped_error(), None);
                    outstanding_clone.set(0);
                    disconnected = true;
                    break;
                }
            }
        }
        state_clone
            .borrow_mut()
            .set_request_pending(outstanding_clone.get() > 0);
        render(&widgets_clone, &state_clone.borrow(), &updating_clone);
        if disconnected {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
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

fn render(widgets: &Widgets, state: &UiState, updating: &Cell<bool>) {
    updating.set(true);
    let presentation = state.presentation();
    widgets.status.set_text(&presentation.status);
    widgets.detail.set_text(&presentation.detail);
    widgets.primary.set_label(&presentation.primary_action);
    widgets
        .primary
        .update_property(&[Property::Label(if presentation.primary_action == "Stop" {
            "Stop noise reduction"
        } else {
            "Start noise reduction"
        })]);
    widgets.spinner.set_spinning(state.request_pending());

    let connected = state.snapshot().is_some();
    let enabled = connected && presentation.controls_enabled;
    for widget in [
        widgets.input.upcast_ref::<gtk::Widget>(),
        widgets.suppression.upcast_ref(),
        widgets.strength.upcast_ref(),
        widgets.latency.upcast_ref(),
        widgets.fail_mode.upcast_ref(),
        widgets.launch_at_login.upcast_ref(),
        widgets.diagnostics.upcast_ref(),
        widgets.primary.upcast_ref(),
    ] {
        widget.set_sensitive(enabled);
    }

    if let Some(message) = presentation.error_message.as_deref() {
        let mut parts = vec![message.to_owned()];
        if let Some(code) = presentation.error_code.as_deref() {
            parts.push(format!("Error code: {code}"));
        }
        if let Some(recovery) = presentation.recovery.as_deref() {
            parts.push(format!("What to do: {recovery}"));
        }
        let text = parts.join("\n");
        widgets.error_text.set_text(&text);
        widgets.retry.set_visible(presentation.retryable);
        widgets.retry.set_sensitive(!state.request_pending());
        widgets.error_revealer.set_reveal_child(true);
    } else {
        widgets.error_revealer.set_reveal_child(false);
    }

    if let Some(snapshot) = state.snapshot() {
        let choices = state.input_choices();
        let labels: Vec<&str> = choices.iter().map(|choice| choice.label.as_str()).collect();
        widgets
            .input_model
            .splice(0, widgets.input_model.n_items(), &labels);
        let selected_id = snapshot.input_stable_id.as_str();
        let selected = choices
            .iter()
            .position(|choice| choice.stable_id == selected_id)
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or(0);
        widgets.input.set_selected(selected);
        widgets.suppression.set_active(snapshot.suppression_enabled);
        widgets.strength.set_value(snapshot.strength);
        widgets
            .strength_value
            .set_text(&format!("{:.0}%", snapshot.strength * 100.0));
        widgets
            .latency
            .set_selected(u32::from(snapshot.latency_profile != "low"));
        widgets
            .fail_mode
            .set_selected(u32::from(snapshot.fail_mode != "closed"));
        widgets.launch_at_login.set_active(snapshot.launch_at_login);
        widgets.rms.set_value(snapshot.metrics.rms.clamp(0.0, 1.0));
        widgets
            .peak
            .set_value(snapshot.metrics.peak.clamp(0.0, 1.0));
        widgets.runtime.set_text(&format!(
            "Noire {} · API {} · PipeWire {} · Model delay {} ms · revision {}",
            snapshot.build_version,
            snapshot.api_version,
            if snapshot.pipewire_version.is_empty() {
                "not connected"
            } else {
                snapshot.pipewire_version.as_str()
            },
            f64::from(snapshot.model_delay_samples) / 48.0,
            snapshot.revision,
        ));
    } else {
        widgets
            .input_model
            .splice(0, widgets.input_model.n_items(), &["Follow system default"]);
        widgets.input.set_selected(0);
        widgets.rms.set_value(0.0);
        widgets.peak.set_value(0.0);
        widgets.runtime.set_text("Daemon details are unavailable.");
    }
    updating.set(false);
}

#[cfg(test)]
mod tests {
    use noire_ipc::{
        API_VERSION, ERROR_CATALOG, ErrorInfo, InputDescriptor, Metrics, SNAPSHOT_SCHEMA_VERSION,
        Snapshot,
    };

    use super::*;

    fn snapshot(state: &str, active: bool) -> Snapshot {
        Snapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            api_version: API_VERSION.to_owned(),
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            revision: 19,
            device_revision: 4,
            state: state.to_owned(),
            active,
            launch_at_login: true,
            input_mode: "selected".to_owned(),
            input_stable_id: "usb:desk".to_owned(),
            input_display_name: "Desk microphone".to_owned(),
            channel: "auto".to_owned(),
            fallback_to_default: false,
            source_node_name: "io.github.rayan6ms.Noire.Microphone".to_owned(),
            latency_profile: "balanced".to_owned(),
            suppression_enabled: true,
            strength: 0.63,
            fail_mode: "closed".to_owned(),
            model_id: "org.rnnoise.nnnoiseless.default".to_owned(),
            model_delay_samples: 480,
            pipewire_version: "1.4.7".to_owned(),
            uptime_millis: 5,
            has_error: false,
            last_error: ErrorInfo::default(),
            metrics: Metrics {
                rms: 0.25,
                peak: 0.75,
                ..Metrics::default()
            },
        }
    }

    fn inputs() -> Vec<InputDescriptor> {
        vec![InputDescriptor {
            stable_id: "usb:desk".to_owned(),
            display_name: "Desk microphone".to_owned(),
            is_default: true,
            availability: "available".to_owned(),
        }]
    }

    fn diagnostic_report() -> DiagnosticReport {
        DiagnosticReport {
            schema_version: 1,
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            api_version: "1.0".to_owned(),
            state: "running".to_owned(),
            source_node_name: "io.github.rayan6ms.Noire.Microphone".to_owned(),
            selected_input_id: "usb:desk".to_owned(),
            last_error_code: String::new(),
            journal_hint: "journalctl --user-unit=noire.service --since=-15min".to_owned(),
            privacy:
                "contains no audio, raw device properties, environment dump, or automatic upload"
                    .to_owned(),
        }
    }

    fn assert_error_catalog_rendering(
        widgets: &Widgets,
        state: &mut UiState,
        updating: &Cell<bool>,
    ) {
        for entry in ERROR_CATALOG {
            let mut catalog_state = snapshot("degraded", true);
            catalog_state.has_error = true;
            catalog_state.last_error = ErrorInfo {
                code: entry.code.to_owned(),
                message: entry.cause.to_owned(),
                recovery: entry.recovery.to_owned(),
                component: "test".to_owned(),
                retryable: entry.retryable,
                timestamp_millis: 1,
            };
            state.converge(catalog_state, inputs());
            render(widgets, state, updating);
            let rendered = widgets.error_text.text();
            assert!(rendered.contains(entry.code), "{}", entry.code);
            assert!(rendered.contains(entry.cause), "{}", entry.code);
            assert!(rendered.contains(entry.recovery), "{}", entry.code);
            assert_eq!(
                widgets.retry.is_visible(),
                entry.retryable,
                "{}",
                entry.code
            );
            assert!(widgets.primary.is_sensitive(), "{}", entry.code);
        }
    }

    fn assert_information_content(widgets: &Widgets) {
        assert!(!widgets.diagnostic_revealer.reveals_child());
        widgets
            .diagnostic_text
            .set_text(&diagnostic_report_text(&diagnostic_report()));
        widgets.diagnostic_revealer.set_reveal_child(true);
        assert!(widgets.diagnostic_text.text().contains("contains no audio"));
        assert!(widgets.diagnostic_text.text().contains("noire.service"));
        assert!(!widgets.diagnostic_text.text().contains("environment dump:"));
        assert_eq!(
            widgets.user_guide.uri(),
            "file:///usr/share/doc/noire-daemon/USER_GUIDE.md"
        );
        let about = about_dialog();
        assert_eq!(about.program_name().as_deref(), Some("Noire"));
        assert_eq!(about.version().as_deref(), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(about.license_type(), gtk::License::Gpl30);
    }

    fn assert_information_accessibility(widgets: &Widgets, window: &gtk::Window) {
        for (name, control, role) in [
            (
                "diagnostics",
                widgets.diagnostics.upcast_ref::<gtk::Widget>(),
                gtk::AccessibleRole::Button,
            ),
            (
                "user guide",
                widgets.user_guide.upcast_ref(),
                gtk::AccessibleRole::Link,
            ),
            (
                "troubleshooting",
                widgets.troubleshooting.upcast_ref(),
                gtk::AccessibleRole::Link,
            ),
            (
                "privacy",
                widgets.privacy.upcast_ref(),
                gtk::AccessibleRole::Link,
            ),
            (
                "about",
                widgets.about.upcast_ref(),
                gtk::AccessibleRole::Button,
            ),
        ] {
            assert_eq!(control.accessible_role(), role, "{name}");
            assert!(control.is_focusable(), "{name}");
            assert!(control.is_sensitive(), "{name}");
            assert!(control.tooltip_text().is_some_and(|text| !text.is_empty()));
            assert!(control.grab_focus(), "{name}");
            assert!(
                gtk::prelude::RootExt::focus(window)
                    .is_some_and(|focused| focused == *control || focused.is_ancestor(control)),
                "{name}"
            );
        }
    }

    fn assert_information_focusable(widgets: &Widgets) {
        for (name, control) in [
            (
                "diagnostics",
                widgets.diagnostics.upcast_ref::<gtk::Widget>(),
            ),
            ("user guide", widgets.user_guide.upcast_ref()),
            ("troubleshooting", widgets.troubleshooting.upcast_ref()),
            ("privacy", widgets.privacy.upcast_ref()),
            ("about", widgets.about.upcast_ref()),
        ] {
            assert!(control.is_focusable(), "{name} must be keyboard focusable");
        }
    }

    #[test]
    #[ignore = "requires a disposable GTK display; run with run_phase8_ui_smoke.sh"]
    fn accessibility_tree_and_keyboard_paths_are_complete() -> Result<(), glib::BoolError> {
        gtk::init()?;
        let widgets = build_widgets();
        let updating = Cell::new(false);
        let mut degraded = snapshot("degraded", true);
        degraded.has_error = true;
        degraded.last_error = ErrorInfo {
            code: "input-unavailable".to_owned(),
            message: "The selected microphone is unavailable.".to_owned(),
            recovery: "Reconnect it or select another microphone.".to_owned(),
            component: "input".to_owned(),
            retryable: true,
            timestamp_millis: 1,
        };
        let mut state = UiState::default();
        state.converge(degraded, inputs());
        render(&widgets, &state, &updating);

        let window = gtk::Window::builder()
            .title("Noire accessibility test")
            .default_width(640)
            .default_height(760)
            .child(&widgets.root)
            .build();
        window.present();
        let context = glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }

        assert_eq!(widgets.root.accessible_role(), gtk::AccessibleRole::Main);
        assert_eq!(
            widgets.status.accessible_role(),
            gtk::AccessibleRole::Status
        );
        assert_eq!(
            widgets.error_text.accessible_role(),
            gtk::AccessibleRole::Alert
        );
        assert_eq!(widgets.rms.accessible_role(), gtk::AccessibleRole::Meter);
        assert_eq!(widgets.peak.accessible_role(), gtk::AccessibleRole::Meter);
        assert!(widgets.status.text().contains("attention"));
        assert!(widgets.error_text.text().contains("input-unavailable"));
        assert!(widgets.error_text.text().contains("What to do:"));

        for (name, control, role) in [
            (
                "primary",
                widgets.primary.upcast_ref::<gtk::Widget>(),
                gtk::AccessibleRole::Button,
            ),
            (
                "input",
                widgets.input.upcast_ref(),
                gtk::AccessibleRole::ComboBox,
            ),
            (
                "suppression",
                widgets.suppression.upcast_ref(),
                gtk::AccessibleRole::Switch,
            ),
            (
                "strength",
                widgets.strength.upcast_ref(),
                gtk::AccessibleRole::Slider,
            ),
            (
                "latency",
                widgets.latency.upcast_ref(),
                gtk::AccessibleRole::ComboBox,
            ),
            (
                "failure mode",
                widgets.fail_mode.upcast_ref(),
                gtk::AccessibleRole::ComboBox,
            ),
            (
                "launch at login",
                widgets.launch_at_login.upcast_ref(),
                gtk::AccessibleRole::Switch,
            ),
            (
                "retry",
                widgets.retry.upcast_ref(),
                gtk::AccessibleRole::Button,
            ),
        ] {
            assert_eq!(control.accessible_role(), role, "{name}");
            assert!(control.is_focusable(), "{name}");
            assert!(control.is_sensitive(), "{name}");
            assert!(
                control.tooltip_text().is_some_and(|text| !text.is_empty()),
                "{name}"
            );
            assert!(control.grab_focus(), "{name}");
            assert!(
                gtk::prelude::RootExt::focus(&window)
                    .is_some_and(|focused| focused == *control || focused.is_ancestor(control)),
                "{name}"
            );
        }
        assert_information_accessibility(&widgets, &window);
        window.close();
        Ok(())
    }

    #[test]
    #[ignore = "requires a disposable GTK display; run with run_phase8_ui_smoke.sh"]
    fn widget_state_matrix_tracks_daemon_truth_and_accessible_controls()
    -> Result<(), glib::BoolError> {
        gtk::init()?;
        let widgets = build_widgets();
        let updating = Cell::new(false);

        let mut state = UiState::default();
        state.converge(snapshot("running", true), inputs());
        render(&widgets, &state, &updating);
        assert_eq!(widgets.status.text(), "Noise reduction is active");
        assert_eq!(widgets.primary.label().as_deref(), Some("Stop"));
        assert!(widgets.primary.is_sensitive());
        assert!(widgets.suppression.is_active());
        assert_eq!(widgets.strength_value.text(), "63%");
        assert_eq!(widgets.latency.selected(), 1);
        assert!(widgets.launch_at_login.is_active());
        assert_eq!(widgets.input_model.n_items(), 2);
        assert_eq!(widgets.input.selected(), 1);
        assert!((widgets.rms.value() - 0.25).abs() < f64::EPSILON);
        assert!((widgets.peak.value() - 0.75).abs() < f64::EPSILON);
        assert!(widgets.runtime.text().contains("revision 19"));
        assert!(!widgets.error_revealer.reveals_child());
        assert_information_content(&widgets);

        state.set_request_pending(true);
        render(&widgets, &state, &updating);
        assert!(!widgets.primary.is_sensitive());
        assert!(widgets.spinner.is_spinning());

        let mut degraded = snapshot("running", true);
        degraded.has_error = true;
        degraded.last_error = ErrorInfo {
            code: "input-unavailable".to_owned(),
            message: "The selected microphone is unavailable.".to_owned(),
            recovery: "Reconnect it or select another microphone.".to_owned(),
            component: "input".to_owned(),
            retryable: true,
            timestamp_millis: 1,
        };
        state.converge(degraded, inputs());
        render(&widgets, &state, &updating);
        assert_eq!(widgets.status.text(), "Needs attention");
        assert!(widgets.error_revealer.reveals_child());
        assert!(widgets.error_text.text().contains("Reconnect it"));
        assert!(widgets.error_text.text().contains("input-unavailable"));
        assert!(widgets.error_text.text().contains("What to do:"));
        assert!(widgets.retry.is_visible());

        assert_error_catalog_rendering(&widgets, &mut state, &updating);

        state.converge(snapshot("recovering", true), inputs());
        render(&widgets, &state, &updating);
        assert_eq!(widgets.status.text(), "Reconnecting");
        assert!(widgets.detail.text().contains("safely muted"));

        state.reject(
            UserError::new(
                "daemon-unavailable",
                "The Noire background service is unavailable.",
                "Start the service, then retry.",
                true,
            ),
            None,
        );
        render(&widgets, &state, &updating);
        assert_eq!(widgets.status.text(), "Daemon unavailable");
        assert!(!widgets.primary.is_sensitive());
        assert!(widgets.error_revealer.reveals_child());

        for (name, control) in [
            ("primary", widgets.primary.upcast_ref::<gtk::Widget>()),
            ("input", widgets.input.upcast_ref()),
            ("suppression", widgets.suppression.upcast_ref()),
            ("strength", widgets.strength.upcast_ref()),
            ("latency", widgets.latency.upcast_ref()),
            ("fail mode", widgets.fail_mode.upcast_ref()),
            ("launch at login", widgets.launch_at_login.upcast_ref()),
            ("retry", widgets.retry.upcast_ref()),
        ] {
            assert!(control.is_focusable(), "{name} must be keyboard focusable");
        }
        assert_information_focusable(&widgets);
        assert_eq!(
            widgets.primary.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert_eq!(
            widgets.input.accessible_role(),
            gtk::AccessibleRole::ComboBox
        );
        assert_eq!(
            widgets.suppression.accessible_role(),
            gtk::AccessibleRole::Switch
        );
        assert_eq!(
            widgets.strength.accessible_role(),
            gtk::AccessibleRole::Slider
        );
        Ok(())
    }
}
