//! The map's "Seasonal peak" mode (Mode::Phase): when in the year every pixel's series peaks, its annual harmonic
//! fitted in the display shader (display.wgsl, harmonic; janus_gpu::phase on the CPU) and shown as a colour: the
//! hue the peak's time of year, the saturation its amplitude, the brightness its mean. A September soybean and an
//! April second crop in different colours, an evergreen forest grey (no season), a pasture pale. Its legend is a
//! wheel of the months.

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Align2, Color32, FontId, Painter, Pos2, Ui, vec2};

use crate::display::Phase;

const MONTHS: [&str; 12] = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"];

/// The colour of a time of year (a fraction of it) at full saturation and brightness, as the map paints it.
pub(crate) fn hue_colour(frac: f32, sat: f32) -> Color32 {
    let [r, g, b] = janus_gpu::phase::hsv(frac.rem_euclid(1.0), sat, 1.0);
    Color32::from_rgb((r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
}

/// The angle on the wheel of a time of year: January at the top, clockwise.
fn angle(frac: f32) -> f32 {
    std::f32::consts::TAU * frac - std::f32::consts::FRAC_PI_2
}

/// The months' wheel: a ring of the hues (paler inside: a smaller amplitude), the months' initials around it, and a
/// mark at `mark` (a time of year: the cursor's peak).
pub(crate) fn wheel(painter: &Painter, centre: Pos2, radius: f32, font: FontId, text: Color32, mark: Option<f32>) {
    let (r0, r1) = (radius * 0.35, radius);
    let n = 96;
    let mut mesh = Mesh::default();
    for i in 0..=n {
        let f = i as f32 / n as f32;
        let a = angle(f);
        let dir = vec2(a.cos(), a.sin());
        mesh.vertices.push(Vertex { pos: centre + dir * r0, uv: WHITE_UV, color: hue_colour(f, 0.15) });
        mesh.vertices.push(Vertex { pos: centre + dir * r1, uv: WHITE_UV, color: hue_colour(f, 1.0) });
    }
    for i in 0..n as u32 {
        let k = 2 * i;
        mesh.indices.extend_from_slice(&[k, k + 1, k + 2, k + 1, k + 3, k + 2]);
    }
    painter.add(mesh);
    for (m, name) in MONTHS.iter().enumerate() {
        // the month's middle
        let a = angle((m as f32 + 0.5) / 12.0);
        let at = centre + vec2(a.cos(), a.sin()) * (radius + 8.0);
        painter.text(at + vec2(1.0, 1.0), Align2::CENTER_CENTER, *name, font.clone(), Color32::from_black_alpha(160));
        painter.text(at, Align2::CENTER_CENTER, *name, font.clone(), text);
    }
    if let Some(f) = mark {
        let a = angle(f);
        let dir = vec2(a.cos(), a.sin());
        let stroke = egui::Stroke::new(2.5, Color32::BLACK);
        painter.line_segment([centre + dir * (r0 - 2.0), centre + dir * (r1 + 2.0)], stroke);
        painter.line_segment(
            [centre + dir * (r0 - 2.0), centre + dir * (r1 + 2.0)],
            egui::Stroke::new(1.0, Color32::WHITE),
        );
    }
}

/// A time of year as a date: "Sep 14".
pub(crate) fn date_of(frac: f32) -> String {
    crate::season::month_day((frac.rem_euclid(1.0) * 365.25).floor() as i32 + 1)
}

/// The Display panel's options and legend of the seasonal peak; returns whether they changed.
pub(crate) fn options_ui(ui: &mut Ui, p: &mut Phase, mark: Option<f32>) -> bool {
    let changed = ui
        .checkbox(&mut p.by_mean, "Brightness by the mean")
        .on_hover_text(
            "On: brighter where the series' mean is higher (from a quarter at the lowest), as a forest is brighter \
             than bare soil.\nOff: every pixel bright, only when (hue) and how much (saturation) it peaks.",
        )
        .changed();
    let size = 132.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size + 8.0), egui::Sense::hover());
    let font = FontId::proportional(11.0);
    wheel(ui.painter(), rect.center(), size / 2.0 - 14.0, font, ui.visuals().text_color(), mark);
    ui.weak(
        "Each pixel's annual harmonic, fitted to its dates: the hue is when it peaks, the saturation how much it \
         rises and falls (the stretch below: the peak-to-peak amplitude), the brightness its mean. Grey: no season \
         (evergreen, water, built-up). Needs dates round the year, 6 at least (overview).",
    );
    changed
}
