//! The label mode "Fraction cover" of the Interpretation panel: an n x n subgrid over the pixel of the point, painted
//! over the images (the Wayback cards, the lightbox, the cube's cards) with the active class, one painting for each
//! year of the image painted over. This is the pure part (which cell is under the cursor, a stroke, the painting
//! applied to a label, the tables) and the card input.
//!
//! Fraction cover follows Validation Viewer by Dr. Matthias Baumann (Humboldt-Universität zu Berlin),
//! https://scm.cms.hu-berlin.de/baumamat/validationviewer, GPL-2.0-or-later, used under GPL-3.0-or-later.

use std::collections::BTreeMap;

use egui::{Color32, Pos2, Rect, Ui, vec2};
use janus_validation::{Fractions, Label, NO_CELL};

use crate::chips::{Cells, Overlay};
use crate::labels_year::YearPoint;

/// The smallest and the largest side of the grid, and the default.
pub const N_RANGE: std::ops::RangeInclusive<usize> = 2..=10;

/// The opacity of a painted cell over the image.
pub const FILL_ALPHA: u8 = 115;

/// Cells painted (or erased) in one frame over the images of one year.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaintOp {
    pub year: i32,
    /// (column, row)
    pub cells: Vec<(usize, usize)>,
    pub erase: bool,
}

/// What the image cards need of the mode, and what they hand back: the paintings of the point being interpreted
/// (by year) and the strokes made.
pub(crate) struct FracUi {
    pub on: bool,
    pub n: usize,
    /// the colour of each class
    pub colors: Vec<Color32>,
    pub years: BTreeMap<i32, Fractions>,
    pub ops: Vec<PaintOp>,
}

impl FracUi {
    pub(crate) fn off() -> FracUi {
        FracUi { on: false, n: 0, colors: Vec::new(), years: BTreeMap::new(), ops: Vec::new() }
    }

    /// The cells to draw over an image of `year` (None: not in this mode).
    pub(crate) fn cells_for(&self, year: i32) -> Option<Cells> {
        if !self.on {
            return None;
        }
        let n = self.n;
        let colors = match self.years.get(&year) {
            Some(f) => {
                let f = f.resampled(n);
                f.cells.iter().map(|&c| usize::try_from(c).ok().and_then(|k| self.colors.get(k).copied())).collect()
            }
            None => vec![None; n * n],
        };
        Some(Cells { n, colors })
    }

    /// Keeps the cells a card painted (none: nothing).
    pub(crate) fn push(&mut self, year: i32, input: &PaintInput) {
        if year != 0 && !input.cells.is_empty() {
            self.ops.push(PaintOp { year, cells: input.cells.clone(), erase: input.erase });
        }
    }
}

/// The pixel's footprint on the screen, for an image drawn in `img`.
pub(crate) fn pixel_rect(img: Rect, o: &Overlay) -> Rect {
    let to = |p: Pos2| img.min + vec2(p.x * img.width(), p.y * img.height());
    Rect::from_min_max(to(o.pixel.min), to(o.pixel.max))
}

/// The cell (column, row) of an n x n grid over `pix` under `p`, if `p` is inside it.
pub(crate) fn cell_at(p: Pos2, pix: Rect, n: usize) -> Option<(usize, usize)> {
    if n == 0 || pix.width() <= 0.0 || pix.height() <= 0.0 {
        return None;
    }
    if !(p.x >= pix.left() && p.x < pix.right() && p.y >= pix.top() && p.y < pix.bottom()) {
        return None;
    }
    let at = |v: f32, lo: f32, size: f32| (((v - lo) / size * n as f32) as usize).min(n - 1);
    Some((at(p.x, pix.left(), pix.width()), at(p.y, pix.top(), pix.height())))
}

/// The cells a stroke from `a` to `b` crosses inside the pixel, in order, each once (no cell is skipped however fast
/// the pointer moved).
pub(crate) fn stroke_cells(a: Pos2, b: Pos2, pix: Rect, n: usize) -> Vec<(usize, usize)> {
    if n == 0 {
        return Vec::new();
    }
    let step = (pix.width().min(pix.height()) / n as f32 / 2.0).max(0.25);
    let k = (((b - a).length() / step).ceil() as usize).clamp(1, 4000);
    let mut out: Vec<(usize, usize)> = Vec::new();
    for i in 0..=k {
        let p = a + (b - a) * (i as f32 / k as f32);
        if let Some(c) = cell_at(p, pix, n)
            && !out.contains(&c)
        {
            out.push(c);
        }
    }
    out
}

/// What a card's input did to the painting this frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct PaintInput {
    pub cells: Vec<(usize, usize)>,
    pub erase: bool,
    /// a stroke is going on (or ended this frame): the card neither pans nor takes the click
    pub active: bool,
}

/// The input of a card in the mode: pressing inside the pixel starts a stroke with the left button (paints) or the
/// right one (erases) and dragging goes on over the cells; X erases the cell under the cursor. Outside the pixel,
/// or with Shift, nothing here: the drag is the pan's.
pub(crate) fn paint_input(ui: &Ui, resp: &egui::Response, img: Rect, o: &Overlay) -> PaintInput {
    let mut out = PaintInput::default();
    let Some(cells) = &o.cells else { return out };
    let (pix, n) = (pixel_rect(img, o), cells.n);
    let id = resp.id.with("fraction stroke");
    let mut session: Option<(Pos2, bool)> = ui.data(|d| d.get_temp(id));
    let (pos, shift, down, pressed) = ui.input(|i| {
        use egui::PointerButton::{Primary, Secondary};
        (
            i.pointer.latest_pos(),
            i.modifiers.shift,
            [i.pointer.button_down(Primary), i.pointer.button_down(Secondary)],
            [i.pointer.button_pressed(Primary), i.pointer.button_pressed(Secondary)],
        )
    });
    if session.is_none()
        && !shift
        && resp.contains_pointer()
        && let Some(p) = pos
        && cell_at(p, pix, n).is_some()
        && (pressed[0] || pressed[1])
    {
        // a stroke starts: its first cell is the one under the cursor
        let erase = !pressed[0];
        session = Some((p, erase));
        out.active = true;
        out.erase = erase;
        out.cells = stroke_cells(p, p, pix, n);
        if !down[usize::from(erase)] {
            session = None;
        }
    } else if let Some((last, erase)) = session {
        out.active = true;
        out.erase = erase;
        if down[usize::from(erase)] {
            let now = pos.unwrap_or(last);
            out.cells = stroke_cells(last, now, pix, n);
            session = Some((now, erase));
        } else {
            session = None;
        }
    }
    // X: the cell under the cursor
    if session.is_none()
        && out.cells.is_empty()
        && resp.contains_pointer()
        && !ui.ctx().egui_wants_keyboard_input()
        && let Some(c) = pos.and_then(|p| cell_at(p, pix, n))
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::X))
    {
        out.cells = vec![c];
        out.erase = true;
    }
    ui.data_mut(|d| match session {
        Some(s) => {
            d.insert_temp(id, s);
        }
        None => {
            d.remove_temp::<(Pos2, bool)>(id);
        }
    });
    if out.active {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        ui.ctx().request_repaint();
    }
    out
}

/// Paints (class >= 0) or erases (`NO_CELL`) `cells` of the painting of `year` in a label, a new painting when the
/// year had none and a resampled one when the grid's side changed; the reference follows: when the label is empty
/// or was the one the cells stood for, it becomes the one they stand for now. True when something changed.
pub(crate) fn apply_paint(
    l: &mut Label,
    year: i32,
    n: usize,
    cells: &[(usize, usize)],
    class: i32,
    label_set: &[String],
    now: &str,
) -> bool {
    let before = l.fraction_reference(label_set).map(str::to_string);
    let mut f = l.fractions.get(&year).map_or_else(|| Fractions::new(n), |f| f.resampled(n));
    let mut changed = false;
    for &(c, r) in cells {
        changed |= f.set(c, r, class);
    }
    if !changed {
        return false;
    }
    f.updated = now.to_string();
    if f.is_empty() {
        l.fractions.remove(&year);
    } else {
        l.fractions.insert(year, f);
    }
    let after = l.fraction_reference(label_set).map(str::to_string);
    if l.label.is_empty() || Some(&l.label) == before.as_ref() {
        l.label = after.unwrap_or_default();
    }
    true
}

/// Clears the painting of a year; the reference follows as in `apply_paint`.
pub(crate) fn clear_year(l: &mut Label, year: i32, label_set: &[String]) -> bool {
    let before = l.fraction_reference(label_set).map(str::to_string);
    if l.fractions.remove(&year).is_none() {
        return false;
    }
    let after = l.fraction_reference(label_set).map(str::to_string);
    if Some(&l.label) == before.as_ref() {
        l.label = after.unwrap_or_default();
    }
    true
}

/// The year of the painting the Validation card shows: the one painted last, else the latest of the label.
pub(crate) fn table_year(l: &Label, last_painted: Option<i32>) -> Option<i32> {
    last_painted
        .filter(|y| l.fractions.contains_key(y))
        .or_else(|| l.fractions.iter().rev().find(|(_, f)| !f.is_empty()).map(|(y, _)| *y))
}

/// A row of the table of a painting: a class, its cells and its share of the n x n.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FracRow {
    pub class: usize,
    pub cells: usize,
    pub pct: f32,
}

/// The cells of each class of a painting and their share of the whole grid.
pub(crate) fn year_table(f: &Fractions, classes: usize) -> Vec<FracRow> {
    let total = (f.n * f.n).max(1) as f32;
    f.counts(classes)
        .into_iter()
        .enumerate()
        .map(|(class, cells)| FracRow { class, cells, pct: 100.0 * cells as f32 / total })
        .collect()
}

/// The names of the columns of the fractions of a table: frac_<class> for each class, frac_n, frac_labelled
/// (the share of cells with a class) and frac_year, of the latest year painted.
pub(crate) fn frac_columns(classes: &[String]) -> Vec<String> {
    classes
        .iter()
        .map(|c| format!("frac_{c}"))
        .chain(["frac_n", "frac_labelled", "frac_year"].map(String::from))
        .collect()
}

/// The value of column k of `frac_columns(classes)` for a label ("" when nothing is painted).
pub(crate) fn frac_value(l: &Label, classes: &[String], k: usize) -> String {
    let Some((y, f)) = l.fractions.iter().rev().find(|(_, f)| !f.is_empty()) else { return String::new() };
    let total = (f.n * f.n) as f64;
    if k < classes.len() {
        format!("{:.4}", f.counts(classes.len())[k] as f64 / total)
    } else {
        match k - classes.len() {
            0 => f.n.to_string(),
            1 => format!("{:.4}", f.labelled() as f64 / total),
            _ => y.to_string(),
        }
    }
}

/// One row for each point and year painted: pid, lon, lat, year, n, labelled (the share of cells with a class),
/// frac_<class> for each class (the share of the n x n) and the interpreter.
pub(crate) fn fractions_csv(points: &[YearPoint], classes: &[String], csv_field: &dyn Fn(&str) -> String) -> String {
    let mut out = String::from("pid,lon,lat,year,n,labelled");
    for c in classes {
        out += &format!(",{}", csv_field(&format!("frac_{c}")));
    }
    out += ",interpreter\n";
    for p in points {
        let (lon, lat) =
            p.lon_lat.map_or((String::new(), String::new()), |(a, b)| (format!("{a:.8}"), format!("{b:.8}")));
        for (y, f) in p.label.fractions.iter().filter(|(_, f)| !f.is_empty()) {
            let total = (f.n * f.n) as f64;
            out += &format!("{},{lon},{lat},{y},{},{:.4}", csv_field(&p.pid), f.n, f.labelled() as f64 / total);
            for c in f.counts(classes.len()) {
                out += &format!(",{:.4}", c as f64 / total);
            }
            out += &format!(",{}\n", csv_field(p.interpreter));
        }
    }
    out
}

/// The file of the fractions beside a table: <name>_fractions.csv.
pub(crate) fn fractions_path(table: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(table);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    p.with_file_name(format!("{stem}_fractions.csv"))
}

/// Whether any label has cells painted (a table of fractions is worth writing).
pub(crate) fn any_fractions<'a>(labels: impl IntoIterator<Item = &'a Label>) -> bool {
    labels.into_iter().any(|l| !l.fractions.is_empty())
}

/// `NO_CELL` re-exported for the callers that erase.
pub(crate) const ERASE: i32 = NO_CELL;

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn set() -> Vec<String> {
        ["Forest", "Water", "Soil"].map(String::from).into()
    }

    #[test]
    fn the_cell_under_the_cursor() {
        // a pixel of 100 x 50 points at (10, 20), 5 x 5 cells
        let pix = Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 50.0));
        assert_eq!(cell_at(pos2(10.0, 20.0), pix, 5), Some((0, 0)));
        assert_eq!(cell_at(pos2(29.9, 29.9), pix, 5), Some((0, 0)));
        assert_eq!(cell_at(pos2(30.0, 30.0), pix, 5), Some((1, 1)));
        assert_eq!(cell_at(pos2(109.9, 69.9), pix, 5), Some((4, 4)));
        // the far edges and outside are not in
        assert_eq!(cell_at(pos2(110.0, 30.0), pix, 5), None);
        assert_eq!(cell_at(pos2(9.9, 30.0), pix, 5), None);
        assert_eq!(cell_at(pos2(50.0, 70.0), pix, 5), None);
        assert_eq!(cell_at(pos2(50.0, 30.0), pix, 0), None);
    }

    #[test]
    fn the_cell_with_zoom_and_pan() {
        // the pixel's footprint is a fraction of the card (0.4..0.6 of 200 points) under a view of 2x panned by -0.5
        let v = crate::chips::View { z: 2.0, x: -0.5, y: -0.25 };
        let o = Overlay::new(12, 25, pos2(0.5, 0.5), v, true);
        let img = Rect::from_min_size(pos2(100.0, 100.0), vec2(200.0, 200.0));
        let pix = pixel_rect(img, &o);
        // the pixel (12, 12) of 25: 0.48..0.52 of the window, times z, plus the offset
        assert!((pix.left() - (100.0 + (0.48 * 2.0 - 0.5) * 200.0)).abs() < 1e-3);
        assert!((pix.top() - (100.0 + (0.48 * 2.0 - 0.25) * 200.0)).abs() < 1e-3);
        assert!((pix.width() - 0.04 * 2.0 * 200.0).abs() < 1e-3);
        // the centre of the pixel is in the middle cell of 5
        assert_eq!(cell_at(pix.center(), pix, 5), Some((2, 2)));
        assert_eq!(cell_at(pix.left_top() + vec2(0.1, 0.1), pix, 5), Some((0, 0)));
        assert_eq!(cell_at(pix.right_bottom() - vec2(0.1, 0.1), pix, 5), Some((4, 4)));
    }

    #[test]
    fn a_stroke_misses_no_cell() {
        let pix = Rect::from_min_size(pos2(0.0, 0.0), vec2(50.0, 50.0));
        // a fast drag along the middle row, from outside the pixel to outside it
        let row = stroke_cells(pos2(-20.0, 25.0), pos2(80.0, 25.0), pix, 5);
        assert_eq!(row, (0..5).map(|c| (c, 2)).collect::<Vec<_>>());
        // a single point is a single cell
        assert_eq!(stroke_cells(pos2(5.0, 45.0), pos2(5.0, 45.0), pix, 5), vec![(0, 4)]);
        // a stroke outside paints nothing
        assert!(stroke_cells(pos2(60.0, 0.0), pos2(90.0, 40.0), pix, 5).is_empty());
        // a diagonal goes through the diagonal cells (and may touch the neighbours at the corners)
        let d = stroke_cells(pos2(1.0, 1.0), pos2(49.0, 49.0), pix, 5);
        assert!((0..5).all(|k| d.contains(&(k, k))));
    }

    #[test]
    fn painting_a_label() {
        let mut l = Label::default();
        let cells: Vec<(usize, usize)> = (0..2).flat_map(|c| (0..2).map(move |r| (c, r))).collect();
        // 3 of 4 cells: not done, no reference yet
        assert!(apply_paint(&mut l, 2019, 2, &cells[..3], 1, &set(), "t1"));
        assert_eq!(l.fractions[&2019].labelled(), 3);
        assert!(l.label.is_empty() && !l.fraction_done());
        // the same again changes nothing
        assert!(!apply_paint(&mut l, 2019, 2, &cells[..3], 1, &set(), "t2"));
        assert_eq!(l.fractions[&2019].updated, "t1");
        // the last cell: done, the reference is the majority
        assert!(apply_paint(&mut l, 2019, 2, &cells[3..], 2, &set(), "t3"));
        assert!(l.fraction_done());
        assert_eq!(l.label, "Water");
        // the reference follows while it was the derived one
        assert!(apply_paint(&mut l, 2019, 2, &cells[..2], 2, &set(), "t4"));
        assert_eq!(l.label, "Soil");
        // a label chosen by hand stays
        l.label = "Forest".into();
        assert!(apply_paint(&mut l, 2019, 2, &cells[..3], 1, &set(), "t5"));
        assert_eq!(l.label, "Forest");
        // erasing: the cells go, and the painting with the last one
        assert!(apply_paint(&mut l, 2019, 2, &cells, ERASE, &set(), "t6"));
        assert!(l.fractions.is_empty());
        // another grid resamples what was painted
        let mut m = Label::default();
        apply_paint(&mut m, 2020, 2, &[(0, 0)], 0, &set(), "a");
        apply_paint(&mut m, 2020, 4, &[(3, 3)], 1, &set(), "b");
        let f = &m.fractions[&2020];
        assert_eq!((f.n, f.get(0, 0), f.get(1, 1), f.get(3, 3)), (4, 0, 0, 1));
        // clearing a year
        assert!(clear_year(&mut m, 2020, &set()));
        assert!(!clear_year(&mut m, 2020, &set()));
    }

    #[test]
    fn the_year_of_the_table_and_its_rows() {
        let mut l = Label::default();
        apply_paint(&mut l, 2015, 2, &[(0, 0), (1, 0)], 0, &set(), "a");
        apply_paint(&mut l, 2020, 2, &[(0, 0)], 1, &set(), "b");
        assert_eq!(table_year(&l, Some(2015)), Some(2015));
        assert_eq!(table_year(&l, Some(1999)), Some(2020));
        assert_eq!(table_year(&l, None), Some(2020));
        assert_eq!(table_year(&Label::default(), Some(2015)), None);
        let rows = year_table(&l.fractions[&2015], 3);
        assert_eq!(rows[0], FracRow { class: 0, cells: 2, pct: 50.0 });
        assert_eq!(rows[1].cells, 0);
    }

    #[test]
    fn the_columns_of_the_tables() {
        let classes = set();
        let mut l = Label::default();
        assert_eq!(
            frac_columns(&classes),
            ["frac_Forest", "frac_Water", "frac_Soil", "frac_n", "frac_labelled", "frac_year"]
        );
        assert!((0..6).all(|k| frac_value(&l, &classes, k).is_empty()));
        apply_paint(&mut l, 2015, 2, &[(0, 0), (1, 0), (0, 1), (1, 1)], 0, &set(), "a");
        apply_paint(&mut l, 2020, 2, &[(0, 0), (1, 0), (0, 1)], 2, &set(), "b");
        apply_paint(&mut l, 2020, 2, &[(1, 1)], 1, &set(), "c");
        // the latest year painted: 2 of its 4 cells... 3 Soil, 1 Water
        let v: Vec<String> = (0..6).map(|k| frac_value(&l, &classes, k)).collect();
        assert_eq!(v, ["0.0000", "0.2500", "0.7500", "2", "1.0000", "2020"]);
        // a partly painted year
        let mut p = Label::default();
        apply_paint(&mut p, 2021, 4, &[(0, 0), (1, 1)], 1, &set(), "a");
        assert_eq!(frac_value(&p, &classes, 1), "0.1250");
        assert_eq!(frac_value(&p, &classes, 4), "0.1250");
        assert_eq!(frac_value(&p, &classes, 3), "4");
        // the file with a row for each point and year
        let pts = [
            YearPoint { pid: "7".into(), lon_lat: Some((-55.5, -12.25)), interpreter: "ana", label: &l },
            YearPoint { pid: "8".into(), lon_lat: None, interpreter: "b, c", label: &p },
        ];
        let csv = fractions_csv(&pts, &classes, &|s| if s.contains(',') { format!("\"{s}\"") } else { s.to_string() });
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "pid,lon,lat,year,n,labelled,frac_Forest,frac_Water,frac_Soil,interpreter");
        assert_eq!(lines[1], "7,-55.50000000,-12.25000000,2015,2,1.0000,1.0000,0.0000,0.0000,ana");
        assert_eq!(lines[2], "7,-55.50000000,-12.25000000,2020,2,1.0000,0.0000,0.2500,0.7500,ana");
        assert_eq!(lines[3], "8,,,2021,4,0.1250,0.0000,0.1250,0.0000,\"b, c\"");
        assert_eq!(lines.len(), 4);
        assert!(any_fractions([&l]) && !any_fractions([&Label::default()]));
        assert_eq!(fractions_path("/a/b/pts_labels.gpkg"), std::path::Path::new("/a/b/pts_labels_fractions.csv"));
    }

    #[test]
    fn the_cells_drawn() {
        let mut ui = FracUi::off();
        assert!(ui.cells_for(2020).is_none());
        ui.on = true;
        ui.n = 2;
        ui.colors = vec![Color32::RED, Color32::BLUE];
        let mut f = Fractions::new(2);
        f.set(1, 0, 1);
        f.set(0, 1, 5);
        ui.years.insert(2020, f);
        let c = ui.cells_for(2020).unwrap();
        assert_eq!(c.colors, vec![None, Some(Color32::BLUE), None, None]);
        assert_eq!(ui.cells_for(2021).unwrap().colors, vec![None; 4]);
        // another grid than the painted one is resampled
        ui.n = 4;
        assert_eq!(ui.cells_for(2020).unwrap().colors.iter().flatten().count(), 4);
        ui.push(2020, &PaintInput { cells: vec![(0, 0)], erase: true, active: true });
        ui.push(0, &PaintInput { cells: vec![(0, 0)], erase: true, active: true });
        ui.push(2020, &PaintInput::default());
        assert_eq!(ui.ops, vec![PaintOp { year: 2020, cells: vec![(0, 0)], erase: true }]);
    }
}
