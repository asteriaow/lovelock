//! Design tokens and small reusable widgets for the Lovelock Companion UI.
//!
//! This is the single source of truth for color, spacing, radius and
//! typography. Screens should build out of these tokens and helpers rather
//! than hand-rolled `Color32`/margin/radius literals, so the app reads as one
//! deliberately designed surface instead of a pile of one-off panels.

use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Vec2, Visuals};

// ---------------------------------------------------------------------------
// Color tokens
// ---------------------------------------------------------------------------

/// Base window background: a very dark plum, never a stark near-black.
pub const APP_BG: Color32 = Color32::from_rgb(0x17, 0x12, 0x1B);
/// Sidebar / title bar: one step lighter than the app background so the
/// chrome reads as a distinct layer without competing with content.
pub const SIDEBAR_BG: Color32 = Color32::from_rgb(0x1D, 0x16, 0x22);
/// Elevated surface: cards, rows, inputs.
pub const SURFACE_1: Color32 = Color32::from_rgb(0x24, 0x1A, 0x29);
/// Higher surface: the active/selected state of a `SURFACE_1` element.
pub const SURFACE_2: Color32 = Color32::from_rgb(0x2B, 0x20, 0x30);
/// Hover state for an otherwise `SURFACE_1` row.
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x2A, 0x1F, 0x2F);
/// Selected-row tint: a quiet pink-stained surface, not a full accent fill.
pub const SURFACE_SELECTED: Color32 = Color32::from_rgb(0x33, 0x22, 0x35);
/// Text input fill: a touch darker than `APP_BG`, so a field reads as a
/// recessed slot in the page rather than a raised surface like a card.
pub const INPUT_BG: Color32 = Color32::from_rgb(0x12, 0x0E, 0x16);

pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(0x3A, 0x2D, 0x40);
pub const BORDER_ACTIVE: Color32 = ACCENT_PINK;
/// A softer pink than `BORDER_ACTIVE`, used to mark an "on" state (an
/// enabled trigger row) - visually related to the selected state's pink
/// border, but dimmer so the two stay distinguishable.
pub const BORDER_ENABLED: Color32 = Color32::from_rgb(0xC7, 0x6E, 0x9C);

pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xF6, 0xEF, 0xF6);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xBE, 0xB0, 0xC1);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x88, 0x7B, 0x8C);

/// Lovelock's signature accent: a soft bubblegum pink.
pub const ACCENT_PINK: Color32 = Color32::from_rgb(0xF0, 0x6B, 0xA6);
pub const ACCENT_PINK_HOVER: Color32 = Color32::from_rgb(0xF5, 0x8B, 0xBB);
/// Brighter action pink, reserved for primary buttons and the emergency stop.
pub const ACCENT_PINK_BRIGHT: Color32 = Color32::from_rgb(0xF6, 0x4B, 0x9B);
/// Pale secondary accent, used sparingly for small highlights/checkmarks.
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0xFF, 0xC3, 0xE0);

pub const SUCCESS: Color32 = Color32::from_rgb(0x96, 0xE0, 0xB7);
pub const WARNING: Color32 = Color32::from_rgb(0xF5, 0xC7, 0x89);
pub const DANGER: Color32 = Color32::from_rgb(0xF0, 0x8C, 0x9E);
pub const NEUTRAL: Color32 = Color32::from_rgb(0xBE, 0xB0, 0xC1);

// ---------------------------------------------------------------------------
// Spacing scale
// ---------------------------------------------------------------------------

pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;
pub const SPACE_XL: f32 = 24.0;
pub const SPACE_XXL: f32 = 32.0;

/// Main page padding.
pub const PAGE_PADDING: f32 = 20.0;
/// Sidebar inner padding.
pub const SIDEBAR_PADDING: f32 = 12.0;
/// Row/card inner padding.
pub const ROW_PADDING: f32 = 14.0;

// ---------------------------------------------------------------------------
// Corner radii
// ---------------------------------------------------------------------------

pub const RADIUS_XS: u8 = 4;
pub const RADIUS_SM: u8 = 6;
pub const RADIUS_MD: u8 = 8;
pub const RADIUS_LG: u8 = 10;
pub const RADIUS_XL: u8 = 12;
pub const RADIUS_PILL: u8 = 255;

// ---------------------------------------------------------------------------
// Typography
// ---------------------------------------------------------------------------

/// Named font family for the rounded display headings (Baloo 2), layered
/// over the accessible Atkinson Hyperlegible body font.
pub fn heading_family() -> FontFamily {
    FontFamily::Name("heading".into())
}

/// A heading-styled [`egui::RichText`] using the rounded display font.
pub fn heading_text(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text).font(FontId::new(size, heading_family()))
}

/// Page title size (26-30px), set with [`heading_text`].
pub const SIZE_PAGE_TITLE: f32 = 27.0;
/// Section heading size (17-20px), set with [`heading_text`].
pub const SIZE_SECTION_TITLE: f32 = 18.0;
/// Control label size.
pub const SIZE_CONTROL_LABEL: f32 = 14.0;
/// Body text size.
pub const SIZE_BODY: f32 = 13.5;
/// Secondary/meta text size.
pub const SIZE_META: f32 = 12.5;

// ---------------------------------------------------------------------------
// Global style
// ---------------------------------------------------------------------------

pub fn apply(ctx: &Context) {
    let mut visuals = Visuals::dark();
    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.panel_fill = APP_BG;
    visuals.window_fill = SIDEBAR_BG;
    visuals.extreme_bg_color = Color32::from_rgb(0x0E, 0x0B, 0x11);
    visuals.faint_bg_color = SURFACE_1;
    visuals.code_bg_color = SURFACE_1;
    visuals.hyperlink_color = ACCENT_PINK_HOVER;
    visuals.selection.bg_fill = ACCENT_PINK.gamma_multiply(0.28);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT_PINK);
    visuals.window_stroke = Stroke::new(1.0, BORDER_SUBTLE);

    visuals.widgets.noninteractive.bg_fill = SIDEBAR_BG;
    visuals.widgets.noninteractive.weak_bg_fill = SIDEBAR_BG;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    visuals.widgets.inactive.bg_fill = SURFACE_1;
    visuals.widgets.inactive.weak_bg_fill = SURFACE_1;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
    visuals.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_PINK.gamma_multiply(0.7));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);

    visuals.widgets.active.bg_fill = SURFACE_2;
    visuals.widgets.active.weak_bg_fill = SURFACE_2;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_PINK);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(RADIUS_SM);
        widget.expansion = 0.0;
    }

    visuals.window_corner_radius = CornerRadius::same(RADIUS_LG);
    visuals.menu_corner_radius = CornerRadius::same(RADIUS_MD);
    visuals.weak_text_alpha = 0.65;
    visuals.disabled_alpha = 0.5;
    visuals.window_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(80),
    };

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, visuals);

    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(SIZE_SECTION_TITLE, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(SIZE_BODY, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(SIZE_CONTROL_LABEL, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(SIZE_META, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(13.0, egui::FontFamily::Monospace),
        );

        let spacing = &mut style.spacing;
        spacing.item_spacing = Vec2::new(SPACE_SM, SPACE_SM);
        spacing.button_padding = Vec2::new(SPACE_MD, 7.0);
        spacing.interact_size = Vec2::new(44.0, 24.0);
        spacing.indent = 18.0;
        spacing.window_margin = egui::Margin::same(12);
        spacing.menu_margin = egui::Margin::same(8);
        spacing.scroll = egui::style::ScrollStyle {
            bar_width: 8.0,
            ..egui::style::ScrollStyle::solid()
        };
    });
}

/// Registers Atkinson Hyperlegible as the body UI font, Baloo 2 as the
/// display heading font, and the Phosphor icon font so
/// `egui_phosphor::regular::*` glyphs render.
pub fn install_fonts(ctx: &Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "atkinson-hyperlegible".into(),
        egui::FontData::from_static(include_bytes!(
            "../assets/fonts/AtkinsonHyperlegible-Regular.ttf"
        ))
        .into(),
    );
    fonts.font_data.insert(
        "baloo2".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/Baloo2-Variable.ttf")).into(),
    );
    if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        proportional.insert(0, "atkinson-hyperlegible".into());
    }
    fonts
        .families
        .insert(heading_family(), vec!["baloo2".into()]);
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// The standard elevated surface frame used for cards and rows.
pub fn surface(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::group(ui.style())
        .fill(SURFACE_1)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(RADIUS_XS))
        .inner_margin(ROW_PADDING)
}

/// Same as [`surface`], with a lighter border to mark an "on" state (e.g. an
/// enabled-but-not-selected trigger row) without going as loud as
/// [`surface_selected`]'s pink.
pub fn surface_enabled(ui: &egui::Ui) -> egui::Frame {
    surface(ui).stroke(Stroke::new(1.2, BORDER_ENABLED))
}

/// Same as [`surface`], tinted and outlined in pink for the active/selected
/// state of a row.
pub fn surface_selected(ui: &egui::Ui) -> egui::Frame {
    surface(ui)
        .fill(SURFACE_SELECTED)
        .stroke(Stroke::new(1.3, ACCENT_PINK))
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// Bright pink filled button, for the single most important action on a
/// page (e.g. "+ New profile").
pub fn button_primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let button = egui::Button::new(egui::RichText::new(text).color(Color32::WHITE))
        .fill(ACCENT_PINK_BRIGHT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(RADIUS_SM));
    let response = ui.add(button);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Dark surface button with a subtle border, for secondary actions (Copy,
/// Connect, etc.).
pub fn button_secondary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let button = egui::Button::new(egui::RichText::new(text).color(TEXT_PRIMARY))
        .fill(SURFACE_1)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(RADIUS_SM));
    let response = ui.add(button);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Transparent button for icon buttons, overflow menus and minor controls.
pub fn button_ghost(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let button = egui::Button::new(egui::RichText::new(text).color(TEXT_SECONDARY))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(RADIUS_SM));
    let response = ui.add(button);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Strong pink/red-pink button, reserved specifically for destructive or
/// emergency actions.
pub fn button_danger(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let button = egui::Button::new(egui::RichText::new(text).color(Color32::WHITE))
        .fill(DANGER.gamma_multiply(0.9))
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(RADIUS_SM));
    let response = ui.add(button);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

// ---------------------------------------------------------------------------
// Toggles & checkboxes
// ---------------------------------------------------------------------------

/// A compact pill toggle switch: pink track + white thumb when on, muted
/// purple-gray track + muted thumb when off.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(38.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let track_color = if *on {
        ACCENT_PINK
    } else if response.hovered() {
        BORDER_SUBTLE.gamma_multiply(1.4)
    } else {
        BORDER_SUBTLE
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(RADIUS_PILL), track_color);

    let radius = rect.height() / 2.0 - 3.0;
    let thumb_x = if *on {
        rect.right() - radius - 3.0
    } else {
        rect.left() + radius + 3.0
    };
    let thumb_center = egui::pos2(thumb_x, rect.center().y);
    let thumb_color = if *on { Color32::WHITE } else { TEXT_SECONDARY };
    painter.circle_filled(thumb_center, radius, thumb_color);

    response
}

/// A proper square checkbox for a boolean setting, with an optional
/// secondary description line beneath the label explaining its behavior.
/// Returns the checkbox's own response (`.changed()` reports a flip).
pub fn checkbox_row(
    ui: &mut egui::Ui,
    checked: &mut bool,
    label: &str,
    description: Option<&str>,
) -> egui::Response {
    let id = ui.id().with(("checkbox_row", label));
    let box_size = 18.0;
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = SPACE_MD;
        let (box_rect, _) = ui.allocate_exact_size(Vec2::splat(box_size), egui::Sense::hover());
        ui.vertical(|ui| {
            ui.add(egui::Label::new(
                egui::RichText::new(label)
                    .size(SIZE_CONTROL_LABEL)
                    .color(TEXT_PRIMARY),
            ));
            if let Some(description) = description {
                ui.add(egui::Label::new(
                    egui::RichText::new(description)
                        .size(SIZE_META)
                        .color(TEXT_MUTED),
                ));
            }
        });
        box_rect
    });
    let box_rect = row.inner;
    let mut response = ui.interact(row.response.rect, id, egui::Sense::click());
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let painter = ui.painter();
    let radius = CornerRadius::same(4);
    if *checked {
        painter.rect_filled(box_rect, radius, ACCENT_PINK);
        let stroke = Stroke::new(1.8, Color32::from_rgb(0x24, 0x14, 0x1E));
        let p1 = box_rect.left_center() + Vec2::new(box_size * 0.22, box_size * 0.02);
        let p2 = box_rect.center() + Vec2::new(-box_size * 0.05, box_size * 0.22);
        let p3 = box_rect.right_top() + Vec2::new(-box_size * 0.18, box_size * 0.22);
        painter.line_segment([p1, p2], stroke);
        painter.line_segment([p2, p3], stroke);
    } else {
        let fill = if response.hovered() {
            SURFACE_HOVER
        } else {
            SURFACE_1
        };
        painter.rect_filled(box_rect, radius, fill);
        painter.rect_stroke(
            box_rect,
            radius,
            Stroke::new(1.2, BORDER_SUBTLE),
            egui::StrokeKind::Inside,
        );
    }

    response
}

// ---------------------------------------------------------------------------
// Sliders
// ---------------------------------------------------------------------------

/// A labeled slider matching the redesign spec: label above, a thick pink
/// fill/track slider, and the current value in a fixed-width column on the
/// right so values scan in a straight vertical line.
pub fn labeled_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    value_text: impl Fn(f32) -> String,
) -> egui::Response {
    ui.add(egui::Label::new(
        egui::RichText::new(label)
            .size(SIZE_CONTROL_LABEL)
            .color(TEXT_PRIMARY),
    ));
    ui.add_space(2.0);
    let response = ui
        .horizontal(|ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 92.0).max(80.0);
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = SURFACE_2;
            visuals.widgets.hovered.bg_fill = SURFACE_2;
            visuals.widgets.active.bg_fill = SURFACE_2;
            visuals.selection.bg_fill = ACCENT_PINK;
            // NOT `RADIUS_PILL`: egui's slider trailing-fill math adds this
            // corner radius as a literal pixel offset past the handle, so a
            // 255 "full pill" radius here would overfill the rail by 255px.
            // The rail is thin enough that a small radius still reads as a
            // fully rounded pill.
            let corner_radius = CornerRadius::same(RADIUS_SM);
            visuals.widgets.inactive.corner_radius = corner_radius;
            visuals.widgets.hovered.corner_radius = corner_radius;
            visuals.widgets.active.corner_radius = corner_radius;
            let response = ui.add(
                egui::Slider::new(value, range)
                    .step_by(step)
                    .show_value(false)
                    .trailing_fill(true),
            );
            ui.add_sized(
                Vec2::new(80.0, 0.0),
                egui::Label::new(
                    egui::RichText::new(value_text(*value))
                        .size(SIZE_META)
                        .color(TEXT_SECONDARY),
                ),
            );
            response
        })
        .inner;
    ui.add_space(SPACE_XS);
    response
}

/// A drag-grip glyph (six dots) for a reorderable row, meant to be passed as
/// the `drag_body` to `ui.dnd_drag_source`. Allocated in a fixed box so it
/// lines up with `icon_badge` and stays vertically centered regardless of
/// the row's height. Quiet at rest, accent only on hover.
pub fn drag_handle(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(20.0, 34.0), egui::Sense::hover());
    let color = if response.hovered() {
        ACCENT_PINK
    } else {
        TEXT_MUTED
    };
    ui.painter().text(
        glyph_center(rect.center(), 16.0),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::DOTS_SIX_VERTICAL,
        FontId::proportional(16.0),
        color,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    response
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// A section heading (17-20px, display font) with a thin divider beneath it,
/// used to separate groups of controls inside the inspector without wrapping
/// every group in its own bordered card.
pub fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(
        heading_text(text, SIZE_SECTION_TITLE).color(TEXT_PRIMARY),
    ));
}

/// A thin horizontal divider, used between inspector sections instead of a
/// full bordered card per section.
pub fn divider(ui: &mut egui::Ui) {
    ui.add_space(SPACE_SM);
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, BORDER_SUBTLE),
    );
    ui.add_space(SPACE_SM);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BadgeTone {
    Neutral,
    Success,
    Warning,
    Danger,
}
impl BadgeTone {
    pub fn color(self) -> Color32 {
        match self {
            Self::Neutral => NEUTRAL,
            Self::Success => SUCCESS,
            Self::Warning => WARNING,
            Self::Danger => DANGER,
        }
    }
}

/// A small colored status pill.
pub fn badge(ui: &mut egui::Ui, text: &str, tone: BadgeTone) {
    let color = tone.color();
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.16))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(CornerRadius::same(RADIUS_PILL))
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(6.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 3.0, color);
                ui.colored_label(color, text);
            });
        });
}

/// A plain neutral pill, used for the version tag in the title bar.
pub fn pill(ui: &mut egui::Ui, text: &str) {
    egui::Frame::NONE
        .fill(SURFACE_1)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(RADIUS_PILL))
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.add(egui::Label::new(
                egui::RichText::new(text)
                    .size(SIZE_META)
                    .color(TEXT_SECONDARY),
            ));
        });
}

/// Nudges a glyph's anchor point down slightly to compensate for
/// [`egui::Painter::text`]'s `CENTER_CENTER` anchor using the font's full
/// ascent+descent row height rather than the glyph's visual ink bounds.
pub fn glyph_center(point: egui::Pos2, font_size: f32) -> egui::Pos2 {
    point + Vec2::new(0.0, font_size * 0.07)
}

/// A round icon container's tone: dim/neutral when off, a soft pink when
/// the trigger is simply on, and a brighter, more saturated pink when it is
/// also the one currently selected - distinct states that still read as one
/// family (both "on" tones are pink).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IconTone {
    Off,
    On,
    Selected,
}

/// A round icon container (colored ring + centered glyph), used for trigger
/// rows and the inspector header.
pub fn icon_badge(ui: &mut egui::Ui, glyph: &str, tone: IconTone, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let painter = ui.painter();
    let (fill, ring, glyph_color) = match tone {
        IconTone::Selected => (ACCENT_PINK.gamma_multiply(0.28), ACCENT_PINK, ACCENT_SOFT),
        IconTone::On => (
            ACCENT_PINK.gamma_multiply(0.16),
            ACCENT_PINK.gamma_multiply(0.75),
            ACCENT_PINK.gamma_multiply(0.9),
        ),
        IconTone::Off => (SURFACE_2, BORDER_SUBTLE, TEXT_MUTED),
    };
    painter.circle_filled(rect.center(), size / 2.0, fill);
    painter.circle_stroke(rect.center(), size / 2.0 - 0.5, Stroke::new(1.2, ring));
    painter.text(
        glyph_center(rect.center(), size * 0.5),
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(size * 0.46),
        glyph_color,
    );
    response
}
