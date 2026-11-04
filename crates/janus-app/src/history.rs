//! The processing history in Python (Tools > Python history...): every run that ended well (a Zeit tool, the
//! calculator, a preparation, a vector tool...) leaves an entry with its Python, kept between sessions in
//! history.jsonl beside the settings. A layer opened and closed leaves nothing: only what was computed. The code is
//! the run's own script when it has one (rerun.py beside its provenance: the same reads and calls, its outputs
//! written again), else the run as data (its parameters, series and outputs) with the equivalent call named.
//!
//! The window lists the entries (newest first, filtered by a text) and shows the chosen one's code in an editor:
//! read, changed if wished, then copied or saved as a .py or a notebook (.ipynb); the whole history, or what the
//! filter shows, as one script (a `# %%` cell each) or one notebook. A snippet asked elsewhere (the chart's Python,
//! a run's from its menu) opens in the same editor without entering the history.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Ui, vec2};
use serde::{Deserialize, Serialize};

use crate::app::State;
use crate::provenance::Provenance;
#[cfg(test)]
use crate::provenance::SCRIPT;
use crate::python::py_str;

pub const FILE: &str = "history.jsonl";

/// The entries kept at most (the oldest dropped first).
const MAX_ENTRIES: usize = 5000;

/// One run in the history.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Entry {
    /// when it was recorded (ms since 1970): its identity
    pub id: u64,
    /// when the run started ("YYYY-MM-DD HH:MM:SS UTC")
    pub when: String,
    pub tool: String,
    pub tool_name: String,
    pub scope: String,
    pub seconds: f64,
    /// the run's folder (its provenance, its outputs)
    pub dir: String,
    pub outputs: Vec<String>,
    /// the run's own script (true) or the run as data with its equivalent named (false)
    pub script: bool,
    pub code: String,
}

impl Entry {
    /// The entry of a run that ended well in `dir`.
    pub fn of(dir: &Path, p: &Provenance) -> Entry {
        let (code, script) = code_of(dir, p);
        Entry {
            id: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
            when: p.started.clone(),
            tool: p.tool.clone(),
            tool_name: if p.tool_name.is_empty() { p.tool.clone() } else { p.tool_name.clone() },
            scope: p.scope.clone(),
            seconds: p.seconds,
            dir: dir.to_string_lossy().into_owned(),
            outputs: p.outputs.iter().map(|o| o.path.clone()).collect(),
            script,
            code,
        }
    }

    /// Its name in files: the tool and the day and time it ran.
    fn file_stem(&self) -> String {
        let when: String = self.when.chars().filter(|c| c.is_ascii_digit()).take(14).collect();
        let tool: String =
            self.tool.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        format!("janus_{tool}_{when}")
    }

    /// Whether the filter's words are all in its name, scope, time or outputs.
    fn matches(&self, filter: &str) -> bool {
        let hay = format!("{} {} {} {} {}", self.tool_name, self.tool, self.scope, self.when, self.outputs.join(" "))
            .to_lowercase();
        filter.to_lowercase().split_whitespace().all(|w| hay.contains(w))
    }
}

fn path() -> PathBuf {
    crate::settings::app_data_dir().join(FILE)
}

/// The history on disk, oldest first (a line that does not read is skipped).
pub fn load_from(path: &Path) -> Vec<Entry> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect()
}

fn append_to(path: &Path, e: &Entry) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(e).unwrap_or_default())
}

fn rewrite(path: &Path, entries: &[Entry]) -> std::io::Result<()> {
    let mut text = String::new();
    for e in entries {
        text += &serde_json::to_string(e).unwrap_or_default();
        text.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

/// A value as a Python literal: JSON read back by json.loads (any nesting, quotes and backslashes kept).
fn py_json(v: &serde_json::Value) -> String {
    let text = serde_json::to_string_pretty(v).unwrap_or_default();
    if text.contains("'''") || text.ends_with('\\') {
        format!("json.loads({})", py_str(&serde_json::to_string(v).unwrap_or_default()))
    } else {
        format!("json.loads(r'''{text}''')")
    }
}

fn py_list(v: &[String]) -> String {
    if v.is_empty() {
        return "[]".into();
    }
    let mut o = String::from("[\n");
    for s in v {
        o += &format!("    {},\n", py_str(s));
    }
    o + "]"
}

/// The run's code: its header (what ran, on what, what it wrote), then its script, or the run as data. And whether
/// it is a script.
pub fn code_of(dir: &Path, p: &Provenance) -> (String, bool) {
    let mut o = format!(
        "# Janus {}: {}, {} ({})\n",
        p.janus,
        if p.tool_name.is_empty() { &p.tool } else { &p.tool_name },
        p.started,
        if p.seconds < 60.0 { format!("{:.1} s", p.seconds) } else { format!("{:.1} min", p.seconds / 60.0) }
    );
    if let Some(s) = &p.series {
        o += &format!("# Series: {} ({}, {} dates, {} .. {})\n", s.name, s.description, s.dates, s.first, s.last);
        if !s.selection.is_empty() {
            o += &format!("#   showing {}\n", s.selection);
        }
    }
    if !p.scope.is_empty() {
        o += &format!("# Scope: {}\n", p.scope);
    }
    o += &format!("# Folder: {}\n", dir.display());
    if !p.outputs.is_empty() {
        o += "# Wrote:\n";
        for out in p.outputs.iter().take(40) {
            o += &format!("#   {}\n", out.path);
        }
        if p.outputs.len() > 40 {
            o += &format!("#   ... and {} more\n", p.outputs.len() - 40);
        }
    }
    o.push('\n');
    let script = (!p.python.is_empty()).then(|| std::fs::read_to_string(dir.join(&p.python)).ok()).flatten();
    if let Some(s) = script {
        o += &s;
        return (o, true);
    }
    o += "# Janus has no script of its own for this tool: the run as data, and what does the same in Python.\n";
    if !p.python_note.is_empty() {
        for l in p.python_note.lines() {
            o += &format!("#   {l}\n");
        }
    }
    o += "import json\n\n";
    o += &format!("tool = {}\n", py_str(&p.tool));
    o += &format!("params = {}\n", py_json(&serde_json::to_value(&p.params).unwrap_or_default()));
    if !p.extra.is_empty() {
        o += &format!("extra = {}\n", py_json(&serde_json::to_value(&p.extra).unwrap_or_default()));
    }
    if let Some(s) = &p.series {
        o += &format!("# the series' files, in date order\nseries_files = {}\n", py_list(&s.files));
    }
    if let Some([x0, y0, x1, y1]) = p.window {
        o += &format!("# the series' pixels it covered: columns {x0}..{x1}, rows {y0}..{y1}\n");
        o += &format!("window = ({x0}, {y0}, {x1}, {y1})\n");
    }
    let outputs: Vec<String> = p.outputs.iter().map(|o| o.path.clone()).collect();
    o += &format!("outputs = {}\n", py_list(&outputs));
    (o, false)
}

/// Several entries as one script, a `# %%` cell each (VS Code, Spyder and Jupytext run them one by one).
fn as_script(entries: &[&Entry]) -> String {
    let mut o = format!("# Janus: {} runs of the processing history\n\n", entries.len());
    for e in entries {
        o += &format!("# %% {} ({})\n", e.tool_name, e.when);
        o += &e.code;
        if !e.code.ends_with('\n') {
            o.push('\n');
        }
        o.push('\n');
    }
    o
}

/// Entries as a Jupyter notebook: a title and a code cell each.
pub fn as_notebook(cells: &[(String, String)]) -> String {
    let lines = |s: &str| -> Vec<String> {
        let mut v: Vec<String> = s.split_inclusive('\n').map(str::to_string).collect();
        if let Some(l) = v.last_mut()
            && l.ends_with('\n')
        {
            l.pop();
        }
        v
    };
    let mut out = vec![];
    for (title, code) in cells {
        out.push(serde_json::json!({"cell_type": "markdown", "metadata": {}, "source": lines(title)}));
        out.push(serde_json::json!({
            "cell_type": "code", "metadata": {}, "execution_count": null, "outputs": [], "source": lines(code)
        }));
    }
    serde_json::to_string_pretty(&serde_json::json!({
        "cells": out,
        "metadata": {
            "kernelspec": {"name": "python3", "display_name": "Python 3", "language": "python"},
            "language_info": {"name": "python"}
        },
        "nbformat": 4,
        "nbformat_minor": 5
    }))
    .unwrap_or_default()
}

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "break", "class", "continue", "def", "del", "elif", "else",
    "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or",
    "pass", "raise", "return", "try", "while", "with", "yield",
];

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    Plain,
    Comment,
    Str,
    Number,
    Keyword,
    Call,
}

fn tok_color(t: Tok, dark: bool, plain: Color32) -> Color32 {
    let rgb = |d: [u8; 3], l: [u8; 3]| {
        let v = if dark { d } else { l };
        Color32::from_rgb(v[0], v[1], v[2])
    };
    match t {
        Tok::Plain => plain,
        Tok::Comment => rgb([127, 132, 142], [140, 141, 147]),
        Tok::Str => rgb([152, 195, 121], [80, 161, 79]),
        Tok::Number => rgb([209, 154, 102], [152, 104, 1]),
        Tok::Keyword => rgb([198, 120, 221], [166, 38, 164]),
        Tok::Call => rgb([97, 175, 239], [64, 120, 242]),
    }
}

/// Python's tokens, coarsely: comments, strings (triple-quoted and prefixed too), numbers, keywords, names called.
fn tokens(text: &str) -> Vec<(std::ops::Range<usize>, Tok)> {
    let b = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            out.push((start..i, Tok::Comment));
        } else if c == b'"' || c == b'\'' {
            let triple = i + 2 < b.len() && b[i + 1] == c && b[i + 2] == c;
            i += if triple { 3 } else { 1 };
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if triple {
                    if i + 2 < b.len() && b[i] == c && b[i + 1] == c && b[i + 2] == c {
                        i += 3;
                        break;
                    }
                } else if b[i] == c || b[i] == b'\n' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((start..i.min(b.len()), Tok::Str));
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            out.push((start..i, Tok::Number));
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            // a string's prefix: r"...", f'...', b"..."
            if i < b.len()
                && (b[i] == b'"' || b[i] == b'\'')
                && word.len() <= 2
                && word.chars().all(|c| "rRbBfFuU".contains(c))
            {
                continue_string(b, &mut i);
                out.push((start..i, Tok::Str));
                continue;
            }
            let tok = if KEYWORDS.contains(&word) {
                Tok::Keyword
            } else if i < b.len() && b[i] == b'(' {
                Tok::Call
            } else {
                Tok::Plain
            };
            out.push((start..i, tok));
        } else {
            // a byte of a UTF-8 character, an operator, a space
            i += 1;
            while i < b.len() && !text.is_char_boundary(i) {
                i += 1;
            }
            out.push((start..i, Tok::Plain));
        }
    }
    out
}

/// The rest of a string after its prefix (`i` at its quote).
fn continue_string(b: &[u8], i: &mut usize) {
    let c = b[*i];
    let triple = *i + 2 < b.len() && b[*i + 1] == c && b[*i + 2] == c;
    *i += if triple { 3 } else { 1 };
    while *i < b.len() {
        if b[*i] == b'\\' {
            *i += 2;
            continue;
        }
        if triple {
            if *i + 2 < b.len() && b[*i] == c && b[*i + 1] == c && b[*i + 2] == c {
                *i += 3;
                return;
            }
        } else if b[*i] == c || b[*i] == b'\n' {
            *i += 1;
            return;
        }
        *i += 1;
    }
    *i = (*i).min(b.len());
}

/// The code coloured.
fn highlight(text: &str, font: &FontId, dark: bool, plain: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (r, t) in tokens(text) {
        let r = r.start.min(text.len())..r.end.min(text.len());
        job.append(
            &text[r],
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: tok_color(t, dark, plain),
                italics: t == Tok::Comment,
                ..Default::default()
            },
        );
    }
    job
}

/// What the editor shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shown {
    Entry(u64),
    Snippet,
}

/// The window's state and the history read from disk.
#[derive(Default)]
pub struct HistoryUi {
    pub show: bool,
    loaded: bool,
    pub entries: Vec<Entry>,
    shown: Option<Shown>,
    /// a snippet asked elsewhere: its title and code (not in the history)
    snippet: Option<(String, String)>,
    /// the editor's text (changes kept until another entry is chosen) and whether it was changed
    text: String,
    edited: bool,
    filter: String,
    confirm_clear: bool,
    flash: Option<(Instant, String)>,
    /// where the history is kept (tests: a file of their own)
    pub(crate) path: Option<PathBuf>,
}

impl HistoryUi {
    fn file(&self) -> PathBuf {
        self.path.clone().unwrap_or_else(path)
    }

    fn load(&mut self) {
        if !self.loaded {
            self.entries = load_from(&self.file());
            self.loaded = true;
        }
    }

    /// A run that ended well, added (and kept on disk).
    pub fn add(&mut self, e: Entry) {
        self.load();
        let _ = append_to(&self.file(), &e);
        self.entries.push(e);
        if self.entries.len() > MAX_ENTRIES {
            let drop = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..drop);
            let _ = rewrite(&self.file(), &self.entries);
        }
    }

    fn select(&mut self, s: Shown) {
        self.shown = Some(s);
        self.text = match s {
            Shown::Entry(id) => self.entries.iter().find(|e| e.id == id).map_or(String::new(), |e| e.code.clone()),
            Shown::Snippet => self.snippet.as_ref().map_or(String::new(), |s| s.1.clone()),
        };
        self.edited = false;
    }

    /// A snippet in the editor, outside the history (the chart's Python, a run's script): the window opened on it.
    pub fn show_snippet(&mut self, title: &str, code: String) {
        self.load();
        self.snippet = Some((title.to_string(), code));
        self.select(Shown::Snippet);
        self.show = true;
    }

    /// The window opened on the entry of the run in `dir` (the newest), or on the history.
    pub fn show_run(&mut self, dir: &Path) -> bool {
        self.load();
        let d = dir.to_string_lossy();
        let found = self.entries.iter().rev().find(|e| Path::new(&e.dir) == Path::new(d.as_ref())).map(|e| e.id);
        self.show = true;
        if let Some(id) = found {
            self.select(Shown::Entry(id));
        }
        found.is_some()
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some((Instant::now(), text.into()));
    }

    /// The editor's text saved as a .py or a notebook (a dialog asks where).
    fn save_text(&mut self, title: &str, stem: &str, notebook: bool) {
        let (ext, name) = if notebook { ("ipynb", "Jupyter notebook") } else { ("py", "Python") };
        let Some(to) =
            rfd::FileDialog::new().add_filter(name, &[ext]).set_file_name(format!("{stem}.{ext}")).save_file()
        else {
            return;
        };
        let body =
            if notebook { as_notebook(&[(format!("## {title}"), self.text.clone())]) } else { self.text.clone() };
        match std::fs::write(&to, body) {
            Ok(()) => self.flash(format!("saved {}", to.display())),
            Err(e) => self.flash(format!("not saved: {e}")),
        }
    }

    /// The entries the filter shows (oldest first) saved as one script or one notebook.
    fn save_all(&mut self, notebook: bool) {
        let list: Vec<&Entry> = self.entries.iter().filter(|e| e.matches(&self.filter)).collect();
        if list.is_empty() {
            return;
        }
        let (ext, name) = if notebook { ("ipynb", "Jupyter notebook") } else { ("py", "Python") };
        let Some(to) =
            rfd::FileDialog::new().add_filter(name, &[ext]).set_file_name(format!("janus_history.{ext}")).save_file()
        else {
            return;
        };
        let body = if notebook {
            let cells: Vec<(String, String)> =
                list.iter().map(|e| (format!("## {} ({})", e.tool_name, e.when), e.code.clone())).collect();
            as_notebook(&cells)
        } else {
            as_script(&list)
        };
        let n = list.len();
        match std::fs::write(&to, body) {
            Ok(()) => self.flash(format!("{n} runs saved in {}", to.display())),
            Err(e) => self.flash(format!("not saved: {e}")),
        }
    }
}

impl State {
    /// A run that ended well (its provenance written): its entry in the history.
    pub(crate) fn record_history(&mut self, dir: &Path, p: &Provenance) {
        if p.state == "done" {
            self.history.add(Entry::of(dir, p));
        }
    }

    pub(crate) fn history_window(&mut self, ctx: &egui::Context) {
        if !self.history.show {
            return;
        }
        self.history.load();
        let mut open = true;
        let screen = ctx.content_rect();
        crate::panels::window("Python history", ctx)
            .id(egui::Id::new("python history window"))
            .open(&mut open)
            .resizable(true)
            .default_size(vec2((screen.width() * 0.7).min(1100.0), (screen.height() * 0.7).min(680.0)))
            .min_size(vec2(560.0, 320.0))
            .show(ctx, |ui| self.history_ui(ui));
        if !open {
            self.history.show = false;
        }
    }

    fn history_ui(&mut self, ui: &mut Ui) {
        let h = &mut self.history;
        let mut show_prov = None;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut h.filter)
                    .hint_text("Filter (tool, date, file...)")
                    .desired_width(ui.text_style_height(&egui::TextStyle::Body) * 16.0),
            );
            let n = h.entries.iter().filter(|e| e.matches(&h.filter)).count();
            ui.weak(if h.filter.trim().is_empty() {
                format!("{n} runs")
            } else {
                format!("{n} of {} runs", h.entries.len())
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if h.confirm_clear {
                    if ui.button("Cancel").clicked() {
                        h.confirm_clear = false;
                    }
                    if ui.button("Clear it").clicked() {
                        h.entries.clear();
                        let _ = rewrite(&h.file(), &[]);
                        h.confirm_clear = false;
                        if matches!(h.shown, Some(Shown::Entry(_))) {
                            h.shown = None;
                            h.text.clear();
                        }
                    }
                    ui.label("The whole history?");
                } else {
                    if ui
                        .add_enabled(!h.entries.is_empty(), egui::Button::new("Clear history..."))
                        .on_hover_text("Every entry removed (the runs' folders stay)")
                        .clicked()
                    {
                        h.confirm_clear = true;
                    }
                    if ui
                        .add_enabled(n > 0, egui::Button::new("All as notebook..."))
                        .on_hover_text("The runs listed (the filter's), oldest first, as one Jupyter notebook")
                        .clicked()
                    {
                        h.save_all(true);
                    }
                    if ui
                        .add_enabled(n > 0, egui::Button::new("All as .py..."))
                        .on_hover_text("The runs listed (the filter's), oldest first, as one script: a # %% cell each")
                        .clicked()
                    {
                        h.save_all(false);
                    }
                }
            });
        });
        ui.separator();
        let height = ui.available_height();
        let list_w = (ui.text_style_height(&egui::TextStyle::Body) * 20.0).min(ui.available_width() * 0.35);
        ui.horizontal_top(|ui| {
            // the entries, newest first
            ui.allocate_ui_with_layout(vec2(list_w, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_size(vec2(list_w, height));
                egui::ScrollArea::vertical().id_salt("history list").auto_shrink([false, false]).show(ui, |ui| {
                    let mut pick = None;
                    if let Some((title, _)) = &h.snippet {
                        let on = h.shown == Some(Shown::Snippet);
                        let r = ui.add(egui::Button::selectable(on, format!("{title}\nnot in the history")).wrap());
                        if r.clicked() {
                            pick = Some(Shown::Snippet);
                        }
                        ui.separator();
                    }
                    if h.entries.is_empty() {
                        ui.weak(
                            "Nothing computed yet. Each run that ends well (a Zeit tool, the calculator, a \
                             preparation, a vector tool) adds its Python here.",
                        );
                    }
                    for e in h.entries.iter().rev().filter(|e| e.matches(&h.filter)) {
                        let on = h.shown == Some(Shown::Entry(e.id));
                        let when = e.when.trim_end_matches(" UTC");
                        let sub = if e.scope.is_empty() { when.to_string() } else { format!("{when}, {}", e.scope) };
                        let mark = if e.script { "" } else { "  (as data)" };
                        let text = egui::RichText::new(format!("{}{mark}\n{sub}", e.tool_name));
                        let r = ui.add(egui::Button::selectable(on, text).wrap()).on_hover_text(format!(
                            "{}\n{} files written",
                            e.dir,
                            e.outputs.len()
                        ));
                        if r.clicked() {
                            pick = Some(Shown::Entry(e.id));
                        }
                    }
                    if let Some(s) = pick {
                        h.select(s);
                    }
                });
            });
            ui.separator();
            // the chosen one's code
            ui.vertical(|ui| {
                let Some(shown) = h.shown else {
                    ui.add_space(8.0);
                    ui.weak("Choose a run on the left to see its Python.");
                    return;
                };
                let (title, stem, dir, script) = match shown {
                    Shown::Entry(id) => match h.entries.iter().find(|e| e.id == id) {
                        Some(e) => {
                            (format!("{} ({})", e.tool_name, e.when), e.file_stem(), Some(e.dir.clone()), e.script)
                        }
                        None => return,
                    },
                    Shown::Snippet => {
                        let t = h.snippet.as_ref().map_or(String::new(), |s| s.0.clone());
                        (t, "janus_snippet".to_string(), None, true)
                    }
                };
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&title);
                    if !script {
                        ui.weak("the run as data: Janus has no script of its own for this tool");
                    }
                    if h.edited {
                        ui.weak("(changed)");
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy").on_hover_text("The code as the editor shows it").clicked() {
                        ui.ctx().copy_text(h.text.clone());
                        h.flash("copied");
                    }
                    if ui.button("Save .py...").clicked() {
                        h.save_text(&title, &stem, false);
                    }
                    if ui.button("Save notebook...").on_hover_text("A Jupyter notebook (.ipynb)").clicked() {
                        h.save_text(&title, &stem, true);
                    }
                    if h.edited && ui.button("Revert").on_hover_text("The code as Janus wrote it").clicked() {
                        h.select(shown);
                    }
                    if let Some(d) = &dir {
                        if ui.button("Open folder").clicked() {
                            crate::platform::open_in_explorer(d);
                        }
                        if ui.button("Provenance").on_hover_text("What the run took and did").clicked() {
                            show_prov = Some(d.clone());
                        }
                    }
                    if let Shown::Entry(id) = shown
                        && ui
                            .button("Remove")
                            .on_hover_text("This entry out of the history (its folder stays)")
                            .clicked()
                    {
                        h.entries.retain(|e| e.id != id);
                        let _ = rewrite(&h.file(), &h.entries);
                        h.shown = None;
                        h.text.clear();
                    }
                    if let Some((t, msg)) = &h.flash {
                        if t.elapsed().as_secs_f32() < 3.0 {
                            ui.weak(msg);
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                        } else {
                            h.flash = None;
                        }
                    }
                });
                ui.add_space(4.0);
                let dark = ui.visuals().dark_mode;
                let plain = ui.visuals().text_color();
                let font = egui::TextStyle::Monospace.resolve(ui.style());
                let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, wrap: f32| {
                    let mut job = highlight(text.as_str(), &font, dark, plain);
                    job.wrap.max_width = wrap;
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let left = ui.available_height();
                egui::ScrollArea::both().id_salt("history code").auto_shrink([false, false]).max_height(left).show(
                    ui,
                    |ui| {
                        let r = ui.add(
                            egui::TextEdit::multiline(&mut h.text)
                                .id(egui::Id::new("history code editor"))
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .min_size(vec2(0.0, left - 8.0))
                                .layouter(&mut layouter),
                        );
                        if r.changed() {
                            h.edited = true;
                        }
                    },
                );
            });
        });
        if let Some(d) = show_prov {
            self.show_provenance(&d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_between_sessions() {
        let dir = std::env::temp_dir().join(format!("janus-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(FILE);
        let mut h = HistoryUi { path: Some(file.clone()), ..Default::default() };
        let mut p = Provenance::start("warp", "warp", "Reproject, resample, clip");
        p.state = "done".into();
        p.params.insert("gdalwarp".into(), serde_json::json!("-t_srs EPSG:4326 -r near"));
        p.python_note = "gdalwarp <args> <file> <output>".into();
        let e = Entry::of(&dir, &p);
        assert!(!e.script);
        assert!(e.code.contains("params = json.loads(r'''"), "{}", e.code);
        assert!(e.code.contains("-t_srs EPSG:4326"));
        h.add(e.clone());
        // a run with its own script
        std::fs::write(dir.join(SCRIPT), "print('hello')\n").unwrap();
        p.python = SCRIPT.into();
        let e2 = Entry { id: e.id + 1, ..Entry::of(&dir, &p) };
        assert!(e2.script && e2.code.ends_with("print('hello')\n"));
        h.add(e2);
        // read again from disk, in order
        let back = load_from(&file);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].code, e.code);
        assert!(back[1].script);
        // removed and rewritten
        let mut h2 = HistoryUi { path: Some(file.clone()), ..Default::default() };
        h2.load();
        h2.entries.retain(|x| x.id != e.id);
        rewrite(&file, &h2.entries).unwrap();
        assert_eq!(load_from(&file).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notebook_and_script() {
        let nb = as_notebook(&[("## a".into(), "x = 1\nprint(x)\n".into())]);
        let v: serde_json::Value = serde_json::from_str(&nb).unwrap();
        assert_eq!(v["nbformat"], 4);
        assert_eq!(v["cells"][1]["source"], serde_json::json!(["x = 1\n", "print(x)"]));
        let e = Entry {
            tool_name: "T".into(),
            when: "2026-11-04 10:00:00 UTC".into(),
            code: "a = 2".into(),
            ..Default::default()
        };
        let s = as_script(&[&e, &e]);
        assert_eq!(s.matches("# %% T").count(), 2);
    }

    #[test]
    fn python_tokens() {
        let t = tokens("import numpy as np  # c\nx = f(1.5, 'a#b', r\"\\d\")\n\"\"\"doc\n\"\"\"");
        let kinds = |k: Tok| t.iter().filter(|(_, x)| *x == k).count();
        assert_eq!(kinds(Tok::Keyword), 2); // import, as
        assert_eq!(kinds(Tok::Comment), 1);
        assert_eq!(kinds(Tok::Str), 3); // 'a#b', r"\d", the docstring
        assert_eq!(kinds(Tok::Call), 1);
        assert_eq!(kinds(Tok::Number), 1);
    }
}
