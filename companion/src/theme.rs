use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Vec2, Visuals};

/// Named font family for cutesy, rounded display headings (Baloo 2), layered
/// over the accessible body copy font (Atkinson Hyperlegible).
pub fn heading_family() -> FontFamily {
    FontFamily::Name("heading".into())
}

/// A heading-styled [`egui::RichText`] using the rounded display font.
pub fn heading_text(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text).font(FontId::new(size, heading_family()))
}

/// Lovelock's signature accent: a soft, pastel bubblegum pink rather than a
/// harsh neon magenta.
pub const ACCENT: Color32 = Color32::from_rgb(255, 138, 189);
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(255, 195, 224);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(107, 60, 92);
pub const SUCCESS: Color32 = Color32::from_rgb(150, 224, 183);
pub const WARNING: Color32 = Color32::from_rgb(245, 199, 137);
pub const DANGER: Color32 = Color32::from_rgb(240, 140, 158);
pub const NEUTRAL: Color32 = Color32::from_rgb(190, 176, 190);

/// A soft dusty-plum base rather than a stark near-black, to keep the pastel
/// accents feeling gentle instead of neon.
pub const BASE: Color32 = Color32::from_rgb(30, 22, 33);
pub const PANEL: Color32 = Color32::from_rgb(38, 28, 42);
pub const CARD: Color32 = Color32::from_rgb(46, 33, 49);
pub const CARD_RAISED: Color32 = Color32::from_rgb(56, 40, 59);
/// A hairline border: quiet enough to group without drawing the eye, so the
/// content does the talking.
pub const STROKE: Color32 = Color32::from_rgb(70, 52, 72);
pub const TEXT: Color32 = Color32::from_rgb(248, 240, 245);
pub const TEXT_DIM: Color32 = Color32::from_rgb(196, 180, 194);
/// Dot color for the polka-dot background texture. Kept only a hair above
/// [`BASE`] so the pattern reads as a faint grain, never as content.
pub const DOT: Color32 = Color32::from_rgb(46, 34, 50);

pub fn apply(ctx: &Context) {
    let mut visuals = Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BASE;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(8, 5, 9);
    visuals.faint_bg_color = CARD;
    visuals.code_bg_color = CARD;
    visuals.hyperlink_color = ACCENT_BRIGHT;
    visuals.selection.bg_fill = ACCENT_DIM;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.window_stroke = Stroke::new(1.0, STROKE);

    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, STROKE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);

    visuals.widgets.inactive.bg_fill = CARD_RAISED;
    visuals.widgets.inactive.weak_bg_fill = CARD_RAISED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, STROKE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);

    visuals.widgets.hovered.bg_fill = ACCENT_DIM;
    visuals.widgets.hovered.weak_bg_fill = ACCENT_DIM;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);

    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.weak_bg_fill = ACCENT;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_BRIGHT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::ZERO;
        // Calmer pointer feel: controls don't swell on hover, they just
        // re-tint. One less thing twitching under the cursor.
        widget.expansion = 0.0;
    }

    // Square corners throughout the chrome, and drop shadows that lift
    // popovers off the page without the heavy default vignette.
    visuals.window_corner_radius = CornerRadius::ZERO;
    visuals.menu_corner_radius = CornerRadius::ZERO;
    visuals.weak_text_alpha = 0.65;
    visuals.disabled_alpha = 0.45;
    visuals.window_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(80),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(70),
    };

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, visuals);

    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(23.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(16.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(16.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(13.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(14.0, egui::FontFamily::Monospace),
        );

        // One spacing scale, applied deliberately: an 8px grid for gaps and
        // button padding, a slightly taller minimum hit target, indents that
        // line up with the card inner margin, and a slimmer scrollbar.
        let spacing = &mut style.spacing;
        spacing.item_spacing = Vec2::new(8.0, 8.0);
        spacing.button_padding = Vec2::new(12.0, 7.0);
        spacing.interact_size = Vec2::new(48.0, 26.0);
        spacing.indent = 20.0;
        spacing.window_margin = egui::Margin::same(12);
        spacing.menu_margin = egui::Margin::same(8);
        spacing.scroll = egui::style::ScrollStyle {
            bar_width: 8.0,
            ..egui::style::ScrollStyle::solid()
        };
    });
}

/// Registers Atkinson Hyperlegible (an accessibility-focused typeface designed
/// by the Braille Institute for maximum legibility) as the body UI font, Baloo
/// 2 (soft, rounded, cutesy) as the display heading font, and the Phosphor
/// icon font so `egui_phosphor::regular::*` glyphs render.
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

/// Runs `add` with sliders' rail and handle rounded back to a pill shape —
/// the one control that keeps rounded corners after the rest of the UI went
/// square, since a flat rectangular rail and handle read poorly as a slider.
/// Scoped, so it never leaks the rounding onto buttons or other widgets.
pub fn rounded_sliders(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.scope(|ui| {
        let corner_radius = CornerRadius::same(255);
        let visuals = ui.visuals_mut();
        visuals.widgets.inactive.corner_radius = corner_radius;
        visuals.widgets.hovered.corner_radius = corner_radius;
        visuals.widgets.active.corner_radius = corner_radius;
        add(ui);
    });
}

/// A rounded, tinted card frame used to group related controls.
pub fn card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::group(ui.style())
        .fill(CARD)
        .stroke(Stroke::new(1.0, STROKE))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(16.0)
}

/// A slightly brighter card with a soft pink glow, used for the currently
/// selected trigger row.
pub fn card_selected(ui: &egui::Ui) -> egui::Frame {
    card(ui)
        .fill(Color32::from_rgb(64, 42, 62))
        .stroke(Stroke::new(1.5, ACCENT))
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 14,
            spread: 0,
            color: ACCENT.gamma_multiply(0.22),
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BadgeTone {
    Neutral,
    Success,
    Warning,
    Danger,
}
impl BadgeTone {
    fn color(self) -> Color32 {
        match self {
            Self::Neutral => NEUTRAL,
            Self::Success => SUCCESS,
            Self::Warning => WARNING,
            Self::Danger => DANGER,
        }
    }
}

/// A small colored status pill, replacing plain colored status text.
pub fn badge(ui: &mut egui::Ui, text: &str, tone: BadgeTone) {
    let color = tone.color();
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.16))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(CornerRadius::ZERO)
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

/// Paints `text` in the normal body label style, shifted down by `offset`
/// pixels. The space reserved in the layout is the plain, un-shifted label
/// size, so sibling widgets in the same horizontal row lay out and
/// vertically center exactly as if this were a normal `ui.label`. Only the
/// painted glyphs move, which keeps the nudge predictable regardless of
/// what else is in the row.
pub fn label_nudged_down(ui: &mut egui::Ui, text: &str, offset: f32) -> egui::Response {
    colored_text_nudged_down(ui, text, ui.visuals().text_color(), offset)
}

/// Same as [`label_nudged_down`], but with an explicit color, used to nudge
/// icon glyphs (which are painted via `colored_label`) so they can be
/// vertically matched to an adjacent heading without disturbing layout.
pub fn colored_text_nudged_down(
    ui: &mut egui::Ui,
    text: &str,
    color: Color32,
    offset: f32,
) -> egui::Response {
    let font_id = TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font_id, color);
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    ui.painter()
        .galley(rect.left_top() + Vec2::new(0.0, offset), galley, color);
    response
}

/// A checkbox drawn as a radio-style dot instead of egui's tick-in-a-box: a
/// filled accent disc with a small inset dot when on, a hollow ring when off.
/// The whole dot-plus-label row toggles `checked`; the returned response
/// reports `.changed()` and takes `.on_hover_text(..)` like `ui.checkbox`.
pub fn dot_checkbox(ui: &mut egui::Ui, checked: &mut bool, label: &str) -> egui::Response {
    let id = ui.id().with(("dot_checkbox", label));
    let laid_out = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (dot_rect, _) =
            ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
        ui.label(egui::RichText::new(label).color(TEXT));
        dot_rect
    });
    let dot_rect = laid_out.inner;
    let mut response = ui.interact(laid_out.response.rect, id, egui::Sense::click());
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }

    let center = dot_rect.center();
    let radius = 8.0;
    let (fill, ring) = if *checked {
        (ACCENT, ACCENT_BRIGHT)
    } else if response.hovered() {
        (CARD_RAISED, ACCENT)
    } else {
        (CARD_RAISED, STROKE)
    };
    let painter = ui.painter();
    painter.circle_filled(center, radius, fill);
    painter.circle_stroke(center, radius - 0.5, Stroke::new(1.4, ring));
    if *checked {
        painter.circle_filled(center, radius * 0.34, PANEL);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A round icon badge (colored ring + centered glyph), used for trigger rows.
pub fn icon_badge(ui: &mut egui::Ui, glyph: &str, active: bool) -> egui::Response {
    icon_badge_sized(ui, glyph, active, 36.0)
}

/// A drag-grip glyph (six dots) for a reorderable row. Allocated in a
/// fixed box the same height as [`icon_badge`] and painted `CENTER_CENTER`,
/// so it lines up with the badge and stays vertically centered no matter
/// how tall the rest of the row grows.
pub fn drag_handle(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(22.0, 36.0), egui::Sense::hover());
    // Quiet at rest, accent only when the cursor is on it: a grip shouldn't
    // compete with the row's own content for attention.
    let color = if response.hovered() {
        ACCENT
    } else {
        TEXT_DIM.gamma_multiply(0.7)
    };
    ui.painter().text(
        glyph_center(rect.center(), 18.0),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::DOTS_SIX_VERTICAL,
        FontId::proportional(18.0),
        color,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    response
}

/// Same as [`icon_badge`], with an explicit diameter (used for the larger
/// effect-editor header icon).
pub fn icon_badge_sized(ui: &mut egui::Ui, glyph: &str, active: bool, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let painter = ui.painter();
    let (fill, ring, glyph_color) = if active {
        (ACCENT_DIM, ACCENT, ACCENT_BRIGHT)
    } else {
        (CARD_RAISED, STROKE, TEXT_DIM)
    };
    painter.circle_filled(rect.center(), size / 2.0, fill);
    painter.circle_stroke(rect.center(), size / 2.0 - 0.5, Stroke::new(1.4, ring));
    painter.text(
        glyph_center(rect.center(), size * 0.5),
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(size * 0.5),
        glyph_color,
    );
    if active {
        let accent_pos = rect.center() + Vec2::splat(size * 0.30);
        let accent_size = (size * 0.22).max(6.5);
        painter.circle_filled(accent_pos, accent_size, PANEL);
        painter.text(
            glyph_center(accent_pos, accent_size),
            egui::Align2::CENTER_CENTER,
            egui_phosphor::regular::HEART_STRAIGHT,
            FontId::proportional(accent_size),
            ACCENT_BRIGHT,
        );
    }
    response
}

/// Nudges a glyph's anchor point down slightly to compensate for
/// [`egui::Painter::text`]'s `CENTER_CENTER` anchor using the font's full
/// ascent+descent row height rather than the glyph's visual ink bounds:
/// icon fonts otherwise render a few pixels above where they visually look
/// centered against a circle or line drawn around the same point.
fn glyph_center(point: egui::Pos2, font_size: f32) -> egui::Pos2 {
    point + Vec2::new(0.0, font_size * 0.07)
}

/// Small decorative heart glyph, used as a section-header bullet.
pub fn heart_bullet(ui: &mut egui::Ui) {
    ui.colored_label(ACCENT, egui_phosphor::regular::HEART);
}

/// Paints a soft, evenly-spaced polka-dot texture across `ui`'s current
/// rect, purely decorative and drawn behind whatever is added afterward.
pub fn paint_dotted_background(ui: &egui::Ui) {
    let rect = ui.max_rect();
    let spacing = 28.0;
    let radius = 1.3;
    let painter = ui.painter();
    let start_x = (rect.left() / spacing).floor() * spacing;
    let start_y = (rect.top() / spacing).floor() * spacing;
    let mut y = start_y;
    let mut row = 0_i32;
    while y < rect.bottom() {
        let offset = if row % 2 == 0 { 0.0 } else { spacing / 2.0 };
        let mut x = start_x + offset;
        while x < rect.right() {
            painter.circle_filled(egui::pos2(x, y), radius, DOT);
            x += spacing;
        }
        y += spacing;
        row += 1;
    }
}

/// A cute divider: a soft pink line fading from both edges into a small heart
/// at its center. Used under section headings for a bit of flourish.
pub fn flourish(ui: &mut egui::Ui) {
    let height = 14.0;
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let mid = rect.center();
    let gap = 12.0;
    let segments = 24;
    for side in [-1.0_f32, 1.0] {
        for i in 0..segments {
            let t0 = i as f32 / segments as f32;
            let t1 = (i + 1) as f32 / segments as f32;
            // Ease the fade so the line dissolves smoothly toward the edges
            // instead of stepping down linearly.
            let fade = (1.0 - t1).max(0.0).powf(1.7);
            let x0 = mid.x + side * (gap + t0 * (rect.width() / 2.0 - gap));
            let x1 = mid.x + side * (gap + t1 * (rect.width() / 2.0 - gap));
            painter.line_segment(
                [egui::pos2(x0, mid.y), egui::pos2(x1, mid.y)],
                Stroke::new(0.9, ACCENT.gamma_multiply(fade * 0.55)),
            );
        }
    }
    painter.text(
        glyph_center(mid, 11.0),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::HEART_STRAIGHT,
        FontId::proportional(11.0),
        ACCENT_BRIGHT,
    );
}

/// A faint sparkle accent glyph, for a touch of whimsy near headings.
pub fn sparkle(ui: &mut egui::Ui, size: f32) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(egui_phosphor::regular::SPARKLE)
                .size(size)
                .color(ACCENT_BRIGHT.gamma_multiply(0.65)),
        )
        .selectable(false),
    );
}
