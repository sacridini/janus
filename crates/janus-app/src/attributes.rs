//! A vector layer's attributes: a file opens with its geometries alone (OGR skips the fields: 2.5 to 4.5 times
//! faster, janus_geo::vectors::load_vector_geometry), and its fields' values are read when asked: the whole table
//! on a thread (the Attribute table panel, a class column for deep learning, an export), the feature under the
//! cursor and the one being interpreted one at a time by their FIDs. The Attribute table panel: the rows of a layer,
//! filtered and sorted, the one picked highlighted on the map.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use janus_geo::vectors::{FeatureReader, VectorData, attributes, read_attributes};

use crate::app::State;
use crate::theme;
use crate::vectors::VectorLayer;

/// A layer's table: its fields' values, a row per feature.
#[derive(Default)]
pub enum Table {
    #[default]
    NotRead,
    Reading(Reading),
    Read(Arc<Vec<Vec<String>>>),
    Failed(String),
}

/// A table being read on a thread: the rows read so far. Dropped (the layer closed), it stops.
pub struct Reading {
    rx: Receiver<Result<Vec<Vec<String>>, String>>,
    done: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A feature's fields read alone: its values, none in the file, or skipped (asked again when still wanted).
enum One {
    Values(Vec<String>),
    Missing,
    Skipped,
}

/// The thread that reads features one at a time (the file kept open): FIDs asked, values back.
struct OneReader {
    tx: Sender<(usize, i64)>,
    rx: Receiver<(usize, One)>,
    asked: HashSet<usize>,
}

impl OneReader {
    fn start(data: Arc<VectorData>) -> OneReader {
        let (tx, asks) = channel::<(usize, i64)>();
        let (back, rx) = channel();
        std::thread::Builder::new()
            .name("vector-attributes".into())
            .spawn(move || {
                let mut reader = FeatureReader::open(&data);
                while let Ok(first) = asks.recv() {
                    // the latest first; beyond the last few, skipped (the cursor went on)
                    let mut todo = vec![first];
                    todo.extend(asks.try_iter());
                    let old = todo.len().saturating_sub(4);
                    for &(i, _) in &todo[..old] {
                        let _ = back.send((i, One::Skipped));
                    }
                    for &(i, fid) in todo[old..].iter().rev() {
                        let r = match &mut reader {
                            Ok(r) => r.values(fid).map_or(One::Missing, One::Values),
                            Err(_) => One::Missing,
                        };
                        if back.send((i, r)).is_err() {
                            return;
                        }
                    }
                }
            })
            .ok();
        OneReader { tx, rx, asked: HashSet::new() }
    }
}

/// A layer's attributes as read so far.
#[derive(Default)]
pub struct LayerAttributes {
    pub table: Table,
    /// features read alone while the table is not, by index (empty: not found in the file)
    one: HashMap<usize, Vec<String>>,
    reader: Option<OneReader>,
}

impl LayerAttributes {
    /// The replies of the threads taken in; whether one is still at work.
    fn pump(&mut self) -> bool {
        if let Table::Reading(r) = &self.table {
            match r.rx.try_recv() {
                Ok(Ok(rows)) => {
                    self.table = Table::Read(Arc::new(rows));
                    // the features read alone are in it
                    (self.one, self.reader) = (HashMap::new(), None);
                }
                Ok(Err(e)) => self.table = Table::Failed(e),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.table = Table::Failed("the reading stopped".into()),
            }
        }
        if let Some(r) = &mut self.reader {
            for (i, one) in r.rx.try_iter() {
                r.asked.remove(&i);
                match one {
                    One::Values(v) => {
                        self.one.insert(i, v);
                    }
                    One::Missing => {
                        self.one.insert(i, Vec::new());
                    }
                    One::Skipped => {}
                }
            }
            if self.one.len() > 50_000 {
                self.one.clear();
            }
        }
        matches!(self.table, Table::Reading(_)) || self.reader.as_ref().is_some_and(|r| !r.asked.is_empty())
    }
}

impl VectorLayer {
    /// Feature i's row of the table: read with its geometries or in the table (None while the table is not read).
    pub fn row(&self, i: usize) -> Option<&[String]> {
        if self.data.has_values() {
            return self.data.features.get(i).map(|f| f.values.as_slice());
        }
        match &self.attrs.table {
            Table::Read(t) => t.get(i).map(Vec::as_slice),
            _ => None,
        }
    }

    /// Feature i's fields' values once read (in the table, or alone: ask_feature); empty: not found in the file.
    pub fn values(&self, i: usize) -> Option<&[String]> {
        self.row(i).or_else(|| self.attrs.one.get(&i).map(Vec::as_slice))
    }

    /// Every row is there.
    pub fn table_ready(&self) -> bool {
        self.data.has_values() || matches!(self.attrs.table, Table::Read(_))
    }

    /// The table read (for a thread: an export); None: not yet.
    pub fn table(&self) -> Option<Arc<Vec<Vec<String>>>> {
        match &self.attrs.table {
            Table::Read(t) => Some(t.clone()),
            _ => None,
        }
    }

    /// Its table read on a thread, unless it is (or is being) already.
    pub fn want_table(&mut self) {
        if self.data.has_values() || !matches!(self.attrs.table, Table::NotRead) {
            return;
        }
        let (tx, rx) = channel();
        let (done, cancel) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let (data, d, c) = (self.data.clone(), done.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("vector-table".into())
            .spawn(move || {
                let _ = tx.send(read_attributes(&data, &d, &c));
            })
            .ok();
        self.attrs.table = Table::Reading(Reading { rx, done, cancel });
    }

    /// Its table read now, on this thread (the tours of the guides).
    pub fn read_table_now(&mut self) -> Result<(), String> {
        if !self.table_ready() {
            self.attrs.table = Table::Read(Arc::new(attributes(&self.data)?));
        }
        Ok(())
    }

    /// Feature i's fields read alone on a thread, unless they are there (or asked) already.
    pub fn ask_feature(&mut self, i: usize) {
        let Some(f) = self.data.features.get(i) else { return };
        if self.values(i).is_some() || matches!(self.attrs.table, Table::Reading(_)) {
            return;
        }
        let r = self.attrs.reader.get_or_insert_with(|| OneReader::start(self.data.clone()));
        if r.asked.insert(i) {
            let _ = r.tx.send((i, f.fid));
        }
    }
}

/// What the rows shown were made of: the layer, its table, the filter, the sort.
type RowsKey = (u32, usize, String, Option<(usize, bool)>);

/// The Attribute table panel's state.
#[derive(Default)]
pub struct AttributeTableUi {
    pub show: bool,
    /// the layer shown (its id), and the feature picked in it (highlighted on the map)
    pub layer: u32,
    pub picked: Option<usize>,
    /// the panel brought to the front (asked from a layer's menu)
    pub focus: bool,
    filter: String,
    /// the column sorted by (0: the FID, k + 1: field k), descending
    sort: Option<(usize, bool)>,
    /// the rows shown in their order, and what they were made of
    rows: Vec<usize>,
    rows_for: Option<RowsKey>,
    /// the picked row scrolled to at the next frame
    scroll_to: bool,
}

/// `needle` (lower case ASCII) in `hay`, ignoring the case of ASCII letters.
fn contains_ignore_case(hay: &str, needle: &[u8]) -> bool {
    needle.is_empty() || hay.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

/// Layer v's rows that hold `filter` in a field, sorted by `sort` (numbers as numbers when every value of the column
/// is one; empty values last).
fn rows_of(v: &VectorLayer, filter: &str, sort: Option<(usize, bool)>) -> Vec<usize> {
    let n = v.data.features.len();
    let needle = filter.trim().to_ascii_lowercase();
    let mut rows: Vec<usize> = (0..n)
        .filter(|&i| {
            needle.is_empty()
                || contains_ignore_case(&v.data.features[i].fid.to_string(), needle.as_bytes())
                || v.row(i).is_some_and(|r| r.iter().any(|s| contains_ignore_case(s, needle.as_bytes())))
        })
        .collect();
    let Some((col, desc)) = sort else { return rows };
    if col == 0 {
        rows.sort_by_key(|&i| v.data.features[i].fid);
    } else {
        let text = |i: usize| v.row(i).and_then(|r| r.get(col - 1)).map_or("", String::as_str);
        let numbers: Option<Vec<f64>> = rows
            .iter()
            .map(|&i| if text(i).is_empty() { Some(f64::NAN) } else { text(i).trim().parse().ok() })
            .collect();
        match numbers {
            Some(x) => {
                let mut keyed: Vec<(f64, usize)> = x.into_iter().zip(rows.iter().copied()).collect();
                keyed.sort_by(|a, b| a.0.is_nan().cmp(&b.0.is_nan()).then(a.0.total_cmp(&b.0)));
                rows = keyed.into_iter().map(|k| k.1).collect();
            }
            None => rows.sort_by(|&a, &b| text(a).is_empty().cmp(&text(b).is_empty()).then(text(a).cmp(text(b)))),
        }
    }
    if desc {
        rows.reverse();
    }
    rows
}

impl State {
    /// The attributes being read taken in; the features whose fields are shown read alone (the one under the
    /// cursor when its tooltip shows, the one being interpreted); the tables wanted read (the panel's, deep
    /// learning's class column).
    pub(crate) fn pump_attributes(&mut self, ctx: &egui::Context) {
        if self.attr_table.show && self.vectors.is_empty() {
            self.attr_table.show = false;
        }
        let alt = ctx.input(|i| i.modifiers.alt);
        if let Some((li, i)) = self.hover_vector
            && let Some(v) = self.vectors.get_mut(li)
            && v.data.features.get(i).is_some_and(|f| f.kind == janus_geo::vectors::Kind::Point || alt)
        {
            v.ask_feature(i);
        }
        if let Some(li) = self.labels.vector.and_then(|id| self.vector_index(id))
            && let Some(c) = self.labels.current
        {
            self.vectors[li].ask_feature(c);
        }
        let mut wanted = vec![];
        if self.attr_table.show {
            wanted.push(self.attr_table.layer);
        }
        if self.ml.open && self.ml.source == 2 {
            wanted.push(self.ml.vector);
        }
        for id in wanted {
            if let Some(li) = self.vector_index(id) {
                self.vectors[li].want_table();
            }
        }
        let mut busy = false;
        for v in &mut self.vectors {
            busy |= v.attrs.pump();
        }
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// The Attribute table panel on layer `id` (a layer's menu).
    pub fn open_attribute_table(&mut self, id: u32) {
        let t = &mut self.attr_table;
        if t.layer != id {
            t.picked = None;
        }
        (t.show, t.layer, t.focus) = (true, id, true);
    }

    /// Feature i of vector layer li in view: its box with a margin (a point: 40 pixels of the active layer around).
    fn zoom_to_feature(&mut self, li: usize, i: usize) {
        let Some(g) = self.grid() else { return };
        let Some(b) = self.vectors.get(li).and_then(|v| v.boxes.get(i)).filter(|b| b[0] <= b[2]) else { return };
        let b = b.map(f64::from);
        let m = &mut self.map;
        if m.canvas[0] <= 0.0 {
            return;
        }
        let (w, h) = ((b[2] - b[0]).max(40.0), (b[3] - b[1]).max(40.0));
        let s = (0.8 * (f64::from(m.canvas[0]) / w).min(f64::from(m.canvas[1]) / h)).clamp(0.01, crate::map::MAX_ZOOM);
        m.set_view([(b[0] + b[2]) * 0.5 / g.fx, (b[1] + b[3]) * 0.5 / g.fy], s * g.fx);
        m.touched = true;
        m.dirty = true;
    }

    /// The Attribute table panel: a vector layer's rows (read when the panel first shows it), filtered by a text
    /// in any field and sorted by a column (a click on its header); a click on a row picks its feature
    /// (highlighted on the map), a double click zooms to it.
    pub(crate) fn attribute_table_ui(&mut self, ui: &mut Ui) {
        if self.vectors.is_empty() {
            ui.weak("No vector layers: open one (File > Open vector file...).");
            return;
        }
        let li = match self.vector_index(self.attr_table.layer) {
            Some(li) => li,
            None => {
                (self.attr_table.layer, self.attr_table.picked) = (self.vectors[0].id, None);
                0
            }
        };
        let at = &mut self.attr_table;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("attribute table layer")
                .selected_text(&self.vectors[li].name)
                .width(220.0)
                .show_ui(ui, |ui| {
                    for v in &self.vectors {
                        if ui.selectable_value(&mut at.layer, v.id, &v.name).changed() {
                            at.picked = None;
                        }
                    }
                });
            let d = &self.vectors[li].data;
            ui.weak(format!("{} features, {} fields", d.features.len(), d.fields.len()));
        });
        let li = self.vectors.iter().position(|v| v.id == at.layer).unwrap_or(li);
        let v = &mut self.vectors[li];
        let n = v.data.features.len();
        if !v.table_ready() {
            ui.add_space(8.0);
            match &v.attrs.table {
                Table::Failed(e) => {
                    ui.colored_label(theme::warning(), format!("Its attributes could not be read: {e}"));
                    if ui.button("Try again").clicked() {
                        v.attrs.table = Table::NotRead;
                    }
                }
                Table::Reading(r) => {
                    let k = r.done.load(Ordering::Relaxed);
                    ui.add(
                        egui::ProgressBar::new(k as f32 / n.max(1) as f32)
                            .desired_width(ui.available_width().min(320.0))
                            .text(format!("Reading the attributes: {k} of {n} rows")),
                    );
                }
                _ => {
                    ui.weak("Reading the attributes...");
                }
            }
            return;
        }
        let v = &self.vectors[li];
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut at.filter).hint_text("Filter (any field)").desired_width(220.0));
            if !at.filter.is_empty() && ui.small_button("x").on_hover_text("Clear the filter").clicked() {
                at.filter.clear();
            }
        });
        let table_id = v.table().map_or(0, |t| Arc::as_ptr(&t) as usize);
        let key = (v.id, table_id, at.filter.clone(), at.sort);
        if at.rows_for.as_ref() != Some(&key) {
            at.rows = rows_of(v, &at.filter, at.sort);
            at.rows_for = Some(key);
        }
        ui.horizontal(|ui| {
            if at.rows.len() == n {
                ui.weak(format!("{n} rows"));
            } else {
                ui.weak(format!("{} of {n} rows", at.rows.len()));
            }
            if let Some(i) = at.picked {
                ui.weak(format!("|  feature {} picked (double click: zoom to it)", v.data.features[i].fid));
            }
        });
        let fields = &v.data.fields;
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        let mut zoom = None;
        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
            let mut tb = TableBuilder::new(ui)
                .id_salt(("attribute table", v.id))
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::initial(64.0).at_least(36.0).clip(true))
                .columns(Column::initial(120.0).at_least(36.0).clip(true), fields.len())
                .min_scrolled_height(0.0)
                .max_scroll_height(f32::INFINITY);
            if std::mem::take(&mut at.scroll_to)
                && let Some(pos) = at.picked.and_then(|p| at.rows.iter().position(|&i| i == p))
            {
                tb = tb.scroll_to_row(pos, Some(Align::Center));
            }
            let names = std::iter::once("fid").chain(fields.iter().map(String::as_str));
            tb.header(row_h, |mut header| {
                for (k, name) in names.enumerate() {
                    header.col(|ui| {
                        let mark = match at.sort {
                            Some((c, false)) if c == k => " ^",
                            Some((c, true)) if c == k => " v",
                            _ => "",
                        };
                        let b = egui::Button::new(RichText::new(format!("{name}{mark}")).strong()).frame(false);
                        if ui.add(b).on_hover_text("Sort by it (again: descending, then as read)").clicked() {
                            at.sort = match at.sort {
                                Some((c, false)) if c == k => Some((k, true)),
                                Some((c, true)) if c == k => None,
                                _ => Some((k, false)),
                            };
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, at.rows.len(), |mut row| {
                    let i = at.rows[row.index()];
                    row.set_selected(at.picked == Some(i));
                    row.col(|ui| {
                        ui.label(v.data.features[i].fid.to_string());
                    });
                    let values = v.row(i).unwrap_or_default();
                    for k in 0..fields.len() {
                        row.col(|ui| {
                            ui.add(egui::Label::new(values.get(k).map_or("", String::as_str)).truncate());
                        });
                    }
                    let r = row.response();
                    if r.clicked() {
                        at.picked = if at.picked == Some(i) { None } else { Some(i) };
                    }
                    if r.double_clicked() {
                        at.picked = Some(i);
                        zoom = Some(i);
                    }
                });
            });
        });
        if let Some(i) = zoom {
            self.zoom_to_feature(li, i);
        }
    }
}
