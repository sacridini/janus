//! The Classes tab's flows (View > Analysis > Classes, Flows): a Sankey diagram of a categorical series between two
//! to six dates, in the Analysis panel or in a window of its own. A column per date, a bar per class as tall as its
//! pixels (a few points at least, so a rare class stays visible), a ribbon per change from one date to the next as
//! wide as the pixels that made it, shaded from the colour of the class they left to the one they joined; the
//! pixels that kept their class fainter, so that the changes stand out. The classes below a share of the pixels at
//! every date can be grouped as "Other". Counted on the overview, a pair of consecutive dates at a time: the ribbons
//! of 1990 -> 2000 and 2000 -> 2010 meet at 2000's bars, but a pixel is not followed across them (two pixels going
//! A -> B -> C and A -> D -> C draw the same as A -> B -> C and A -> D -> C swapped).

use egui::epaint::Mesh;
use egui::text::LayoutJob;
use egui::{Color32, Pos2, Rect, Sense, Ui, vec2};

use crate::analysis::Transitions;
use crate::app::State;
use crate::theme;

/// The most dates a diagram shows.
pub const MAX_DATES: usize = 6;

/// The most a diagram is zoomed (a class of a few pixels among millions: a sliver still).
const MAX_ZOOM: f32 = 40.0;

/// The shares (%) under which the classes are grouped as "Other" at every date; 0: none grouped. The first is the
/// default.
pub const SMALL: [f64; 5] = [0.5, 1.0, 2.0, 5.0, 0.0];

/// A class's bar in a column, from `y0` to `y1` (points from the diagram's top), of `n` pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    pub k: usize,
    pub y0: f32,
    pub y1: f32,
    pub n: u64,
}

/// The pixels that went from class `i` at column `col` to class `j` at the next: from `a` (top and bottom at the
/// left) to `b` (at the right). The two ends differ in width where a bar was made taller than its pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ribbon {
    pub col: usize,
    pub i: usize,
    pub j: usize,
    pub n: u32,
    pub a: (f32, f32),
    pub b: (f32, f32),
}

#[derive(Debug, Default)]
pub struct Layout {
    /// [column] the bars of the classes shown, in the classes' order
    pub nodes: Vec<Vec<Node>>,
    pub ribbons: Vec<Ribbon>,
}

/// The diagram of `mats` (a K x K matrix per pair of consecutive dates, rows = from) in `height` points, `gap`
/// between bars, a bar `min_h` points tall at least; the classes not `shown` left out. A bar is as tall as the larger
/// of the pixels coming to it and those leaving it (the two differ by the pixels without a value at the other date);
/// one scale for every column, so that the fullest one fills the height.
pub fn layout(mats: &[Vec<u32>], k_n: usize, shown: &[bool], height: f32, gap: f32, min_h: f32) -> Layout {
    let cols = mats.len() + 1;
    let on = |k: usize| shown.get(k).copied().unwrap_or(true);
    let size = |c: usize, k: usize| -> u64 {
        let incoming: u64 =
            if c > 0 { (0..k_n).filter(|&i| on(i)).map(|i| u64::from(mats[c - 1][i * k_n + k])).sum() } else { 0 };
        let outgoing: u64 =
            if c < cols - 1 { (0..k_n).filter(|&j| on(j)).map(|j| u64::from(mats[c][k * k_n + j])).sum() } else { 0 };
        incoming.max(outgoing)
    };
    let sizes: Vec<Vec<u64>> =
        (0..cols).map(|c| (0..k_n).map(|k| if on(k) { size(c, k) } else { 0 }).collect()).collect();
    let bars = |s: &Vec<u64>| s.iter().filter(|&&n| n > 0).count();
    let mut scale = f32::INFINITY;
    for s in &sizes {
        let total: u64 = s.iter().sum();
        if total > 0 {
            scale = scale.min((height - gap * bars(s).saturating_sub(1) as f32).max(1.0) / total as f32);
        }
    }
    if !scale.is_finite() {
        return Layout { nodes: vec![Vec::new(); cols], ribbons: Vec::new() };
    }
    // the bars raised to min_h take room from the others: the scale shrinks until every column fits again (a few
    // rounds: each can only make more bars small)
    if min_h > 0.0 {
        for _ in 0..8 {
            let mut next = scale;
            for s in &sizes {
                let (mut small, mut large) = (0usize, 0u64);
                for &n in s.iter().filter(|&&n| n > 0) {
                    if n as f32 * scale < min_h {
                        small += 1;
                    } else {
                        large += n;
                    }
                }
                let room = height - gap * bars(s).saturating_sub(1) as f32 - min_h * small as f32;
                if large > 0 && room > 0.0 {
                    next = next.min(room / large as f32);
                }
            }
            if next >= scale {
                break;
            }
            scale = next;
        }
    }
    // [column][class] how much taller than its pixels a bar was made (1: as tall)
    let mut stretch = vec![vec![1f32; k_n]; cols];
    let nodes: Vec<Vec<Node>> = sizes
        .iter()
        .enumerate()
        .map(|(c, s)| {
            let mut y = 0.0;
            let mut col = Vec::new();
            for (k, &n) in s.iter().enumerate().filter(|(_, n)| **n > 0) {
                let h = (n as f32 * scale).max(min_h);
                stretch[c][k] = h / (n as f32 * scale);
                col.push(Node { k, y0: y, y1: y + h, n });
                y += h + gap;
            }
            col
        })
        .collect();
    let top = |c: usize, k: usize| nodes[c].iter().find(|n| n.k == k).map_or(0.0, |n| n.y0);
    let mut ribbons = Vec::new();
    for (c, m) in mats.iter().enumerate() {
        // where the next ribbon leaves each bar of this column and comes into each of the next
        let mut out: Vec<f32> = (0..k_n).map(|k| top(c, k)).collect();
        let mut inc: Vec<f32> = (0..k_n).map(|k| top(c + 1, k)).collect();
        for i in (0..k_n).filter(|&i| on(i)) {
            for j in (0..k_n).filter(|&j| on(j)) {
                let n = m[i * k_n + j];
                if n == 0 {
                    continue;
                }
                let h = n as f32 * scale * stretch[c][i];
                ribbons.push(Ribbon { col: c, i, j, n, a: (out[i], out[i] + h), b: (0.0, 0.0) });
                out[i] += h;
            }
        }
        // into the next bars in the order of the classes they came from, so that ribbons do not cross inside a bar
        let mut order: Vec<usize> = (0..ribbons.len()).filter(|&r| ribbons[r].col == c).collect();
        order.sort_by_key(|&r| (ribbons[r].j, ribbons[r].i));
        for r in order {
            let rb = &mut ribbons[r];
            let h = rb.n as f32 * scale * stretch[c + 1][rb.j];
            rb.b = (inc[rb.j], inc[rb.j] + h);
            inc[rb.j] += h;
        }
    }
    Layout { nodes, ribbons }
}

/// The classes (of `k_n`) to group: those `shown` under `share` of the pixels at every date of `mats` (`both`: the
/// pixels with a class at the two dates of each), when there are two or more of them.
pub fn small_classes(mats: &[Vec<u32>], both: &[u64], k_n: usize, shown: &[bool], share: f64) -> Vec<bool> {
    let mut most = vec![0f64; k_n];
    for (m, &b) in mats.iter().zip(both) {
        let b = b.max(1) as f64;
        for k in 0..k_n {
            let row: u64 = (0..k_n).map(|j| u64::from(m[k * k_n + j])).sum();
            let col: u64 = (0..k_n).map(|i| u64::from(m[i * k_n + k])).sum();
            most[k] = most[k].max(row.max(col) as f64 / b);
        }
    }
    let small: Vec<bool> = (0..k_n).map(|k| shown.get(k).copied().unwrap_or(true) && most[k] * 100.0 < share).collect();
    if small.iter().filter(|&&s| s).count() >= 2 { small } else { vec![false; k_n] }
}

/// `mats` with the `small` classes added up into one more, the last: (K + 1) x (K + 1).
pub fn group(mats: &[Vec<u32>], k_n: usize, small: &[bool]) -> Vec<Vec<u32>> {
    let g = k_n + 1;
    let to = |k: usize| if small[k] { k_n } else { k };
    mats.iter()
        .map(|m| {
            let mut out = vec![0u32; g * g];
            for i in 0..k_n {
                for j in 0..k_n {
                    out[to(i) * g + to(j)] += m[i * k_n + j];
                }
            }
            out
        })
        .collect()
}

/// The dates of a diagram: up to four spread over the series (the first and the last among them).
pub fn default_dates(t_n: usize) -> Vec<usize> {
    let mut d: Vec<usize> = (0..4).map(|k| k * (t_n.saturating_sub(1)) / 3).collect();
    d.dedup();
    d
}

/// The colour `u` of the way from `a` to `b`.
fn mix(a: Color32, b: Color32, u: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * u).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// A ribbon's band from `a` to `b` between x0 and x1, an S curve, as a mesh shaded from `from` to `to`.
fn ribbon_mesh(x0: f32, x1: f32, a: (f32, f32), b: (f32, f32), from: Color32, to: Color32) -> Mesh {
    const STEPS: usize = 32;
    let mut mesh = Mesh::default();
    for s in 0..=STEPS {
        let u = s as f32 / STEPS as f32;
        let e = u * u * (3.0 - 2.0 * u);
        let x = x0 + (x1 - x0) * u;
        let color = mix(from, to, u);
        mesh.colored_vertex(Pos2::new(x, a.0 + (b.0 - a.0) * e), color);
        mesh.colored_vertex(Pos2::new(x, a.1 + (b.1 - a.1) * e), color);
        if s > 0 {
            let v = (2 * s) as u32;
            mesh.add_triangle(v - 2, v - 1, v);
            mesh.add_triangle(v - 1, v + 1, v);
        }
    }
    mesh
}

impl State {
    /// The diagram's window (Open in a window): the same diagram, as large as the window.
    pub(crate) fn class_flows_window(&mut self, ctx: &egui::Context) {
        if !self.analysis.flow_window {
            return;
        }
        if self.session.is_none() {
            self.analysis.flow_window = false;
            return;
        }
        // a layer without classes (yet: they are found as it opens): nothing to draw, the window kept for it
        if !self.classes.on() || self.classes.list.is_empty() {
            return;
        }
        let mut open = true;
        let screen = ctx.content_rect();
        egui::Window::new("Class flows")
            .id(egui::Id::new("class flows window"))
            .open(&mut open)
            .resizable(true)
            .collapsible(false)
            .default_size(vec2((screen.width() * 0.75).min(1400.0), (screen.height() * 0.75).min(900.0)))
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .min_size(vec2(420.0, 280.0))
            .show(ctx, |ui| {
                let size = ui.available_size();
                self.class_flows_ui(ui, size, true);
            });
        if !open {
            self.analysis.flow_window = false;
        }
    }

    /// The Sankey diagram in a `size` area: the dates and the options above it, the diagram, a tooltip on a bar or
    /// a ribbon. `in_window`: drawn in its window (the panel then only says so).
    pub(crate) fn class_flows_ui(&mut self, ui: &mut Ui, size: egui::Vec2, in_window: bool) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let t_n = info.t();
        if t_n < 2 {
            ui.weak("One date: no flows.");
            return;
        }
        if self.analysis.flow_window && !in_window {
            ui.add_space(8.0);
            ui.weak("The flows are in their window.");
            if ui.button("Back to the panel").clicked() {
                self.analysis.flow_window = false;
            }
            return;
        }
        let a = &mut self.analysis;
        if a.flow_dates.len() < 2 || a.flow_dates.iter().any(|&t| t >= t_n) {
            a.flow_dates = default_dates(t_n);
        }
        let w = ui.text_style_height(&egui::TextStyle::Body) * 6.5;
        let (mut remove, mut add) = (None, false);
        ui.horizontal_wrapped(|ui| {
            let n = a.flow_dates.len();
            for (k, t) in a.flow_dates.iter_mut().enumerate() {
                egui::ComboBox::from_id_salt(("flow date", k)).width(w).selected_text(&info.layers[*t].label).show_ui(
                    ui,
                    |ui| {
                        for (i, l) in info.layers.iter().enumerate() {
                            ui.selectable_value(t, i, &l.label);
                        }
                    },
                );
                if n > 2 && ui.small_button("x").on_hover_text("Removes this date").clicked() {
                    remove = Some(k);
                }
            }
            if n < MAX_DATES {
                add = ui.small_button("+").on_hover_text("Adds the current date").clicked();
            }
            ui.separator();
            ui.checkbox(&mut a.flow_changes, "Changes only").on_hover_text(
                "Leaves out the pixels that kept their class: the bars and ribbons are the pixels that changed, at \
                 a scale of their own (the shares in the tooltips stay of all the pixels)",
            );
            let label = |i: usize| if SMALL[i] > 0.0 { format!("Other < {}%", SMALL[i]) } else { "No Other".into() };
            egui::ComboBox::from_id_salt("flow small")
                .selected_text(label(a.flow_small.min(SMALL.len() - 1)))
                .show_ui(ui, |ui| {
                    for i in 0..SMALL.len() {
                        ui.selectable_value(&mut a.flow_small, i, label(i));
                    }
                })
                .response
                .on_hover_text(
                    "The classes under this share of the pixels at every date grouped as one, Other (grey): \
                     fewer slivers. Its tooltip names them.",
                );
            ui.separator();
            let z = a.flow_zoom.max(1.0);
            if ui.add_enabled(z > 1.0, egui::Button::new("-").small()).on_hover_text("Zoom out").clicked() {
                a.flow_zoom = (z / 1.5).max(1.0);
                a.flow_pan /= 1.5;
            }
            ui.label(format!("{z:.1}x")).on_hover_text("The diagram's zoom");
            if ui.add_enabled(z < MAX_ZOOM, egui::Button::new("+").small()).on_hover_text("Zoom in").clicked() {
                a.flow_zoom = (z * 1.5).min(MAX_ZOOM);
                a.flow_pan *= 1.5;
            }
            if ui.add_enabled(z > 1.0, egui::Button::new("Fit").small()).on_hover_text("The whole diagram").clicked() {
                (a.flow_zoom, a.flow_pan) = (1.0, 0.0);
            }
            if !in_window
                && ui
                    .small_button("Open in a window")
                    .on_hover_text("The diagram in a window of its own, as large as you make it")
                    .clicked()
            {
                a.flow_window = true;
            }
            ui.weak("(?)").on_hover_text(
                "A column per date, a bar per class as tall as its pixels, a ribbon per change as wide as the \
                 pixels that made it, shaded from the class they left to the one they joined; the pixels that kept \
                 their class fainter. Counted on the overview, one pair of consecutive dates at a time: the ribbons \
                 meet at each date's bars, but a pixel is not followed across several dates.\nThe wheel zooms on \
                 the pointer, a drag moves the diagram, a double click shows it whole.",
            );
        });
        if let Some(k) = remove {
            a.flow_dates.remove(k);
        }
        if add {
            a.flow_dates.push(self.display.t);
        }
        a.flow_dates.sort_unstable();
        a.flow_dates.dedup();
        // the pairs counted again when their dates or the classes change
        let key = a.classes.key;
        let dates = a.flow_dates.clone();
        let pairs: Vec<(usize, usize)> = dates.windows(2).map(|w| (w[0], w[1])).collect();
        let stale =
            |tr: &Transitions, &(f, t): &(usize, usize)| tr.key != key || tr.from != f || tr.to != t || !tr.done;
        if self.analysis.flows.len() != pairs.len()
            || self.analysis.flows.iter().zip(&pairs).any(|(tr, p)| stale(tr, p))
        {
            let flows: Vec<Transitions> = pairs
                .iter()
                .enumerate()
                .map(|(k, &(f, t))| match self.analysis.flows.get(k) {
                    Some(tr) if !stale(tr, &(f, t)) => std::mem::take(&mut self.analysis.flows[k]),
                    _ => {
                        let mut tr = self.count_transitions(f, t);
                        tr.key = key;
                        tr
                    }
                })
                .collect();
            self.analysis.flows = flows;
        }
        let flows = &self.analysis.flows;
        if flows.iter().any(|tr| !tr.done) {
            ui.weak("Waiting for the dates in the overview...");
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }
        let Some(values) = flows.first().map(|tr| tr.values.clone()) else { return };
        let k_n = values.len();
        let both: Vec<u64> = flows.iter().map(|tr| tr.both).collect();
        let shown0: Vec<bool> = values.iter().map(|&v| self.classes.find(v).is_none_or(|e| e.visible)).collect();
        let mut names: Vec<String> =
            values.iter().map(|&v| self.classes.find(v).map_or(v.to_string(), |e| e.name.clone())).collect();
        let mut colours: Vec<Color32> = values
            .iter()
            .map(|&v| {
                self.classes
                    .find(v)
                    .map_or(Color32::from_gray(153), |e| Color32::from_rgb(e.color[0], e.color[1], e.color[2]))
            })
            .collect();
        // the small classes grouped as the last one, Other
        let raw: Vec<Vec<u32>> = flows.iter().map(|tr| tr.m.clone()).collect();
        let share = SMALL[self.analysis.flow_small.min(SMALL.len() - 1)];
        let small = if share > 0.0 { small_classes(&raw, &both, k_n, &shown0, share) } else { vec![false; k_n] };
        let grouped: Vec<String> = (0..k_n).filter(|&k| small[k]).map(|k| names[k].clone()).collect();
        let (full, k_n, shown) = if grouped.is_empty() {
            (raw, k_n, shown0)
        } else {
            names.push(format!("Other ({} classes)", grouped.len()));
            colours.push(Color32::from_gray(140));
            let mut shown = shown0;
            shown.push(true);
            (group(&raw, k_n, &small), k_n + 1, shown)
        };
        let changes = self.analysis.flow_changes;
        let mats: Vec<Vec<u32>> = full
            .iter()
            .map(|m| {
                let mut m = m.clone();
                if changes {
                    // the pixels that kept their class out
                    for k in 0..k_n {
                        m[k * k_n + k] = 0;
                    }
                }
                m
            })
            .collect();

        let body = ui.text_style_height(&egui::TextStyle::Body);
        ui.add_space(4.0);
        let (rect, resp) =
            ui.allocate_exact_size(vec2(size.x, ui.available_height().max(80.0)), Sense::click_and_drag());
        // the class names and their shares beside the first and the last columns, the dates above
        let font = egui::TextStyle::Body.resolve(ui.style());
        let small_font = egui::FontId::new(font.size * 0.9, font.family.clone());
        let widest = names
            .iter()
            .enumerate()
            .filter(|(k, _)| shown[*k])
            .map(|(_, n)| ui.fonts_mut(|f| f.layout_no_wrap(n.clone(), font.clone(), Color32::WHITE).size().x))
            .fold(0.0, f32::max);
        let pct_w = ui.fonts_mut(|f| f.layout_no_wrap("  00.0%".into(), small_font.clone(), Color32::WHITE).size().x);
        let side = (widest + pct_w + 10.0).min(rect.width() * 0.3);
        let area = Rect::from_min_max(rect.min + vec2(side, body * 2.0), rect.max - vec2(side, 6.0));
        // zoomed (the wheel on the pointer, the buttons), moved by a drag, whole again on a double click
        let (mut zoom, mut pan) = (self.analysis.flow_zoom.max(1.0), self.analysis.flow_pan);
        if resp.double_clicked() {
            (zoom, pan) = (1.0, 0.0);
        }
        if let Some(p) = resp.hover_pos().filter(|p| area.y_range().contains(p.y)) {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                let z = (zoom * (wheel / 200.0).exp()).clamp(1.0, MAX_ZOOM);
                let at = p.y - area.top();
                pan = (pan + at) * z / zoom - at;
                zoom = z;
            }
        }
        if resp.dragged() {
            pan -= resp.drag_delta().y;
        }
        let height = area.height() * zoom;
        pan = pan.clamp(0.0, (height - area.height()).max(0.0));
        (self.analysis.flow_zoom, self.analysis.flow_pan) = (zoom, pan);
        if zoom > 1.0 && (resp.hovered() || resp.dragged()) {
            ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
        }
        // the gaps between bars: room to tell them apart, less with many classes
        let most_bars = (0..=mats.len())
            .map(|c| {
                (0..k_n)
                    .filter(|&k| {
                        shown[k]
                            && (c > 0 && (0..k_n).any(|i| mats[c - 1][i * k_n + k] > 0)
                                || c < mats.len() && (0..k_n).any(|j| mats[c][k * k_n + j] > 0))
                    })
                    .count()
            })
            .max()
            .unwrap_or(1)
            .max(1);
        let gap = (body * 0.6).min(height * 0.3 / most_bars as f32);
        let lay = layout(&mats, k_n, &shown, height, gap, 2.0);
        if lay.nodes.iter().all(|c| c.is_empty()) {
            ui.painter().text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                if changes { "No pixel changed class between these dates." } else { "No pixels." },
                egui::TextStyle::Body.resolve(ui.style()),
                ui.visuals().weak_text_color(),
            );
            return;
        }
        // the diagram's y on the screen
        let top = area.top() - pan;
        let cols = dates.len();
        let bar = (body * 0.9).clamp(10.0, 16.0);
        let x_of = |c: usize| area.left() + (area.width() - bar) * c as f32 / (cols - 1).max(1) as f32;
        // the diagram clipped to its area (the names beside it too), the dates above it
        let painter = ui.painter_at(Rect::from_x_y_ranges(rect.x_range(), area.top() - 2.0..=area.bottom() + 2.0));
        let header = ui.painter_at(rect);
        let text = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();

        // what the pointer is over: a bar (column, class) or a ribbon
        let pointer = resp.hover_pos();
        let over_node = pointer.and_then(|p| {
            (0..cols).find_map(|c| {
                let x = x_of(c);
                (p.x >= x - 2.0 && p.x <= x + bar + 2.0)
                    .then(|| lay.nodes[c].iter().find(|n| p.y >= top + n.y0 - 1.0 && p.y <= top + n.y1 + 1.0))
                    .flatten()
                    .map(|n| (c, n.k))
            })
        });
        let over_ribbon = pointer.filter(|_| over_node.is_none()).and_then(|p| {
            // the topmost drawn: the changes are drawn over the pixels that stayed
            let hit = |r: &Ribbon| {
                let (x0, x1) = (x_of(r.col) + bar, x_of(r.col + 1));
                if p.x < x0 || p.x > x1 {
                    return false;
                }
                let u = (p.x - x0) / (x1 - x0);
                let e = u * u * (3.0 - 2.0 * u);
                let (y0, y1) = (r.a.0 + (r.b.0 - r.a.0) * e, r.a.1 + (r.b.1 - r.a.1) * e);
                p.y >= top + y0 && p.y <= top + y1.max(y0 + 1.0)
            };
            lay.ribbons
                .iter()
                .position(|r| r.i != r.j && hit(r))
                .or_else(|| lay.ribbons.iter().position(|r| r.i == r.j && hit(r)))
        });
        let (over_node, over_ribbon) = if area.y_range().contains(pointer.map_or(0.0, |p| p.y)) && !resp.dragged() {
            (over_node, over_ribbon)
        } else {
            (None, None)
        };
        let lit = |r: &Ribbon| match (over_node, over_ribbon) {
            (Some((c, k)), _) => (r.col == c && r.i == k) || (r.col + 1 == c && r.j == k),
            (_, Some(o)) => std::ptr::eq(r, &lay.ribbons[o]),
            _ => true,
        };
        let focus = over_node.is_some() || over_ribbon.is_some();
        // the pixels that stayed first, the changes over them
        for pass in [true, false] {
            for r in lay.ribbons.iter().filter(|r| (r.i == r.j) == pass) {
                let alpha = match (focus, lit(r), pass) {
                    (false, _, true) => 0.3,
                    (false, _, false) => 0.68,
                    (true, true, _) => 0.85,
                    (true, false, _) => 0.06,
                };
                let (x0, x1) = (x_of(r.col) + bar, x_of(r.col + 1));
                // off the area, or thinner than half a point (drawn, they come out as dashes)
                if top + r.a.0.min(r.b.0) > area.bottom()
                    || top + r.a.1.max(r.b.1) < area.top()
                    || (r.a.1 - r.a.0).max(r.b.1 - r.b.0) < 0.5 && !(focus && lit(r))
                {
                    continue;
                }
                let (a, b) = ((top + r.a.0, top + r.a.1), (top + r.b.0, top + r.b.1));
                let from = theme::on_plot(colours[r.i]).gamma_multiply(alpha);
                let to = theme::on_plot(colours[r.j]).gamma_multiply(alpha);
                painter.add(ribbon_mesh(x0, x1, a, b, from, to));
            }
        }
        let pct = |n: u64, of: u64| crate::analysis::percent(n as f64 / of.max(1) as f64);
        // a class's pixels at column c (leaving it, or at the last column coming to it), of all the pixels
        let at = |c: usize, k: usize| -> (u64, u64) {
            if c < cols - 1 {
                ((0..k_n).map(|j| u64::from(full[c][k * k_n + j])).sum(), both[c])
            } else {
                ((0..k_n).map(|i| u64::from(full[c - 1][i * k_n + k])).sum(), both[c - 1])
            }
        };
        for (c, col) in lay.nodes.iter().enumerate() {
            let x = x_of(c);
            // the labels of this column from the top down: the next one only below the last
            let mut free = f32::NEG_INFINITY;
            for n in col {
                let r = Rect::from_x_y_ranges(x..=x + bar, top + n.y0..=top + n.y1.max(n.y0 + 1.0));
                let dim = focus && over_node.is_some_and(|(oc, ok)| ok != n.k || oc.abs_diff(c) > 1);
                painter.rect_filled(r, 2.0, if dim { colours[n.k].gamma_multiply(0.45) } else { colours[n.k] });
                // the names beside the outer columns, with their share; a class at neither (changes only: what
                // changed in between) named beside its bars inside, over a backdrop
                let outer = |c: usize| lay.nodes[c].iter().any(|m| m.k == n.k);
                let inner = c > 0 && c < cols - 1 && !outer(0) && !outer(cols - 1);
                if !(c == 0 || c == cols - 1 || inner) {
                    continue;
                }
                // at the middle of what is seen of the bar (zoomed in)
                let (y0, y1) = (r.top().max(area.top()), r.bottom().min(area.bottom()));
                if y1 < y0 {
                    continue;
                }
                let y = ((y0 + y1) * 0.5).max(free + body * 0.5);
                if y > area.bottom() - body * 0.3 || y - (y0 + y1) * 0.5 > body * 1.2 {
                    continue; // no room near its bar
                }
                let (n_at, of) = at(c, n.k);
                let mut job = LayoutJob::default();
                job.append(&names[n.k], 0.0, egui::TextFormat::simple(font.clone(), text));
                job.append(&format!("  {}", pct(n_at, of)), 0.0, egui::TextFormat::simple(small_font.clone(), weak));
                let g = painter.layout_job(job);
                let (pos, align) = if c == 0 {
                    (Pos2::new(x - 6.0, y), egui::Align2::RIGHT_CENTER)
                } else {
                    (Pos2::new(x + bar + 6.0, y), egui::Align2::LEFT_CENTER)
                };
                let place = align.anchor_size(pos, g.size());
                if inner {
                    painter.rect_filled(
                        place.expand2(vec2(3.0, 1.0)),
                        3.0,
                        ui.visuals().window_fill.gamma_multiply(0.85),
                    );
                }
                painter.galley(place.min, g, text);
                free = place.bottom();
            }
            header.text(
                Pos2::new(x + bar * 0.5, rect.top() + body * 0.8),
                egui::Align2::CENTER_CENTER,
                &info.layers[dates[c]].label,
                egui::FontId::new(font.size * 1.08, font.family.clone()),
                ui.visuals().strong_text_color(),
            );
        }
        let other = |k: usize| {
            if k == k_n - 1 && !grouped.is_empty() {
                let mut list = grouped.iter().take(12).cloned().collect::<Vec<_>>().join(", ");
                if grouped.len() > 12 {
                    list += ", ...";
                }
                format!("\n({list})")
            } else {
                String::new()
            }
        };
        if let Some((c, k)) = over_node {
            // the class at that date: its pixels, where they came from and went to
            let (n_at, of) = at(c, k);
            let mut tip =
                format!("{}: {}{}\n{} of the pixels", info.layers[dates[c]].label, names[k], other(k), pct(n_at, of));
            if c > 0 {
                let stayed = full[c - 1][k * k_n + k];
                let came: u64 = (0..k_n).filter(|&i| i != k).map(|i| u64::from(full[c - 1][i * k_n + k])).sum();
                tip += &format!(
                    "\nsince {}: {} stayed, {} came from other classes",
                    info.layers[dates[c - 1]].label,
                    pct(u64::from(stayed), both[c - 1]),
                    pct(came, both[c - 1])
                );
            }
            if c < cols - 1 {
                let left: u64 = (0..k_n).filter(|&j| j != k).map(|j| u64::from(full[c][k * k_n + j])).sum();
                tip +=
                    &format!("\nto {}: {} went to other classes", info.layers[dates[c + 1]].label, pct(left, both[c]));
            }
            resp.on_hover_text_at_pointer(tip);
        } else if let Some(o) = over_ribbon {
            let r = &lay.ribbons[o];
            resp.on_hover_text_at_pointer(format!(
                "{} -> {}\n{} -> {}: {} of the pixels\n{} pixels of the overview",
                info.layers[dates[r.col]].label,
                info.layers[dates[r.col + 1]].label,
                names[r.i],
                names[r.j],
                pct(u64::from(r.n), both[r.col]),
                r.n
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flows() {
        // 3 classes, 3 dates: 0 forest, 1 pasture, 2 water (hidden in the last case)
        let k = 3;
        let m1 = vec![60, 30, 0, 0, 10, 0, 0, 0, 20]; // forest -> pasture 30
        let m2 = vec![50, 10, 0, 5, 35, 0, 0, 0, 20]; // forest -> pasture 10, pasture -> forest 5
        let l = layout(&[m1.clone(), m2.clone()], k, &[true; 3], 120.0, 0.0, 0.0);
        // one scale: 120 pixels at each date in 120 points
        assert_eq!(
            l.nodes[0],
            vec![
                Node { k: 0, y0: 0.0, y1: 90.0, n: 90 },
                Node { k: 1, y0: 90.0, y1: 100.0, n: 10 },
                Node { k: 2, y0: 100.0, y1: 120.0, n: 20 }
            ]
        );
        assert_eq!(l.nodes[1][0], Node { k: 0, y0: 0.0, y1: 60.0, n: 60 });
        assert_eq!(l.nodes[2][1], Node { k: 1, y0: 55.0, y1: 100.0, n: 45 });
        // forest -> pasture leaves forest's bar under forest -> forest and comes into pasture's over pasture ->
        // pasture
        let fp = l.ribbons.iter().find(|r| r.col == 0 && r.i == 0 && r.j == 1).unwrap();
        assert_eq!((fp.a, fp.b), ((60.0, 90.0), (60.0, 90.0)));
        let pp = l.ribbons.iter().find(|r| r.col == 0 && r.i == 1 && r.j == 1).unwrap();
        assert_eq!(pp.b, (90.0, 100.0));
        // every ribbon into a bar fills it
        for c in 1..3 {
            for n in &l.nodes[c] {
                let h: f32 = l.ribbons.iter().filter(|r| r.col + 1 == c && r.j == n.k).map(|r| r.b.1 - r.b.0).sum();
                assert!((h - (n.y1 - n.y0)).abs() < 1e-3);
            }
        }
        // water hidden: 100 pixels in 100 points, with a gap of 2 between the two bars
        let l = layout(&[m1, m2], k, &[true, true, false], 102.0, 2.0, 0.0);
        assert_eq!(l.nodes[0].len(), 2);
        assert_eq!(l.nodes[0][1], Node { k: 1, y0: 92.0, y1: 102.0, n: 10 });
        assert!(l.ribbons.iter().all(|r| r.i != 2 && r.j != 2));
        assert_eq!(default_dates(41), vec![0, 13, 26, 40]);
        assert_eq!(default_dates(2), vec![0, 1]);
    }

    #[test]
    fn rare_classes_and_other() {
        // 4 classes, 2 dates: two of 1 pixel among 1000
        let k = 4;
        let m = vec![900, 0, 0, 0, 0, 97, 0, 0, 0, 0, 1, 1, 0, 0, 0, 1];
        let ms = [m];
        let l = layout(&ms, k, &[true; 4], 100.0, 2.0, 3.0);
        for col in &l.nodes {
            // every bar at least 3 points, the column within the height
            assert!(col.iter().all(|n| n.y1 - n.y0 >= 3.0 - 1e-4));
            assert!(col.last().unwrap().y1 <= 100.0 + 1e-3);
            // the ribbons out of a bar fill it, though it was stretched
            for n in col {
                let h: f32 = l.ribbons.iter().filter(|r| r.i == n.k).map(|r| r.a.1 - r.a.0).sum();
                if n.n > 0 && l.nodes[0].contains(n) {
                    assert!((h - (n.y1 - n.y0)).abs() < 1e-3, "{n:?} {h}");
                }
            }
        }
        // under 0.5%: classes 2 and 3 grouped as a fifth
        let small = small_classes(&ms, &[1000], k, &[true; 4], 0.5);
        assert_eq!(small, [false, false, true, true]);
        let g = group(&ms, k, &small);
        assert_eq!(g[0][4 * 5 + 4], 3);
        assert_eq!(g[0][0], 900);
        assert_eq!(g[0].iter().sum::<u32>(), 1000);
        // a single small class is not grouped
        assert_eq!(small_classes(&ms, &[1000], k, &[true, true, true, false], 0.5), [false; 4]);
    }
}
