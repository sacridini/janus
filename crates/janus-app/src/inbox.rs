//! The inbox of the open Janus: what an assistant asks to show (`janus mcp`'s open_in_janus) shown in the window
//! already open rather than a new one. The server writes a request (a JSON file) into the inbox folder beside
//! Janus's data; a window watching it (an `alive-<pid>` file touched every few seconds says so) takes it (renamed,
//! so that one window only does), opens its files as layers one after the other, drops its places as pins and
//! comes to the front. A thread watches the folder and wakes the window when a request arrives.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use crate::app::State;

/// A window is watching when its alive file is younger than this.
const ALIVE: Duration = Duration::from_secs(15);

pub fn dir() -> PathBuf {
    crate::settings::app_data_dir().join("inbox")
}

/// Whether a window of Janus watches the inbox.
pub fn watched() -> bool {
    let Ok(rd) = std::fs::read_dir(dir()) else { return false };
    rd.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with("alive-")
            && e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < ALIVE)
    })
}

/// Posts a request ({paths, expression, places}) for the window watching; its file.
pub fn post(req: &Value) -> Result<PathBuf, String> {
    let d = dir();
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |t| t.as_nanos());
    let tmp = d.join(format!("{stamp}.part"));
    std::fs::write(&tmp, req.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    // whole when it appears
    let file = d.join(format!("{stamp}.json"));
    std::fs::rename(&tmp, &file).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// What a request asks, in order.
enum Item {
    /// a file or folder as a layer, the quantity shown
    Open(String, Option<String>),
    /// pins (pixels of the active layer), with their classes
    Pins(Vec<(usize, usize, String)>),
}

#[derive(Default)]
pub struct Inbox {
    started: bool,
    checked: Option<Instant>,
    queue: VecDeque<Item>,
}

/// The thread: the alive file touched every 5 s, the window woken when a request is there.
fn watch(ctx: egui::Context) {
    let d = dir();
    let alive = d.join(format!("alive-{}", std::process::id()));
    let mut touched: Option<Instant> = None;
    loop {
        if touched.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(&alive, b"");
            touched = Some(Instant::now());
        }
        let waiting = std::fs::read_dir(&d)
            .map(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x == "json")))
            .unwrap_or(false);
        if waiting {
            ctx.request_repaint();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The requests waiting, taken (each by one window).
fn take() -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(dir()) else { return vec![] };
    let mut files: Vec<PathBuf> =
        rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    let mut out = vec![];
    for f in files {
        let mine = f.with_extension(format!("taken-{}", std::process::id()));
        if std::fs::rename(&f, &mine).is_err() {
            continue;
        }
        if let Some(v) = std::fs::read_to_string(&mine).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            out.push(v);
        }
        let _ = std::fs::remove_file(&mine);
    }
    out
}

impl State {
    /// The inbox watched (not in tours and tests): its requests taken, their files opened one at a time.
    pub(crate) fn pump_inbox(&mut self, ctx: &egui::Context) {
        if self.opts.scripted() || self.opts.screenshot.is_some() || self.opts.measure_startup {
            return;
        }
        if !self.inbox.started {
            self.inbox.started = true;
            let c = ctx.clone();
            let _ = std::thread::Builder::new().name("inbox".into()).spawn(move || watch(c));
        }
        if self.inbox.checked.is_none_or(|t| t.elapsed() >= Duration::from_millis(400)) {
            self.inbox.checked = Some(Instant::now());
            for req in take() {
                let expr = req["expression"].as_str().filter(|e| !e.trim().is_empty()).map(String::from);
                for p in req["paths"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    self.inbox.queue.push_back(Item::Open(p.to_string(), expr.clone()));
                }
                let pins: Vec<(usize, usize, String)> = req["places"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| {
                        let (c, r) = (p["col"].as_f64()?, p["row"].as_f64()?);
                        Some((c as usize, r as usize, p["class"].as_str().unwrap_or("").to_string()))
                    })
                    .collect();
                if !pins.is_empty() {
                    self.inbox.queue.push_back(Item::Pins(pins));
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        if self.opening.is_some() || !self.vector_loads.is_empty() {
            return;
        }
        match self.inbox.queue.pop_front() {
            Some(Item::Open(path, expr)) => {
                let inputs = self.take_vector_inputs(vec![path], ctx);
                if !inputs.is_empty() {
                    let mut sel = janus_cube::BandSelection::default();
                    if let Some(e) = expr {
                        let name = janus_cube::calc::indices::find(e.trim()).map_or(String::new(), |i| i.name.clone());
                        sel.expr = janus_cube::calc::ExprId::intern(&name, e.trim());
                    }
                    self.error = None;
                    let add = !self.layers.is_empty();
                    self.opening = Some(crate::session::Opening::start_with(inputs, add, sel, ctx));
                }
                ctx.request_repaint();
            }
            Some(Item::Pins(pins)) if self.session.is_some() => {
                let before = std::mem::take(&mut self.ml.drop_class);
                for (x, y, class) in pins {
                    self.ml.drop_class = class;
                    self.drop_pin(x, y);
                }
                self.ml.drop_class = before;
                self.map.dirty = true;
            }
            _ => {}
        }
    }
}
