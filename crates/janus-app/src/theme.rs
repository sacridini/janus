//! The interface's look: Dear ImGui's dark style (the C++'s default theme, theme.cpp) in egui's terms, and ImGui's
//! ProggyClean at 13 px. egui has fewer colours than ImGui's ~55 slots, so the match is close, not exact
//! (RUST_MIGRATION.md, "Aparência"). The other six themes come in phase 4.

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{Color32, FontData, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};

/// An ImGui colour (floats, alpha not premultiplied).
fn im(r: f32, g: f32, b: f32, a: f32) -> Color32 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Color32::from_rgba_unmultiplied(c(r), c(g), c(b), c(a))
}

/// The colour of the cursor's series and marks (the C++'s theme status colour).
pub fn cursor_series() -> Color32 {
    im(1.0, 0.85, 0.2, 1.0)
}

/// Colours of the pins, in order (ImPlot's "Deep" after the cursor's).
pub fn pin_color(i: usize) -> Color32 {
    let keys = janus_gpu::colormaps::all()[0].keys().collect::<Vec<_>>();
    let c = keys[i % keys.len()];
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A warning in the panels (the dark theme's status colour, theme.cpp).
pub fn warning() -> Color32 {
    im(1.0, 0.7, 0.4, 1.0)
}

pub fn weak_text() -> Color32 {
    im(0.50, 0.50, 0.50, 1.0)
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    let window = im(0.06, 0.06, 0.06, 1.0);
    let frame = im(0.16, 0.29, 0.48, 0.54);
    let border = im(0.43, 0.43, 0.50, 0.50);
    let button = im(0.26, 0.59, 0.98, 0.40);
    let hovered = im(0.26, 0.59, 0.98, 1.0);
    let active = im(0.06, 0.53, 0.98, 1.0);
    let text = Color32::WHITE;
    v.override_text_color = Some(text);
    v.panel_fill = window;
    v.window_fill = im(0.08, 0.08, 0.08, 0.94);
    v.window_stroke = Stroke::new(1.0, border);
    // text fields and the charts' background (ImPlot's dark PlotBg: black at half opacity over the window)
    v.extreme_bg_color = im(0.03, 0.03, 0.03, 1.0);
    v.faint_bg_color = im(1.0, 1.0, 1.0, 0.06);
    v.code_bg_color = im(0.19, 0.19, 0.20, 1.0);
    v.hyperlink_color = hovered;
    v.selection.bg_fill = im(0.26, 0.59, 0.98, 0.80);
    v.selection.stroke = Stroke::new(1.0, text);
    v.window_corner_radius = 0.into();
    v.menu_corner_radius = 0.into();
    for (w, fill) in [
        (&mut v.widgets.inactive, button),
        (&mut v.widgets.hovered, hovered),
        (&mut v.widgets.active, active),
        (&mut v.widgets.open, active),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = 0.into();
        w.fg_stroke = Stroke::new(1.0, text);
        w.bg_stroke = Stroke::NONE;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_fill = window;
    v.widgets.noninteractive.weak_bg_fill = window;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    v.widgets.noninteractive.corner_radius = 0.into();
    // text fields and combo boxes: ImGui's FrameBg
    v.widgets.inactive.bg_fill = frame;
    v.text_cursor.stroke = Stroke::new(1.0, text);
    v
}

/// The fonts and style of every frame.
pub fn apply(ctx: &egui::Context) {
    // ProggyClean (MIT, Tristan Grimmer): ImGui's bitmap font, drawn for 13 px
    ctx.add_font(FontInsert::new(
        "ProggyClean",
        FontData::from_static(include_bytes!("../assets/ProggyClean.ttf")),
        vec![
            InsertFontFamily { family: FontFamily::Proportional, priority: FontPriority::Highest },
            InsertFontFamily { family: FontFamily::Monospace, priority: FontPriority::Highest },
        ],
    ));
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.visuals = visuals();
        s.spacing.item_spacing = egui::vec2(8.0, 4.0);
        s.spacing.button_padding = egui::vec2(4.0, 3.0);
        s.spacing.interact_size.y = 19.0;
        s.spacing.window_margin = Margin::same(8);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.slider_rail_height = 4.0;
        s.interaction.tooltip_delay = 0.3;
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(13.0)),
            (TextStyle::Body, FontId::proportional(13.0)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Heading, FontId::proportional(13.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
    });
}

/// The dock's look: tabs as ImGui draws them.
pub fn dock_style(style: &egui::Style) -> egui_dock::Style {
    let mut st = egui_dock::Style::from_egui(style);
    let title = im(0.04, 0.04, 0.04, 1.0);
    let tab = im(0.18, 0.35, 0.58, 0.86);
    let selected = im(0.20, 0.41, 0.68, 1.0);
    st.tab_bar.bg_fill = title;
    st.tab_bar.hline_color = selected;
    st.tab_bar.height = 21.0;
    st.tab.active.bg_fill = selected;
    st.tab.focused.bg_fill = selected;
    st.tab.inactive.bg_fill = tab;
    st.tab.hovered.bg_fill = im(0.26, 0.59, 0.98, 0.80);
    for t in [&mut st.tab.active, &mut st.tab.focused, &mut st.tab.inactive, &mut st.tab.hovered] {
        t.text_color = Color32::WHITE;
        t.corner_radius = 0.into();
        t.outline_color = Color32::TRANSPARENT;
    }
    st.tab.tab_body.bg_fill = style.visuals.panel_fill;
    st.tab.tab_body.stroke = Stroke::NONE;
    st.separator.color_idle = im(0.43, 0.43, 0.50, 0.50);
    st.separator.color_hovered = im(0.10, 0.40, 0.75, 0.78);
    st.separator.color_dragged = im(0.10, 0.40, 0.75, 1.0);
    st.separator.width = 1.0;
    st.main_surface_border_stroke = Stroke::NONE;
    st
}
