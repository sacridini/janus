//! The Files panel (src/file_browser.cpp): a lazily listed folder tree (rasters and vector files by default) to
//! open series without the system's dialog. A folder is listed in the background the first time it is expanded, so
//! slow disks or network shares never freeze the interface; its rasters are previewed as a series.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use egui::Ui;

use crate::app::State;

#[derive(Clone)]
struct Entry {
    path: String,
    name: String,
    dir: bool,
    size: u64,
}

struct Listing {
    entries: Vec<Entry>,
    /// its rasters as a series (describe_series_files)
    preview: String,
}

struct Node {
    e: Entry,
    listed: bool,
    preview: String,
    children: Vec<Node>,
    pending: Option<Receiver<Listing>>,
}

impl Node {
    fn new(e: Entry) -> Self {
        Node { e, listed: false, preview: String::new(), children: Vec::new(), pending: None }
    }
}

/// What the panel asks for: open these paths as the series, or add them as a layer.
pub enum Action {
    None,
    Open(Vec<String>),
    AddLayer(Vec<String>),
}

pub struct FileBrowser {
    roots: Vec<Node>,
    recent: Vec<String>,
    pub favorites: Vec<String>,
    selected: BTreeSet<String>,
    /// being revealed (lower case): the folder to open and the item to scroll to
    reveal_dir: String,
    reveal_item: String,
    reveal_relist: bool,
    /// the item revealed last (lower case)
    revealed: String,
    show_all: bool,
    filter: String,
    /// the favourites changed (kept in the settings)
    pub favorites_changed: bool,
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

fn human_size(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else {
        format!("{:.0} KB", b as f64 / 1024.0)
    }
}

/// The folder `dir` (lower case) is `p` or inside it.
fn within(dir: &str, p: &str) -> bool {
    if p.is_empty() || !dir.starts_with(p) {
        return false;
    }
    dir.len() == p.len() || p.ends_with(['/', '\\']) || dir[p.len()..].starts_with(['/', '\\'])
}

fn list(path: String, ctx: egui::Context) -> Receiver<Listing> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut entries: Vec<Entry> = std::fs::read_dir(&path)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|d| {
                        let name = d.file_name().to_string_lossy().into_owned();
                        if name.starts_with('$') {
                            return None; // $Recycle.Bin and friends
                        }
                        let md = d.metadata().ok();
                        let dir = md.as_ref().is_some_and(|m| m.is_dir());
                        Some(Entry {
                            path: d.path().to_string_lossy().into_owned(),
                            name,
                            dir,
                            size: md.filter(|_| !dir).map_or(0, |m| m.len()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| lower(&a.name).cmp(&lower(&b.name))));
        // the folder as a series, before opening it: its rasters and their dates
        let rasters: Vec<String> = entries
            .iter()
            .filter(|e| !e.dir && janus_cube::inputs::is_raster_path(Path::new(&e.path)))
            .map(|e| e.name.clone())
            .collect();
        let preview = janus_cube::describe_series_files(&rasters);
        let _ = tx.send(Listing { entries, preview });
        ctx.request_repaint();
    });
    rx
}

impl FileBrowser {
    pub fn new(favorites: Vec<String>) -> Self {
        let roots = crate::platform::root_folders()
            .into_iter()
            .map(|r| Node::new(Entry { path: r.clone(), name: r, dir: true, size: 0 }))
            .collect();
        FileBrowser {
            roots,
            recent: Vec::new(),
            favorites,
            selected: BTreeSet::new(),
            reveal_dir: String::new(),
            reveal_item: String::new(),
            reveal_relist: false,
            revealed: String::new(),
            show_all: false,
            filter: String::new(),
            favorites_changed: false,
        }
    }

    /// The folder of a series that was opened, under "Recent".
    pub fn add_recent(&mut self, path: &str) {
        let p = Path::new(path);
        let dir = if p.is_dir() { p.to_path_buf() } else { p.parent().map(Path::to_path_buf).unwrap_or_default() };
        let dir = dir.to_string_lossy().into_owned();
        self.recent.retain(|r| *r != dir);
        self.recent.insert(0, dir);
        self.recent.truncate(8);
    }

    fn add_favorite(&mut self, folder: &str) {
        if !self.favorites.iter().any(|f| f == folder) {
            self.favorites.push(folder.to_string());
            self.favorites_changed = true;
        }
    }

    /// Expands the tree down to this folder, or to the folder of this file, and scrolls there (a series just
    /// opened). The folder is listed again, so files written since show up.
    pub fn reveal(&mut self, path: &str) {
        let p = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
        let dir = p.is_dir();
        self.reveal_item = lower(&p.to_string_lossy());
        self.reveal_dir = if dir {
            self.reveal_item.clone()
        } else {
            lower(&p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default())
        };
        self.reveal_relist = true;
    }

    /// A path is being revealed (the tree not yet there).
    pub fn revealing(&self) -> bool {
        !self.reveal_dir.is_empty()
    }

    /// The item revealed last (lower case).
    pub fn revealed(&self) -> &str {
        &self.revealed
    }

    fn visible(&self, e: &Entry) -> bool {
        if !self.filter.is_empty() && !e.dir && !lower(&e.name).contains(&lower(&self.filter)) {
            return false;
        }
        let p = Path::new(&e.path);
        e.dir || self.show_all || janus_cube::inputs::is_raster_path(p) || janus_cube::inputs::is_vector_path(p)
    }

    /// A folder of the Favorites or Recent lists, with its menu.
    fn shortcut(&mut self, ui: &mut Ui, dir: &str, favorite: bool, act: &mut Action) {
        let r = ui
            .selectable_label(false, dir)
            .on_hover_text("Double click: open this folder as a series.\nRight click: more options.");
        if r.double_clicked() {
            *act = Action::Open(vec![dir.to_string()]);
        }
        r.context_menu(|ui| {
            if ui.button("Open folder as series").clicked() {
                *act = Action::Open(vec![dir.to_string()]);
            }
            if ui.button("Add folder as layer").clicked() {
                *act = Action::AddLayer(vec![dir.to_string()]);
            }
            if ui.button("Reveal in tree").clicked() {
                self.reveal(dir);
            }
            if favorite && ui.button("Remove from favorites").clicked() {
                self.favorites.retain(|f| f != dir);
                self.favorites_changed = true;
            }
            if !favorite && ui.button("Add to favorites").clicked() {
                self.add_favorite(dir);
            }
        });
    }

    fn node(&mut self, ui: &mut Ui, n: &mut Node, act: &mut Action, reveal: bool) {
        let lp = lower(&n.e.path);
        if !n.e.dir {
            let sel = self.selected.contains(&n.e.path);
            let r = ui.horizontal(|ui| {
                let r = ui.selectable_label(sel, &n.e.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.weak(human_size(n.e.size)));
                r
            });
            let r = r.inner.on_hover_text(format!(
                "{}\n{}\nDouble click: open as series. Ctrl+click: select several.",
                n.e.path,
                human_size(n.e.size)
            ));
            if reveal && !self.reveal_item.is_empty() && lp == self.reveal_item {
                r.scroll_to_me(Some(egui::Align::Min));
            }
            if r.clicked() {
                if ui.input(|i| i.modifiers.command) {
                    if sel {
                        self.selected.remove(&n.e.path);
                    } else {
                        self.selected.insert(n.e.path.clone());
                    }
                } else {
                    self.selected = [n.e.path.clone()].into();
                }
            }
            if r.double_clicked() {
                *act = Action::Open(vec![n.e.path.clone()]);
            }
            r.context_menu(|ui| {
                let paths: Vec<String> = if self.selected.contains(&n.e.path) {
                    self.selected.iter().cloned().collect()
                } else {
                    vec![n.e.path.clone()]
                };
                let label = if paths.len() > 1 { "Open selected files as a series" } else { "Open as series" };
                if ui.button(label).clicked() {
                    *act = Action::Open(paths.clone());
                }
                if ui.button("Add as layer").clicked() {
                    *act = Action::AddLayer(paths);
                }
                if ui.button("Show in Explorer").clicked() {
                    let parent =
                        Path::new(&n.e.path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                    crate::platform::open_in_explorer(&parent);
                }
            });
            return;
        }
        // the folders on the way to the revealed path open, and the view scrolls to its folder
        let on_path = reveal && !self.reveal_dir.is_empty() && within(&self.reveal_dir, &lp);
        let target = on_path && lp.len() == self.reveal_dir.len();
        let mut header = egui::CollapsingHeader::new(&n.e.name).id_salt(&n.e.path);
        if on_path {
            header = header.open(Some(true));
        }
        let ctx = ui.ctx().clone();
        let resp = header.show(ui, |ui| {
            if !n.listed && n.pending.is_none() {
                n.pending = Some(list(n.e.path.clone(), ctx.clone()));
            }
            if let Some(rx) = &n.pending {
                match rx.try_recv() {
                    Ok(l) => {
                        n.children = l.entries.into_iter().map(Node::new).collect();
                        n.preview = l.preview;
                        n.listed = true;
                        n.pending = None;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        ui.weak("listing...");
                    }
                    Err(_) => n.pending = None,
                }
            }
            let mut rasters = 0;
            let mut next = false; // a child on the way to the revealed path
            let mut children = std::mem::take(&mut n.children);
            for c in &mut children {
                if !self.visible(&c.e) {
                    continue;
                }
                if !c.e.dir {
                    rasters += 1;
                }
                if on_path && !target && c.e.dir && within(&self.reveal_dir, &lower(&c.e.path)) {
                    next = true;
                }
                self.node(ui, c, act, on_path);
            }
            // revealed (its folder is listed), or the way there is gone: done
            if on_path && n.listed && (target || !next) {
                if target {
                    self.revealed = self.reveal_item.clone();
                }
                self.reveal_dir.clear();
                self.reveal_item.clear();
            }
            if n.listed && children.is_empty() {
                ui.weak("(empty)");
            }
            if rasters > 1 {
                let r = ui.small_button("Open these files as a series");
                let r = if n.preview.is_empty() { r } else { r.on_hover_text(&n.preview) };
                if r.clicked() {
                    *act = Action::Open(
                        children.iter().filter(|c| !c.e.dir && self.visible(&c.e)).map(|c| c.e.path.clone()).collect(),
                    );
                }
            }
            n.children = children;
        });
        if target {
            resp.header_response.scroll_to_me(Some(egui::Align::Min));
            if std::mem::take(&mut self.reveal_relist) && n.listed {
                // files written since it was listed
                n.listed = false;
                n.children.clear();
            }
        }
        let h = if n.listed {
            resp.header_response.on_hover_text(format!(
                "{}\n{}",
                n.e.path,
                if n.preview.is_empty() { "no rasters" } else { &n.preview }
            ))
        } else {
            resp.header_response
        };
        h.context_menu(|ui| {
            if ui.button("Open folder as series").clicked() {
                *act = Action::Open(vec![n.e.path.clone()]);
            }
            if ui.button("Add folder as layer").clicked() {
                *act = Action::AddLayer(vec![n.e.path.clone()]);
            }
            if ui.button("Add to favorites").clicked() {
                self.add_favorite(&n.e.path);
            }
            if ui.button("Show in Explorer").clicked() {
                crate::platform::open_in_explorer(&n.e.path);
            }
        });
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Action {
        let mut act = Action::None;
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_all, "All files")
                .on_hover_text("Off: only rasters (.tif, .tiff, .vrt, .nc, .img, .jp2, ...)");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter).hint_text("filter by name").desired_width(f32::INFINITY),
            );
        });
        if !self.selected.is_empty() {
            ui.horizontal(|ui| {
                let n = self.selected.len();
                if ui.button(if n > 1 { "Open selected as series" } else { "Open" }).clicked() {
                    act = Action::Open(self.selected.iter().cloned().collect());
                }
                if ui.button("Add as layer").clicked() {
                    act = Action::AddLayer(self.selected.iter().cloned().collect());
                }
                ui.weak(format!("{n} selected"));
            });
        }
        ui.separator();
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            if !self.favorites.is_empty() {
                egui::CollapsingHeader::new("Favorites").default_open(true).show(ui, |ui| {
                    for f in self.favorites.clone() {
                        self.shortcut(ui, &f, true, &mut act);
                    }
                });
            }
            if !self.recent.is_empty() {
                egui::CollapsingHeader::new("Recent").default_open(true).show(ui, |ui| {
                    for r in self.recent.clone() {
                        self.shortcut(ui, &r, false, &mut act);
                    }
                });
            }
            // the revealed path opens under the root nearest to it only (the home folder rather than its drive)
            let reveal_root = self
                .roots
                .iter()
                .enumerate()
                .filter(|(_, r)| !self.reveal_dir.is_empty() && within(&self.reveal_dir, &lower(&r.e.path)))
                .max_by_key(|(_, r)| r.e.path.len())
                .map(|(i, _)| i);
            if reveal_root.is_none() {
                self.reveal_dir.clear();
                self.reveal_item.clear();
            }
            let mut roots = std::mem::take(&mut self.roots);
            for (i, r) in roots.iter_mut().enumerate() {
                self.node(ui, r, &mut act, Some(i) == reveal_root);
            }
            self.roots = roots;
        });
        act
    }
}

impl State {
    /// The Files panel.
    pub(crate) fn files_ui(&mut self, ui: &mut Ui) {
        let act = self.files.ui(ui);
        if std::mem::take(&mut self.files.favorites_changed) {
            self.settings.s.favorites = self.files.favorites.clone();
            self.settings.changed();
        }
        let ctx = ui.ctx().clone();
        match act {
            Action::Open(p) if !p.is_empty() => self.open_inputs(p, false, &ctx),
            Action::AddLayer(p) if !p.is_empty() => self.open_inputs(p, !self.layers.is_empty(), &ctx),
            _ => {}
        }
    }
}
