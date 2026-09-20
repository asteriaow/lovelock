//! The vibration settings editor (mode + strength/duration sliders) shared by
//! every trigger's inspector panel.

use crate::action::{
    MAX_VIBRATE_DURATION, MAX_VIBRATE_STRENGTH, MIN_VIBRATE_STRENGTH, VibrateActionSettings,
    VibrateMode, duration_steps,
};
use crate::theme;
use egui::Ui;

pub fn draw_vibrate_settings_editor(ui: &mut Ui, vibrate: &mut VibrateActionSettings) {
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(
            egui::RichText::new("Mode")
                .size(theme::SIZE_CONTROL_LABEL)
                .color(theme::TEXT_PRIMARY),
        ));
        egui::ComboBox::from_id_salt("vibrate-mode")
            .selected_text(vibrate.mode.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut vibrate.mode, VibrateMode::Interval, "Random");
                ui.selectable_value(&mut vibrate.mode, VibrateMode::Fixed, "Fixed");
            });
    });
    ui.add_space(theme::SPACE_SM);
    match vibrate.mode {
        VibrateMode::Interval => {
            theme::labeled_slider(
                ui,
                "Minimum strength",
                &mut vibrate.interval.minimum_strength,
                MIN_VIBRATE_STRENGTH..=MAX_VIBRATE_STRENGTH,
                1.0,
                |v| format!("{v:.0} / {MAX_VIBRATE_STRENGTH:.0}"),
            );
            theme::labeled_slider(
                ui,
                "Maximum strength",
                &mut vibrate.interval.maximum_strength,
                MIN_VIBRATE_STRENGTH..=MAX_VIBRATE_STRENGTH,
                1.0,
                |v| format!("{v:.0} / {MAX_VIBRATE_STRENGTH:.0}"),
            );
            duration_slider(
                ui,
                "Minimum duration",
                &mut vibrate.interval.minimum_duration_seconds,
            );
            duration_slider(
                ui,
                "Maximum duration",
                &mut vibrate.interval.maximum_duration_seconds,
            );
        }
        VibrateMode::Fixed => {
            theme::labeled_slider(
                ui,
                "Strength",
                &mut vibrate.fixed.strength,
                MIN_VIBRATE_STRENGTH..=MAX_VIBRATE_STRENGTH,
                1.0,
                |v| format!("{v:.0} / {MAX_VIBRATE_STRENGTH:.0}"),
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

    ui.add(egui::Label::new(
        egui::RichText::new(label)
            .size(theme::SIZE_CONTROL_LABEL)
            .color(theme::TEXT_PRIMARY),
    ));
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 92.0).max(80.0);
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.bg_fill = theme::SURFACE_2;
        visuals.widgets.hovered.bg_fill = theme::SURFACE_2;
        visuals.widgets.active.bg_fill = theme::SURFACE_2;
        visuals.selection.bg_fill = theme::ACCENT_PINK;
        // See the comment in `theme::labeled_slider`: this must stay a small
        // radius, not `RADIUS_PILL` - egui's trailing-fill math uses it as a
        // literal pixel offset past the handle.
        let corner_radius = egui::CornerRadius::same(theme::RADIUS_SM);
        visuals.widgets.inactive.corner_radius = corner_radius;
        visuals.widgets.hovered.corner_radius = corner_radius;
        visuals.widgets.active.corner_radius = corner_radius;
        ui.add(
            egui::Slider::new(&mut index, 0..=last_index)
                .show_value(false)
                .trailing_fill(true),
        );
        let seconds = steps[index.clamp(0, last_index) as usize];
        ui.add_sized(
            egui::Vec2::new(80.0, 0.0),
            egui::Label::new(
                egui::RichText::new(format!(
                    "{} / {:.0} s",
                    crate::action::format_seconds_value(seconds),
                    MAX_VIBRATE_DURATION
                ))
                .size(theme::SIZE_META)
                .color(theme::TEXT_SECONDARY),
            ),
        );
    });
    *value = steps[index.clamp(0, last_index) as usize];
    ui.add_space(theme::SPACE_XS);
}
