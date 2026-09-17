use crate::action::{
    MAX_VIBRATE_DURATION, MAX_VIBRATE_STRENGTH, MIN_VIBRATE_STRENGTH, VibrateActionSettings,
    VibrateMode, duration_steps,
};
use crate::theme::ACCENT;
use egui::{Color32, Ui};
use std::ops::RangeInclusive;

pub fn draw_vibrate_settings_editor(ui: &mut Ui, vibrate: &mut VibrateActionSettings) {
    ui.horizontal(|ui| {
        crate::theme::label_nudged_down(ui, "Mode", 10.0);
        egui::ComboBox::from_id_salt("vibrate-mode")
            .selected_text(vibrate.mode.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut vibrate.mode, VibrateMode::Interval, "Random");
                ui.selectable_value(&mut vibrate.mode, VibrateMode::Fixed, "Fixed");
            });
    });
    ui.add_space(4.0);
    match vibrate.mode {
        VibrateMode::Interval => {
            slider_input(
                ui,
                "Minimum strength",
                &mut vibrate.interval.minimum_strength,
                MIN_VIBRATE_STRENGTH..=MAX_VIBRATE_STRENGTH,
                1.0,
                "",
            );
            slider_input(
                ui,
                "Maximum strength",
                &mut vibrate.interval.maximum_strength,
                MIN_VIBRATE_STRENGTH..=MAX_VIBRATE_STRENGTH,
                1.0,
                "",
            );
            duration_slider(ui, "Minimum duration", &mut vibrate.interval.minimum_duration_seconds);
            duration_slider(ui, "Maximum duration", &mut vibrate.interval.maximum_duration_seconds);
        }
        VibrateMode::Fixed => {
            slider_input(
                ui,
                "Strength",
                &mut vibrate.fixed.strength,
                MIN_VIBRATE_STRENGTH..=MAX_VIBRATE_STRENGTH,
                1.0,
                "",
            );
            duration_slider(ui, "Duration", &mut vibrate.fixed.duration_seconds);
        }
    }
    if vibrate.interval.minimum_strength > vibrate.interval.maximum_strength {
        vibrate.interval.maximum_strength = vibrate.interval.minimum_strength;
    }
    if vibrate.interval.minimum_duration_seconds > vibrate.interval.maximum_duration_seconds {
        vibrate.interval.maximum_duration_seconds = vibrate.interval.minimum_duration_seconds;
    }
}

/// A slider over the fixed duration grid (quarter seconds up to a second, then
/// whole seconds). The slider moves in even steps across the grid indices
/// rather than over raw seconds, since a duration resolves onto that exact grid
/// anyway - see [`crate::action::duration_steps`].
fn duration_slider(ui: &mut Ui, label: &str, value: &mut f32) {
    let steps = duration_steps();
    let last_index = steps.len().saturating_sub(1) as i32;
    let mut index = steps
        .iter()
        .position(|step| (*step - *value).abs() < 0.01)
        .unwrap_or_else(|| {
            steps
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| (**a - *value).abs().total_cmp(&(**b - *value).abs()))
                .map(|(index, _)| index)
                .unwrap_or(0)
        }) as i32;
    ui.label(label);
    ui.horizontal(|ui| {
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = ui.available_width() * 0.62;
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = input_background();
            visuals.widgets.hovered.bg_fill = input_background();
            visuals.widgets.active.bg_fill = input_background();
            visuals.selection.bg_fill = ACCENT;
            // Sliders are the one control that keeps its rounded, pill-shaped
            // rail and handle after the rest of the UI went square.
            let corner_radius = egui::CornerRadius::same(255);
            visuals.widgets.inactive.corner_radius = corner_radius;
            visuals.widgets.hovered.corner_radius = corner_radius;
            visuals.widgets.active.corner_radius = corner_radius;
            ui.add(
                egui::Slider::new(&mut index, 0..=last_index)
                    .show_value(false)
                    .trailing_fill(true),
            );
        });
        let seconds = steps[index.clamp(0, last_index) as usize];
        ui.weak(format!(
            "{} s / {:.0} s",
            crate::action::format_seconds_value(seconds),
            MAX_VIBRATE_DURATION
        ));
    });
    *value = steps[index.clamp(0, last_index) as usize];
    ui.add_space(4.0);
}

fn input_background() -> Color32 {
    Color32::from_rgb(42, 30, 40)
}
fn slider_input(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    step: f64,
    unit: &str,
) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = ui.available_width() * 0.62;
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = input_background();
            visuals.widgets.hovered.bg_fill = input_background();
            visuals.widgets.active.bg_fill = input_background();
            visuals.selection.bg_fill = ACCENT;
            let corner_radius = egui::CornerRadius::same(255);
            visuals.widgets.inactive.corner_radius = corner_radius;
            visuals.widgets.hovered.corner_radius = corner_radius;
            visuals.widgets.active.corner_radius = corner_radius;
            ui.add(
                egui::Slider::new(value, range.clone())
                    .step_by(step)
                    .show_value(false)
                    .trailing_fill(true),
            );
        });
        ui.weak(format!("{:.2}{unit} / {:.0}{unit}", *value, range.end()));
    });
    ui.add_space(4.0);
}
