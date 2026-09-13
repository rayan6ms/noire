//! Suppression strength control with a local drag preview and one commit on release.

use std::{cell::Cell, rc::Rc};

use gpui::{
    Bounds, Context, DispatchPhase, EventEmitter, FocusHandle, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, Styled as _, Window, canvas, div,
    prelude::*, px, relative, rgb,
};

use super::Palette;

const THUMB_RADIUS: f32 = 8.0;

pub(super) struct StrengthChanged(pub f64);

pub(super) struct StrengthSlider {
    value: f64,
    saved_value: f64,
    dragging: bool,
    enabled: bool,
    palette: Palette,
    focus: FocusHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl EventEmitter<StrengthChanged> for StrengthSlider {}

impl StrengthSlider {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            value: 0.55,
            saved_value: 0.55,
            dragging: false,
            enabled: false,
            palette: Palette::dark(),
            focus: cx.focus_handle(),
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    pub(super) fn sync(&mut self, value: f64, enabled: bool, palette: Palette) {
        self.enabled = enabled;
        self.palette = palette;
        self.saved_value = value;
        // Keep the preview until release and while the daemon applies it.
        if enabled && !self.dragging {
            self.value = value;
        }
    }

    fn preview(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let bounds = self.bounds.get();
        self.value = strength_at_position((x - bounds.left()).into(), bounds.size.width.into());
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        self.dragging = false;
        if self.enabled && (self.value - self.saved_value).abs() > f64::EPSILON {
            cx.emit(StrengthChanged(self.value));
        } else {
            self.value = self.saved_value;
        }
        cx.notify();
    }
}

impl Render for StrengthSlider {
    #[allow(clippy::too_many_lines)]
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let bounds = Rc::clone(&self.bounds);
        let slider = cx.entity().downgrade();
        #[allow(clippy::cast_possible_truncation)]
        let fraction = self.value as f32;

        div()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_lg()
            .p_3()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Strength"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(p.muted))
                            .child(format!("{:.0}%", self.value * 100.0)),
                    ),
            )
            .child(
                div()
                    .id("strength-slider")
                    .track_focus(&self.focus)
                    .tab_stop(self.enabled)
                    .relative()
                    .h(px(36.0))
                    .w_full()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .focus(|style| style.border_color(rgb(p.accent)))
                    .when(!self.enabled, |slider| slider.opacity(0.45))
                    .when(self.enabled, |slider| {
                        slider
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|slider, event: &MouseDownEvent, window, cx| {
                                    slider.focus.focus(window);
                                    slider.dragging = true;
                                    slider.preview(event.position.x, cx);
                                    cx.stop_propagation();
                                }),
                            )
                            .on_key_down(cx.listener(
                                |slider, event: &gpui::KeyDownEvent, window, cx| {
                                    if event.keystroke.key == "tab" {
                                        if event.keystroke.modifiers.shift {
                                            window.focus_prev();
                                        } else {
                                            window.focus_next();
                                        }
                                        cx.stop_propagation();
                                    } else if let Some(value) = strength_for_key(
                                        slider.value,
                                        &event.keystroke.key,
                                        event.keystroke.modifiers.shift,
                                    ) {
                                        slider.value = value;
                                        slider.commit(cx);
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                    })
                    .child(
                        div()
                            .absolute()
                            .left(px(THUMB_RADIUS))
                            .right(px(THUMB_RADIUS))
                            .top(px(15.0))
                            .h(px(4.0))
                            .rounded_full()
                            .bg(rgb(p.faint))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(fraction))
                                    .rounded_full()
                                    .bg(rgb(p.accent)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(relative(fraction))
                                    .ml(px(-THUMB_RADIUS))
                                    .top(px(2.0 - THUMB_RADIUS))
                                    .size(px(THUMB_RADIUS * 2.0))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(rgb(p.surface))
                                    .bg(rgb(p.text)),
                            ),
                    )
                    .child(
                        canvas(
                            move |layout, _, _| bounds.set(layout),
                            move |_, (), window, _| {
                                let moving = slider.clone();
                                window.on_mouse_event(
                                    move |event: &MouseMoveEvent, phase, _, cx| {
                                        if phase == DispatchPhase::Bubble {
                                            let _ = moving.update(cx, |slider, cx| {
                                                if slider.dragging {
                                                    slider.preview(event.position.x, cx);
                                                    cx.stop_propagation();
                                                }
                                            });
                                        }
                                    },
                                );
                                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                                    if phase == DispatchPhase::Bubble
                                        && event.button == MouseButton::Left
                                    {
                                        let _ = slider.update(cx, |slider, cx| {
                                            if slider.dragging {
                                                slider.preview(event.position.x, cx);
                                                slider.commit(cx);
                                                cx.stop_propagation();
                                            }
                                        });
                                    }
                                });
                            },
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_xs()
                    .text_color(rgb(p.muted))
                    .child("0% · Less suppression")
                    .child("More suppression · 100%"),
            )
    }
}

fn strength_at_position(x: f32, width: f32) -> f64 {
    let travel = width - THUMB_RADIUS * 2.0;
    if travel <= 0.0 {
        return 0.0;
    }
    let fraction = f64::from(((x - THUMB_RADIUS) / travel).clamp(0.0, 1.0));
    (fraction * 100.0).round() / 100.0
}

fn strength_for_key(value: f64, key: &str, shift: bool) -> Option<f64> {
    let step = if shift { 0.1 } else { 0.01 };
    let next = match key {
        "left" | "down" => value - step,
        "right" | "up" => value + step,
        "pageup" => value + 0.1,
        "pagedown" => value - 0.1,
        "home" => 0.0,
        "end" => 1.0,
        _ => return None,
    };
    Some((next.clamp(0.0, 1.0) * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::{strength_at_position, strength_for_key};

    #[test]
    fn pointer_positions_use_thumb_centers_and_clamp_outside_the_track() {
        for (x, expected) in [
            (-20.0, 0.0),
            (8.0, 0.0),
            (50.0, 0.42),
            (58.0, 0.5),
            (108.0, 1.0),
            (130.0, 1.0),
        ] {
            assert!((strength_at_position(x, 116.0) - expected).abs() < f64::EPSILON);
        }
        assert!(strength_at_position(8.0, 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn keyboard_adjustments_support_fine_steps_coarse_steps_and_endpoints() {
        for (value, key, shift, expected) in [
            (0.55, "right", false, 0.56),
            (0.55, "left", false, 0.54),
            (0.55, "up", true, 0.65),
            (0.55, "pagedown", false, 0.45),
            (0.0, "left", false, 0.0),
            (1.0, "right", false, 1.0),
            (0.55, "home", false, 0.0),
            (0.55, "end", false, 1.0),
        ] {
            assert!(
                strength_for_key(value, key, shift)
                    .is_some_and(|actual| (actual - expected).abs() < f64::EPSILON)
            );
        }
        assert!(strength_for_key(0.55, "a", false).is_none());
    }
}
