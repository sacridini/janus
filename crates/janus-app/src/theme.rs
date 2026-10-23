//! The interface's look (src/theme.cpp): the C++'s seven themes (Dear ImGui's Dark, Light and Classic, and Janus,
//! Studio, Graphite and Matrix of its own) in egui's terms, their status colours (accent, warning, error, the
//! cursor's series), translucent panels, and ImGui's ProggyClean at the size set in Settings. Each theme is built
//! as the C++ builds it, on ImGui's colour slots; egui has fewer colours than ImGui's ~55 slots, so the slots are
//! mapped onto egui's visuals (RUST_MIGRATION.md, "Aparência").

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};

use egui::epaint::text::FontTweak;
use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};

pub const NAMES: [&str; 7] = ["Dark", "Light", "Classic", "Janus", "Studio", "Graphite", "Matrix"];
pub const DARK: usize = 0;
pub const LIGHT: usize = 1;
pub const CLASSIC: usize = 2;
pub const JANUS: usize = 3;
pub const STUDIO: usize = 4;
pub const GRAPHITE: usize = 5;
pub const MATRIX: usize = 6;

/// The interface's text in ImGui's pixels (the C++'s FontSize setting): 13 is ImGui's ProggyClean as drawn.
pub const DEFAULT_FONT_SIZE: i32 = 13;
pub const MIN_FONT_SIZE: i32 = 10;
pub const MAX_FONT_SIZE: i32 = 28;

/// egui sizes a font by its em, ImGui by its line (ascent - descent): ProggyClean's 7 × 13 px cells, ImGui's at
/// 13 px, are egui's at 16.
pub fn egui_font_size(px: i32) -> f32 {
    px as f32 * 16.0 / 13.0
}

static CURRENT: AtomicUsize = AtomicUsize::new(DARK);
static OPACITY: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0

/// An RGBA colour as ImGui keeps it: floats, alpha not premultiplied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct C(pub f32, pub f32, pub f32, pub f32);

impl C {
    fn hex(h: u32) -> C {
        C(((h >> 16) & 0xFF) as f32 / 255.0, ((h >> 8) & 0xFF) as f32 / 255.0, (h & 0xFF) as f32 / 255.0, 1.0)
    }

    fn a(self, a: f32) -> C {
        C(self.0, self.1, self.2, a)
    }

    fn mix(self, b: C, t: f32) -> C {
        C(
            self.0 + (b.0 - self.0) * t,
            self.1 + (b.1 - self.1) * t,
            self.2 + (b.2 - self.2) * t,
            self.3 + (b.3 - self.3) * t,
        )
    }

    /// Drawn over the opaque `under`.
    fn over(self, under: C) -> C {
        let a = self.3.clamp(0.0, 1.0);
        C(self.0 * a + under.0 * (1.0 - a), self.1 * a + under.1 * (1.0 - a), self.2 * a + under.2 * (1.0 - a), 1.0)
    }

    pub fn color(self) -> Color32 {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        Color32::from_rgba_unmultiplied(c(self.0), c(self.1), c(self.2), c(self.3))
    }

    fn of(c: Color32) -> C {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        C(f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0, f32::from(a) / 255.0)
    }

    fn luminance(self) -> f32 {
        let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * lin(self.0) + 0.7152 * lin(self.1) + 0.0722 * lin(self.2)
    }
}

/// The ImGui colour slots the interface uses, and the chart's (ImPlot's) background, grid and legend.
#[derive(Clone, Copy)]
struct Palette {
    text: C,
    text_disabled: C,
    window_bg: C,
    popup_bg: C,
    border: C,
    frame_bg: C,
    frame_bg_hovered: C,
    frame_bg_active: C,
    title_bg: C,
    title_bg_active: C,
    menu_bar_bg: C,
    scrollbar_bg: C,
    scrollbar_grab: C,
    scrollbar_grab_hovered: C,
    scrollbar_grab_active: C,
    check_mark: C,
    slider_grab: C,
    slider_grab_active: C,
    button: C,
    button_hovered: C,
    button_active: C,
    header: C,
    header_hovered: C,
    header_active: C,
    separator: C,
    separator_hovered: C,
    separator_active: C,
    tab: C,
    tab_hovered: C,
    tab_selected: C,
    tab_selected_overline: C,
    docking_empty_bg: C,
    table_header_bg: C,
    table_row_bg_alt: C,
    text_selected_bg: C,
    text_link: C,
    /// the charts' background (ImPlot's PlotBg, None: the window's) and grid
    plot_bg: Option<C>,
    plot_grid: Option<C>,
}

/// The tabs as ImGui derives them from the headers and the title bar.
fn tabs(p: &mut Palette, k: f32) {
    p.tab_hovered = p.header_hovered;
    p.tab = p.header.mix(p.title_bg_active, k);
    p.tab_selected = p.header_active.mix(p.title_bg_active, 0.60);
    p.tab_selected_overline = p.header_active;
}

/// ImGui::StyleColorsDark.
fn imgui_dark() -> Palette {
    let mut p = Palette {
        text: C(1.00, 1.00, 1.00, 1.00),
        text_disabled: C(0.50, 0.50, 0.50, 1.00),
        window_bg: C(0.06, 0.06, 0.06, 0.94),
        popup_bg: C(0.08, 0.08, 0.08, 0.94),
        border: C(0.43, 0.43, 0.50, 0.50),
        frame_bg: C(0.16, 0.29, 0.48, 0.54),
        frame_bg_hovered: C(0.26, 0.59, 0.98, 0.40),
        frame_bg_active: C(0.26, 0.59, 0.98, 0.67),
        title_bg: C(0.04, 0.04, 0.04, 1.00),
        title_bg_active: C(0.16, 0.29, 0.48, 1.00),
        menu_bar_bg: C(0.14, 0.14, 0.14, 1.00),
        scrollbar_bg: C(0.02, 0.02, 0.02, 0.53),
        scrollbar_grab: C(0.31, 0.31, 0.31, 1.00),
        scrollbar_grab_hovered: C(0.41, 0.41, 0.41, 1.00),
        scrollbar_grab_active: C(0.51, 0.51, 0.51, 1.00),
        check_mark: C(0.26, 0.59, 0.98, 1.00),
        slider_grab: C(0.24, 0.52, 0.88, 1.00),
        slider_grab_active: C(0.26, 0.59, 0.98, 1.00),
        button: C(0.26, 0.59, 0.98, 0.40),
        button_hovered: C(0.26, 0.59, 0.98, 1.00),
        button_active: C(0.06, 0.53, 0.98, 1.00),
        header: C(0.26, 0.59, 0.98, 0.31),
        header_hovered: C(0.26, 0.59, 0.98, 0.80),
        header_active: C(0.26, 0.59, 0.98, 1.00),
        separator: C(0.43, 0.43, 0.50, 0.50),
        separator_hovered: C(0.10, 0.40, 0.75, 0.78),
        separator_active: C(0.10, 0.40, 0.75, 1.00),
        tab: C(0.0, 0.0, 0.0, 0.0),
        tab_hovered: C(0.0, 0.0, 0.0, 0.0),
        tab_selected: C(0.0, 0.0, 0.0, 0.0),
        tab_selected_overline: C(0.0, 0.0, 0.0, 0.0),
        docking_empty_bg: C(0.20, 0.20, 0.20, 1.00),
        table_header_bg: C(0.19, 0.19, 0.20, 1.00),
        table_row_bg_alt: C(1.00, 1.00, 1.00, 0.06),
        text_selected_bg: C(0.26, 0.59, 0.98, 0.35),
        text_link: C(0.26, 0.59, 0.98, 1.00),
        plot_bg: None,
        plot_grid: None,
    };
    tabs(&mut p, 0.80);
    p
}

/// ImGui::StyleColorsLight (and ImPlot's light charts).
fn imgui_light() -> Palette {
    let mut p = Palette {
        text: C(0.00, 0.00, 0.00, 1.00),
        // 0.60 gives 2.5:1 on the window (theme.cpp)
        text_disabled: C(0.42, 0.42, 0.42, 1.00),
        window_bg: C(0.94, 0.94, 0.94, 1.00),
        popup_bg: C(1.00, 1.00, 1.00, 0.98),
        border: C(0.00, 0.00, 0.00, 0.30),
        frame_bg: C(1.00, 1.00, 1.00, 1.00),
        frame_bg_hovered: C(0.26, 0.59, 0.98, 0.40),
        frame_bg_active: C(0.26, 0.59, 0.98, 0.67),
        title_bg: C(0.96, 0.96, 0.96, 1.00),
        title_bg_active: C(0.82, 0.82, 0.82, 1.00),
        menu_bar_bg: C(0.86, 0.86, 0.86, 1.00),
        scrollbar_bg: C(0.98, 0.98, 0.98, 0.53),
        scrollbar_grab: C(0.69, 0.69, 0.69, 0.80),
        scrollbar_grab_hovered: C(0.49, 0.49, 0.49, 0.80),
        scrollbar_grab_active: C(0.49, 0.49, 0.49, 1.00),
        check_mark: C(0.26, 0.59, 0.98, 1.00),
        slider_grab: C(0.26, 0.59, 0.98, 0.78),
        slider_grab_active: C(0.46, 0.54, 0.80, 0.60),
        button: C(0.26, 0.59, 0.98, 0.40),
        button_hovered: C(0.26, 0.59, 0.98, 1.00),
        button_active: C(0.06, 0.53, 0.98, 1.00),
        header: C(0.26, 0.59, 0.98, 0.31),
        header_hovered: C(0.26, 0.59, 0.98, 0.80),
        header_active: C(0.26, 0.59, 0.98, 1.00),
        separator: C(0.39, 0.39, 0.39, 0.62),
        separator_hovered: C(0.14, 0.44, 0.80, 0.78),
        separator_active: C(0.14, 0.44, 0.80, 1.00),
        tab: C(0.0, 0.0, 0.0, 0.0),
        tab_hovered: C(0.0, 0.0, 0.0, 0.0),
        tab_selected: C(0.0, 0.0, 0.0, 0.0),
        tab_selected_overline: C(0.0, 0.0, 0.0, 0.0),
        docking_empty_bg: C(0.20, 0.20, 0.20, 1.00),
        table_header_bg: C(0.78, 0.87, 0.98, 1.00),
        table_row_bg_alt: C(0.30, 0.30, 0.30, 0.09),
        text_selected_bg: C(0.26, 0.59, 0.98, 0.35),
        text_link: C(0.26, 0.59, 0.98, 1.00),
        plot_bg: Some(C(0.42, 0.57, 1.00, 0.13)),
        plot_grid: Some(C(1.00, 1.00, 1.00, 1.00)),
    };
    tabs(&mut p, 0.90);
    p
}

/// ImGui::StyleColorsClassic (and ImPlot's classic charts).
fn imgui_classic() -> Palette {
    let mut p = Palette {
        text: C(0.90, 0.90, 0.90, 1.00),
        text_disabled: C(0.60, 0.60, 0.60, 1.00),
        window_bg: C(0.00, 0.00, 0.00, 0.85),
        popup_bg: C(0.11, 0.11, 0.14, 0.92),
        border: C(0.50, 0.50, 0.50, 0.50),
        frame_bg: C(0.43, 0.43, 0.43, 0.39),
        frame_bg_hovered: C(0.47, 0.47, 0.69, 0.40),
        frame_bg_active: C(0.42, 0.41, 0.64, 0.69),
        title_bg: C(0.27, 0.27, 0.54, 0.83),
        title_bg_active: C(0.32, 0.32, 0.63, 0.87),
        menu_bar_bg: C(0.40, 0.40, 0.55, 0.80),
        scrollbar_bg: C(0.20, 0.25, 0.30, 0.60),
        scrollbar_grab: C(0.40, 0.40, 0.80, 0.30),
        scrollbar_grab_hovered: C(0.40, 0.40, 0.80, 0.40),
        scrollbar_grab_active: C(0.41, 0.39, 0.80, 0.60),
        check_mark: C(0.90, 0.90, 0.90, 0.50),
        slider_grab: C(1.00, 1.00, 1.00, 0.30),
        slider_grab_active: C(0.41, 0.39, 0.80, 0.60),
        button: C(0.35, 0.40, 0.61, 0.62),
        button_hovered: C(0.40, 0.48, 0.71, 0.79),
        button_active: C(0.46, 0.54, 0.80, 1.00),
        header: C(0.40, 0.40, 0.90, 0.45),
        header_hovered: C(0.45, 0.45, 0.90, 0.80),
        header_active: C(0.53, 0.53, 0.87, 0.80),
        separator: C(0.50, 0.50, 0.50, 0.60),
        separator_hovered: C(0.60, 0.60, 0.70, 1.00),
        separator_active: C(0.70, 0.70, 0.90, 1.00),
        tab: C(0.0, 0.0, 0.0, 0.0),
        tab_hovered: C(0.0, 0.0, 0.0, 0.0),
        tab_selected: C(0.0, 0.0, 0.0, 0.0),
        tab_selected_overline: C(0.0, 0.0, 0.0, 0.0),
        docking_empty_bg: C(0.20, 0.20, 0.20, 1.00),
        table_header_bg: C(0.27, 0.27, 0.38, 1.00),
        table_row_bg_alt: C(1.00, 1.00, 1.00, 0.07),
        text_selected_bg: C(0.00, 0.00, 1.00, 0.35),
        text_link: C(0.53, 0.53, 0.87, 0.80),
        plot_bg: Some(C(0.00, 0.00, 0.00, 0.35)),
        plot_grid: Some(C(0.90, 0.90, 0.90, 0.25)),
    };
    tabs(&mut p, 0.80);
    p
}

/// Janus' visual identity (branding/README.md): ImGui blue #4296FA, window #0F0F0F, panel #1C2A3C, the date's
/// orange #F29A38 only as an accent.
fn janus() -> Palette {
    let mut p = imgui_dark();
    let (blue, panel, orange) = (C::hex(0x4296FA), C::hex(0x1C2A3C), C::hex(0xF29A38));
    p.text = C(0.92, 0.94, 0.97, 1.0);
    p.text_disabled = C(0.53, 0.59, 0.67, 1.0);
    p.window_bg = C::hex(0x0F0F0F);
    p.popup_bg = C(0.07, 0.09, 0.13, 0.98);
    p.border = C(0.20, 0.28, 0.38, 0.60);
    p.frame_bg = panel;
    p.frame_bg_hovered = panel.mix(blue, 0.30);
    p.frame_bg_active = panel.mix(blue, 0.45);
    p.title_bg = C(0.06, 0.08, 0.11, 1.0);
    p.title_bg_active = panel;
    p.menu_bar_bg = C(0.08, 0.11, 0.15, 1.0);
    p.scrollbar_bg = C(0.04, 0.05, 0.07, 0.60);
    p.scrollbar_grab = C(0.19, 0.26, 0.36, 1.0);
    p.scrollbar_grab_hovered = C(0.25, 0.34, 0.47, 1.0);
    p.scrollbar_grab_active = C(0.31, 0.42, 0.58, 1.0);
    p.separator = p.border;
    p.tab = p.header.mix(panel, 0.80);
    p.tab_selected = p.header_active.mix(panel, 0.60);
    p.docking_empty_bg = C(0.04, 0.05, 0.07, 1.0);
    p.table_header_bg = panel;
    p.text_link = orange;
    p
}

/// Studio: Ableton Live's mid grey, dark text, the charts in a dark display (the data colours keep their
/// brightness).
fn studio() -> Palette {
    let mut p = imgui_light();
    let (orange, win) = (C::hex(0xFF9A1F), C::hex(0xB0B0B0));
    p.text = C::hex(0x101010);
    p.text_disabled = C::hex(0x3C3C3C);
    p.window_bg = win;
    p.popup_bg = C::hex(0xBEBEBE);
    p.border = C::hex(0x6E6E6E);
    p.frame_bg = C::hex(0xC6C6C6);
    p.frame_bg_hovered = C::hex(0xD2D2D2);
    p.frame_bg_active = C::hex(0xDCDCDC);
    p.title_bg = C::hex(0x8C8C8C);
    p.title_bg_active = C::hex(0x9A9A9A);
    p.menu_bar_bg = C::hex(0x9E9E9E);
    p.scrollbar_bg = C::hex(0xA2A2A2);
    p.scrollbar_grab = C::hex(0x7E7E7E);
    p.scrollbar_grab_hovered = C::hex(0x6C6C6C);
    p.scrollbar_grab_active = C::hex(0x5A5A5A);
    p.check_mark = C::hex(0x1A1A1A);
    p.slider_grab = C::hex(0x5E5E5E);
    p.slider_grab_active = orange;
    p.button = C::hex(0xA0A0A0);
    p.button_hovered = C::hex(0xBDBDBD);
    p.button_active = orange;
    p.header = C::hex(0x9C9C9C);
    p.header_hovered = C::hex(0xC0C0C0);
    p.header_active = orange.mix(win, 0.35);
    p.separator = C::hex(0x7E7E7E);
    p.separator_hovered = orange;
    p.separator_active = orange;
    p.tab = C::hex(0x969696);
    p.tab_hovered = C::hex(0xC0C0C0);
    p.tab_selected = win;
    p.tab_selected_overline = orange;
    p.docking_empty_bg = C::hex(0x8A8A8A);
    p.table_header_bg = C::hex(0x9C9C9C);
    p.table_row_bg_alt = C(1.0, 1.0, 1.0, 0.06);
    p.text_selected_bg = orange.mix(win, 0.45);
    p.text_link = C::hex(0x0A3D73);
    p.plot_bg = Some(C::hex(0x2A2A2A));
    p.plot_grid = Some(C(1.0, 1.0, 1.0, 0.13));
    p
}

/// Graphite: Max's dark grey, darker fields, light text, a yellow accent.
fn graphite() -> Palette {
    let mut p = imgui_dark();
    let (yellow, win) = (C::hex(0xF5B22E), C::hex(0x333333));
    p.text = C::hex(0xDCDCDC);
    p.text_disabled = C::hex(0xA2A2A2);
    p.window_bg = win;
    p.popup_bg = C::hex(0x2A2A2A);
    p.border = C::hex(0x4C4C4C);
    p.frame_bg = C::hex(0x242424);
    p.frame_bg_hovered = C::hex(0x2C2C2C);
    p.frame_bg_active = C::hex(0x1C1C1C);
    p.title_bg = C::hex(0x262626);
    p.title_bg_active = C::hex(0x2E2E2E);
    p.menu_bar_bg = C::hex(0x282828);
    p.scrollbar_bg = C::hex(0x2B2B2B);
    p.scrollbar_grab = C::hex(0x4E4E4E);
    p.scrollbar_grab_hovered = C::hex(0x5C5C5C);
    p.scrollbar_grab_active = C::hex(0x6A6A6A);
    p.check_mark = yellow;
    p.slider_grab = C::hex(0x8A8A8A);
    p.slider_grab_active = yellow;
    p.button = C::hex(0x464646);
    p.button_hovered = C::hex(0x545454);
    p.button_active = yellow.mix(win, 0.45);
    p.header = C::hex(0x464646);
    p.header_hovered = C::hex(0x525252);
    p.header_active = yellow.mix(win, 0.60);
    p.separator = C::hex(0x4C4C4C);
    p.separator_hovered = yellow;
    p.separator_active = yellow;
    p.tab = C::hex(0x2A2A2A);
    p.tab_hovered = C::hex(0x4A4A4A);
    p.tab_selected = win;
    p.tab_selected_overline = yellow;
    p.docking_empty_bg = C::hex(0x1E1E1E);
    p.table_header_bg = C::hex(0x2A2A2A);
    p.table_row_bg_alt = C(1.0, 1.0, 1.0, 0.03);
    p.text_selected_bg = yellow.mix(win, 0.65);
    p.text_link = yellow;
    p.plot_bg = Some(C::hex(0x262626));
    p
}

/// Matrix: phosphor green on black, the controls in dark greens, the charts on black with a green grid.
fn matrix() -> Palette {
    let mut p = imgui_dark();
    let (green, mid, win) = (C::hex(0x00FF41), C::hex(0x00B32D), C::hex(0x000000));
    p.text = green;
    p.text_disabled = C::hex(0x22A047);
    p.window_bg = win;
    p.popup_bg = C::hex(0x020A04);
    p.border = C::hex(0x0B4F1F);
    p.frame_bg = C::hex(0x03180A);
    p.frame_bg_hovered = C::hex(0x06301A);
    p.frame_bg_active = C::hex(0x0A4424);
    p.title_bg = C::hex(0x010802);
    p.title_bg_active = C::hex(0x03200B);
    p.menu_bar_bg = C::hex(0x020E05);
    p.scrollbar_bg = C::hex(0x010502);
    p.scrollbar_grab = C::hex(0x0B4F1F);
    p.scrollbar_grab_hovered = C::hex(0x107A2E);
    p.scrollbar_grab_active = mid;
    p.check_mark = green;
    p.slider_grab = mid;
    p.slider_grab_active = green;
    p.button = C::hex(0x05260E);
    p.button_hovered = C::hex(0x0A4A1C);
    p.button_active = C::hex(0x107A2E);
    p.header = C::hex(0x05260E);
    p.header_hovered = C::hex(0x0A4A1C);
    p.header_active = C::hex(0x0E5F26);
    p.separator = C::hex(0x0B4F1F);
    p.separator_hovered = mid;
    p.separator_active = green;
    p.tab = C::hex(0x020E05);
    p.tab_hovered = C::hex(0x0A4A1C);
    p.tab_selected = C::hex(0x05260E);
    p.tab_selected_overline = green;
    p.docking_empty_bg = win;
    p.table_header_bg = C::hex(0x03180A);
    p.table_row_bg_alt = C(0.0, 1.0, 0.25, 0.04);
    p.text_selected_bg = C(0.0, 1.0, 0.25, 0.30);
    p.text_link = C::hex(0x7DFF9E);
    p.plot_bg = Some(C::hex(0x000000));
    p.plot_grid = Some(C(0.0, 1.0, 0.25, 0.14));
    p
}

fn palette(id: usize) -> Palette {
    match id {
        LIGHT => imgui_light(),
        CLASSIC => imgui_classic(),
        JANUS => janus(),
        STUDIO => studio(),
        GRAPHITE => graphite(),
        MATRIX => matrix(),
        _ => imgui_dark(),
    }
}

/// The status colours of a theme: accent, warning, error, the cursor's series, the window's clear colour.
struct Status {
    accent: C,
    warning: C,
    error: C,
    cursor: C,
    clear: C,
}

fn status() -> Status {
    let white = C(0.95, 0.95, 0.95, 1.0);
    match CURRENT.load(Ordering::Relaxed) {
        LIGHT => Status {
            accent: C(0.05, 0.33, 0.68, 1.0),
            warning: C(0.60, 0.32, 0.0, 1.0),
            error: C(0.75, 0.12, 0.10, 1.0),
            cursor: C(0.10, 0.10, 0.12, 1.0),
            clear: C(0.80, 0.80, 0.82, 1.0),
        },
        JANUS => Status {
            accent: C(0.45, 0.70, 1.0, 1.0),
            warning: C::hex(0xF29A38),
            error: C(1.0, 0.52, 0.45, 1.0),
            cursor: white,
            clear: C(0.04, 0.05, 0.07, 1.0),
        },
        // Studio's charts are dark displays: the cursor's series stays white
        STUDIO => Status {
            accent: C::hex(0x0A3D73),
            warning: C::hex(0x5C2C00),
            error: C::hex(0x7A0E0E),
            cursor: white,
            clear: C::hex(0x8A8A8A),
        },
        GRAPHITE => Status {
            accent: C::hex(0xF5B22E),
            warning: C(1.0, 0.62, 0.38, 1.0),
            error: C(1.0, 0.50, 0.45, 1.0),
            cursor: white,
            clear: C::hex(0x1E1E1E),
        },
        // a pale green accent and cursor; the warning and the error stand out of the green
        MATRIX => Status {
            accent: C::hex(0xB6FFC8),
            warning: C::hex(0xE8FF5A),
            error: C::hex(0xFF5A5A),
            cursor: C::hex(0xD9FFE3),
            clear: C::hex(0x000000),
        },
        _ => Status {
            accent: C(0.55, 0.80, 1.0, 1.0),
            warning: C(1.0, 0.7, 0.4, 1.0),
            error: C(1.0, 0.55, 0.45, 1.0),
            cursor: white,
            clear: C(0.08, 0.08, 0.09, 1.0),
        },
    }
}

pub fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

pub fn panel_opacity() -> f32 {
    f32::from_bits(OPACITY.load(Ordering::Relaxed))
}

pub fn accent() -> Color32 {
    status().accent.color()
}

pub fn warning() -> Color32 {
    status().warning.color()
}

pub fn error() -> Color32 {
    status().error.color()
}

/// The cursor's series and marks: white on the dark themes, near black on the light one.
pub fn cursor_series() -> Color32 {
    status().cursor.color()
}

pub fn weak_text() -> Color32 {
    palette(current()).text_disabled.color()
}

/// The window's clear colour: nothing under translucent panels (each panel's background is the one layer over the
/// desktop, so they all have the opacity set).
pub fn clear_color() -> [f32; 4] {
    let c = status().clear;
    let a = if panel_opacity() < 1.0 { 0.0 } else { 1.0 };
    [c.0 * a, c.1 * a, c.2 * a, a]
}

/// The contrast ratio of two colours (WCAG).
fn ratio(a: C, b: C) -> f32 {
    let (la, lb) = (a.luminance(), b.luminance());
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
pub fn contrast(a: Color32, b: Color32) -> f32 {
    ratio(C::of(a), C::of(b))
}

/// `c` made legible on `bg`: towards black on a light background, towards white on a dark one, the smallest step
/// (bisection) that reaches the ratio.
fn legible(c: C, bg: C, ratio_min: f32) -> C {
    let k = |c: C| ratio(c, bg);
    if k(c) >= ratio_min {
        return c;
    }
    // a little above the ratio: the colour is rounded to 8 bits when drawn
    let ratio_min = ratio_min + 0.05;
    if k(c) >= ratio_min {
        return c;
    }
    let target = if bg.luminance() > 0.18 { C(0.0, 0.0, 0.0, c.3) } else { C(1.0, 1.0, 1.0, c.3) };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..16 {
        let t = (lo + hi) * 0.5;
        if k(c.mix(target, t)) >= ratio_min {
            hi = t;
        } else {
            lo = t;
        }
    }
    c.mix(target, hi)
}

/// The theme's opaque window background (what is behind a translucent window is unknown).
fn background() -> C {
    palette(current()).window_bg.over(C(0.0, 0.0, 0.0, 1.0))
}

fn plot_background() -> C {
    let p = palette(current());
    match p.plot_bg {
        Some(c) if c.3 > 0.0 && panel_opacity() >= 1.0 => c.over(background()),
        _ => background(),
    }
}

/// A data colour on the chart, legible on its background (3:1).
pub fn on_plot(c: Color32) -> Color32 {
    legible(C::of(c), plot_background(), 3.0).color()
}

/// A colour as text on the window (4.5:1), e.g. the table's column headers.
pub fn on_window(c: Color32) -> Color32 {
    let p = palette(current());
    legible(C::of(c), p.table_header_bg.over(background()), 4.5).color()
}

/// The chart's background (ImPlot's PlotBg over the window; none with translucent panels).
pub fn plot_fill() -> Color32 {
    if panel_opacity() < 1.0 { Color32::TRANSPARENT } else { plot_background().color() }
}

fn visuals(id: usize, opacity: f32) -> Visuals {
    let mut p = palette(id);
    let dark = id != LIGHT && id != STUDIO;
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    p.window_bg.3 = 1.0;
    // translucent panels: the backgrounds take the opacity (the fields and buttons part of it, so they still read
    // as controls); popups and menus keep most of theirs, they are read over whatever is behind
    if opacity < 1.0 {
        let (o, soft, popup) = (opacity, 0.5 + 0.5 * opacity, opacity.max(0.88));
        for c in [
            &mut p.window_bg,
            &mut p.menu_bar_bg,
            &mut p.title_bg,
            &mut p.title_bg_active,
            &mut p.docking_empty_bg,
            &mut p.scrollbar_bg,
            &mut p.table_header_bg,
            &mut p.tab,
            &mut p.tab_selected,
        ] {
            c.3 *= o;
        }
        for c in [&mut p.frame_bg, &mut p.frame_bg_hovered, &mut p.frame_bg_active, &mut p.button, &mut p.header] {
            c.3 *= soft;
        }
        p.popup_bg.3 *= popup;
    }
    let text = p.text.color();
    v.override_text_color = Some(text);
    v.weak_text_color = Some(p.text_disabled.color());
    v.panel_fill = p.window_bg.color();
    v.window_fill = p.popup_bg.color();
    v.window_stroke = Stroke::new(1.0, p.border.color());
    // text fields and the charts' background
    v.extreme_bg_color = if opacity < 1.0 { Color32::TRANSPARENT } else { plot_background_of(&p).color() };
    v.text_edit_bg_color = Some(p.frame_bg.color());
    v.faint_bg_color = p.table_row_bg_alt.color();
    v.code_bg_color = p.table_header_bg.color();
    v.hyperlink_color = p.text_link.color();
    v.selection.bg_fill = p.text_selected_bg.color();
    v.selection.stroke = Stroke::new(1.0, text);
    v.window_corner_radius = 0.into();
    v.menu_corner_radius = 0.into();
    v.warn_fg_color = status().warning.color();
    v.error_fg_color = status().error.color();
    // the studio themes: flat controls outlined as in the audio programs; Matrix's fields outlined like a terminal
    let outlined = id == STUDIO || id == GRAPHITE || id == MATRIX;
    let rounding = if id == GRAPHITE { 2 } else { 0 };
    let outline = Stroke::new(if outlined { 1.0 } else { 0.0 }, p.border.color());
    for (w, fill, frame) in [
        (&mut v.widgets.inactive, p.button, p.frame_bg),
        (&mut v.widgets.hovered, p.button_hovered, p.frame_bg_hovered),
        (&mut v.widgets.active, p.button_active, p.frame_bg_active),
        (&mut v.widgets.open, p.header_active, p.frame_bg_active),
    ] {
        w.bg_fill = frame.color();
        w.weak_bg_fill = fill.color();
        w.corner_radius = rounding.into();
        w.fg_stroke = Stroke::new(1.0, text);
        w.bg_stroke = outline;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_fill = p.window_bg.color();
    v.widgets.noninteractive.weak_bg_fill = p.window_bg.color();
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.separator.color());
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    v.widgets.noninteractive.corner_radius = 0.into();
    v.selection.bg_fill = p.header_active.a(p.header_active.3 * 0.8).color();
    v.slider_trailing_fill = true;
    v.text_cursor.stroke = Stroke::new(1.0, text);
    v
}

fn plot_background_of(p: &Palette) -> C {
    let win = p.window_bg.over(C(0.0, 0.0, 0.0, 1.0));
    match p.plot_bg {
        Some(c) if c.3 > 0.0 => c.over(win),
        _ => win,
    }
}

/// The face in use: -1 none yet, 0 ProggyClean, 1 ProggyForever.
static FACE: AtomicI32 = AtomicI32::new(-1);

/// The fonts for the first frame (ImGui's at 13 px).
pub fn load_fonts(ctx: &egui::Context) {
    set_face(ctx, DEFAULT_FONT_SIZE);
}

/// As the C++'s applyFont: ProggyClean (MIT, Tristan Grimmer), ImGui's bitmap font, keeps the usual look at 13 px;
/// ProggyForever (MIT, Disco Hello and Tristan Grimmer), its scalable companion, stays sharp at the other sizes.
/// egui's fonts again only when the face changes.
fn set_face(ctx: &egui::Context, font_px: i32) {
    let vector = font_px != DEFAULT_FONT_SIZE;
    if FACE.swap(i32::from(vector), Ordering::Relaxed) == i32::from(vector) {
        return;
    }
    let mut defs = FontDefinitions::default();
    defs.font_data
        .insert("ProggyClean".into(), Arc::new(FontData::from_static(include_bytes!("../assets/ProggyClean.ttf"))));
    // egui sizes text by its em, ImGui by its line: ProggyForever's line is 0.828 em, ProggyClean's 0.8125 (13 / 16,
    // egui_font_size's factor), so it is drawn at 0.8125 / 0.828 to keep ImGui's line heights
    defs.font_data.insert(
        "ProggyForever".into(),
        Arc::new(
            FontData::from_static(include_bytes!("../assets/ProggyForever-Regular-minimal.ttf"))
                .tweak(FontTweak { scale: 0.8125 / 0.828, ..Default::default() }),
        ),
    );
    let first = if vector { "ProggyForever" } else { "ProggyClean" };
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        defs.families.entry(family).or_default().insert(0, first.to_string());
    }
    ctx.set_fonts(defs);
}

/// The face the interface is drawn with: true for the scalable one (sizes other than 13 px).
pub fn scalable_face() -> bool {
    FACE.load(Ordering::Relaxed) == 1
}

/// The style of every frame: theme `id`, its panels' opacity and the text at `font_px` (ImGui's pixels).
pub fn apply(ctx: &egui::Context, id: usize, opacity: f32, font_px: i32) {
    let id = id.min(NAMES.len() - 1);
    CURRENT.store(id, Ordering::Relaxed);
    OPACITY.store(opacity.clamp(0.1, 1.0).to_bits(), Ordering::Relaxed);
    let dark = id != LIGHT && id != STUDIO;
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);
    let font_px = font_px.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    set_face(ctx, font_px);
    let size = egui_font_size(font_px);
    // the spacing grows with the text, as ImGui's frames follow its font
    let k = size / 16.0;
    ctx.style_mut_of(theme, |s| {
        s.visuals = visuals(id, opacity);
        s.spacing.item_spacing = egui::vec2(8.0, 4.0);
        s.spacing.button_padding = egui::vec2(4.0, 3.0) * k.max(1.0);
        s.spacing.interact_size.y = (size + 6.0).max(19.0);
        s.spacing.window_margin = Margin::same(8);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.slider_rail_height = 4.0;
        s.interaction.tooltip_delay = 0.3;
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(size)),
            (TextStyle::Body, FontId::proportional(size)),
            (TextStyle::Button, FontId::proportional(size)),
            (TextStyle::Heading, FontId::proportional(size)),
            (TextStyle::Monospace, FontId::monospace(size)),
        ]
        .into();
    });
}

/// The map's empty background: the C++'s, or nothing (the panel's own, at its opacity) with translucent panels.
pub fn map_background() -> [f32; 4] {
    if panel_opacity() < 1.0 { [0.0; 4] } else { [0.10, 0.10, 0.115, 1.0] }
}

/// The dock's look: tabs and separators as ImGui draws them in the theme.
pub fn dock_style(style: &egui::Style) -> egui_dock::Style {
    let p = palette(current());
    let mut st = egui_dock::Style::from_egui(style);
    let o = panel_opacity();
    let fade = |c: C| if o < 1.0 { c.a(c.3 * o) } else { c };
    st.tab_bar.bg_fill = fade(p.title_bg).color();
    st.tab_bar.hline_color = p.tab_selected_overline.color();
    st.tab_bar.height = (style.text_styles[&TextStyle::Body].size + 8.0).max(21.0);
    st.tab.active.bg_fill = fade(p.tab_selected).color();
    st.tab.focused.bg_fill = fade(p.tab_selected).color();
    st.tab.inactive.bg_fill = fade(p.tab).color();
    st.tab.hovered.bg_fill = p.tab_hovered.color();
    for t in [&mut st.tab.active, &mut st.tab.focused, &mut st.tab.inactive, &mut st.tab.hovered] {
        t.text_color = p.text.color();
        t.corner_radius = 0.into();
        t.outline_color = Color32::TRANSPARENT;
    }
    st.tab.tab_body.bg_fill = style.visuals.panel_fill;
    st.tab.tab_body.stroke = Stroke::NONE;
    st.separator.color_idle = p.separator.color();
    st.separator.color_hovered = p.separator_hovered.color();
    st.separator.color_dragged = p.separator_active.color();
    st.separator.width = 1.0;
    st.main_surface_border_stroke = Stroke::NONE;
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default font draws ProggyClean's 7 × 13 px cells, as ImGui does at 13 px.
    #[test]
    fn imgui_font_size() {
        let ctx = egui::Context::default();
        load_fonts(&ctx);
        apply(&ctx, DARK, 1.0, DEFAULT_FONT_SIZE);
        // the frame's font atlas not uploaded anywhere: egui 0.36 panics on a TexturesDelta dropped unapplied
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        let g = ctx.fonts_mut(|f| {
            f.layout_no_wrap("MMMMMMMMMM".into(), FontId::proportional(egui_font_size(13)), Color32::WHITE)
        });
        assert!((g.size().x - 70.0).abs() < 0.5 && (g.size().y - 13.0).abs() < 0.5, "{:?}", g.size());
    }

    /// Every theme keeps the text and the data colours legible (WCAG: 4.5:1 for text, 3:1 for chart series), and
    /// the dark theme draws the data colours as they are (theme.cpp's self-test).
    #[test]
    fn legible_themes() {
        let data = [
            Color32::from_rgb(255, 209, 61),
            Color32::from_rgb(255, 153, 51),
            Color32::from_rgb(115, 158, 230),
            Color32::from_rgb(242, 38, 242),
        ];
        for (id, name) in NAMES.iter().enumerate() {
            CURRENT.store(id, Ordering::Relaxed);
            OPACITY.store(1f32.to_bits(), Ordering::Relaxed);
            let p = palette(id);
            let win = background();
            let mut min_text = f32::MAX;
            for c in [p.text, p.text_disabled, status().accent, status().warning, status().error] {
                min_text = min_text.min(contrast(c.color(), win.color()));
            }
            let mut changed = 0;
            let mut min_plot = f32::MAX;
            let pins: Vec<Color32> = (1..=8).map(crate::series::pin_color).collect();
            for &c in data.iter().chain(&pins).chain(std::iter::once(&cursor_series())) {
                let q = on_plot(c);
                min_plot = min_plot.min(contrast(q, plot_background().color()));
                changed += usize::from(q != c);
            }
            assert!(min_text >= 4.5, "{name}: text {min_text}");
            assert!(min_plot >= 3.0, "{name}: chart {min_plot}");
            if id == DARK {
                assert_eq!(changed, 0, "the dark theme must draw the data colours unchanged");
            }
        }
        CURRENT.store(DARK, Ordering::Relaxed);
    }
}
