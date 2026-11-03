//! Text that does not fit its place, found in what a frame painted (`--shots layout`, the font sizes' tour): a
//! text cut by the side of its clip rectangle (a column, a panel, a window narrower than it), taller than its clip,
//! out of the frame it is drawn in (a button, a field, a tab narrower than its label), or over another text of
//! the same layer (a fixed width a longer label runs past, into its neighbour). Read at the end of each pass, from
//! the layers' shapes before they are flattened (a window's texts are compared with its own only), counted when two
//! passes in a row show it. A text cut short on purpose, with the whole of it on hover, is marked where it is drawn
//! (`cut_on_purpose`) and not reported.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use egui::epaint::{Shape, TextShape};
use egui::{LayerId, Order, Rect};

/// What is wrong with a text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// cut by the left or right side of its clip rectangle
    Clipped,
    /// taller than its clip rectangle (cut at the top and the bottom)
    TooTall,
    /// past the sides of the frame (a button's, a field's) its middle is in
    OutOfFrame,
    /// over another text of the same layer
    Overlap,
    /// cut by the bottom of its clip rectangle (a panel too short, unless it scrolls)
    CutBelow,
    /// drawn in a fixed size much smaller than the interface's text (it does not follow the Font size setting)
    Small,
    /// cut short with an ellipsis (a combo box's or a column's fixed width) and not marked as cut on purpose
    /// (the whole text on hover)
    Elided,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Clipped => "clipped",
            Kind::TooTall => "too tall",
            Kind::OutOfFrame => "out of its frame",
            Kind::Overlap => "over another text",
            Kind::CutBelow => "cut at the bottom",
            Kind::Small => "fixed small font",
            Kind::Elided => "cut short (...)",
        }
    }
}

/// A text that does not fit, the first time it was seen.
#[derive(Clone, Debug)]
pub struct Finding {
    pub kind: Kind,
    pub text: String,
    /// the other text (Overlap)
    pub other: String,
    /// by how many points it does not fit
    pub by: f32,
    pub rect: Rect,
    pub layer: String,
}

/// A finding's identity: its kind, layer, text and the other text.
type Key = (Kind, String, String, String);

/// Whether the frames are read, and what they found (deduplicated by kind, layer and text).
#[derive(Default)]
struct Audit {
    on: bool,
    found: HashMap<Key, Finding>,
    /// what the last pass found: a text counts when two passes in a row find it (a grid's columns take their
    /// width from the pass before, and a cell grown in this one is over the next for a single frame)
    last: HashSet<Key>,
}

static AUDIT: LazyLock<Mutex<Audit>> = LazyLock::new(Default::default);

/// Whether the plugin is there (the font sizes' tour): texts cut on purpose are noted only then.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// The texts of this pass cut short on purpose (as `Text::text`: trimmed, its first 80 characters).
static ON_PURPOSE: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// A text cut short on purpose where it is drawn, the whole of it on hover (a long name in a column, a caption
/// under a picture): not reported as cut short. Called in the pass that draws it; nothing outside the tour.
pub fn cut_on_purpose(text: &str) {
    if INSTALLED.load(Ordering::Relaxed) {
        ON_PURPOSE.lock().unwrap().insert(key_of(text));
    }
}

/// A text as the audit keeps it: trimmed, its first 80 characters.
fn key_of(text: &str) -> String {
    text.trim().chars().take(80).collect()
}

/// Points a text may pass its place by before it counts (antialiasing, a glyph's side bearing).
const SLACK: f32 = 1.5;

/// Starts reading the frames (a clean list).
pub fn start() {
    let mut a = AUDIT.lock().unwrap();
    a.on = true;
    a.found.clear();
    a.last.clear();
}

/// Stops reading them; what they found, the worst first.
pub fn stop() -> Vec<Finding> {
    let mut a = AUDIT.lock().unwrap();
    a.on = false;
    let mut v: Vec<Finding> = a.found.drain().map(|(_, f)| f).collect();
    v.sort_by(|a, b| a.kind.cmp(&b.kind).then(b.by.total_cmp(&a.by)));
    v
}

/// The plugin reading each pass's shapes while the audit is on.
pub struct LayoutAudit;

impl egui::Plugin for LayoutAudit {
    fn debug_name(&self) -> &'static str {
        "layout audit"
    }

    fn setup(&mut self, _ctx: &egui::Context) {
        INSTALLED.store(true, Ordering::Relaxed);
    }

    fn on_begin_pass(&mut self, _ui: &mut egui::Ui) {
        ON_PURPOSE.lock().unwrap().clear();
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        if !AUDIT.lock().unwrap().on {
            return;
        }
        let on_purpose = std::mem::take(&mut *ON_PURPOSE.lock().unwrap());
        let ctx = ui.ctx();
        // a pass egui throws away (it lays out again before painting) is never seen
        if ctx.will_discard() {
            return;
        }
        let body = ctx.style_of(ctx.theme()).text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
        let layers: Vec<LayerId> = ctx.memory(|m| m.layer_ids().collect());
        let mut found = vec![];
        for layer in layers {
            // the debug painter's and the tooltips' (they follow the pointer, the tour's) are not the interface's
            if layer.order == Order::Debug {
                continue;
            }
            let (texts, frames, bars) = ctx.graphics(|g| {
                let mut s = Shapes::default();
                if let Some(list) = g.get(layer) {
                    for c in list.all_entries() {
                        collect(&c.shape, c.clip_rect, &mut s);
                    }
                }
                (s.texts, s.frames, s.bars)
            });
            check(&texts, &frames, &bars, &on_purpose, body, &layer.short_debug_format(), &mut found);
        }
        let key = |f: &Finding| (f.kind, f.layer.clone(), f.text.clone(), f.other.clone());
        let mut a = AUDIT.lock().unwrap();
        let last = std::mem::replace(&mut a.last, found.iter().map(key).collect());
        for f in found {
            let key = key(&f);
            if !last.contains(&key) {
                continue;
            }
            match a.found.get_mut(&key) {
                Some(old) if old.by >= f.by => {}
                Some(old) => *old = f,
                None => {
                    a.found.insert(key, f);
                }
            }
        }
    }
}

/// A text as painted: its words, where its glyphs are, its clip rectangle.
struct Text {
    text: String,
    ink: Rect,
    /// its lines' glyphs, each line's box within `ink` (a text wrapped in a row of widgets starts its first line
    /// after them and its next ones under them: its box is over them, its lines are not)
    lines: Vec<Rect>,
    clip: Rect,
    /// its font's size (the first section's)
    size: f32,
    /// cut short with an ellipsis
    elided: bool,
}

/// A layer's texts, its frames (rectangles drawn with a fill or a border: buttons, fields, tabs) with their clip,
/// and its scroll bars' places (thin rectangles, painted even while invisible: a scroll area whose content is
/// larger than it).
#[derive(Default)]
struct Shapes {
    texts: Vec<Text>,
    frames: Vec<(Rect, Rect)>,
    bars: Vec<Rect>,
}

fn collect(shape: &Shape, clip: Rect, s: &mut Shapes) {
    match shape {
        Shape::Vec(v) => {
            for x in v {
                collect(x, clip, s);
            }
        }
        Shape::Text(t) => {
            if let Some(t) = text_of(t, clip) {
                s.texts.push(t);
            }
        }
        Shape::Rect(r) => {
            let seen = r.fill.a() > 0 || r.stroke.width > 0.0 && r.stroke.color.a() > 0;
            if seen && r.rect.is_positive() {
                s.frames.push((r.rect, clip));
            }
            let (w, h) = (r.rect.width(), r.rect.height());
            if w > 0.0 && h > 0.0 && (w <= 14.0 && h >= 24.0 || h <= 14.0 && w >= 24.0) {
                s.bars.push(r.rect);
            }
        }
        _ => {}
    }
}

/// Whether a clip rectangle scrolls that way: a scroll bar along its right side (`vertical`) or its bottom.
fn scrolls(clip: Rect, bars: &[Rect], vertical: bool) -> bool {
    bars.iter().any(|b| {
        if vertical {
            b.height() > b.width()
                && (b.center().x - clip.max.x).abs() <= 16.0
                && b.y_range().intersection(clip.y_range()).span() >= 0.6 * clip.height()
        } else {
            // a horizontal scroll area clips across only: its bar is at its content's bottom, anywhere in the clip
            b.width() > b.height()
                && b.center().y >= clip.min.y
                && b.center().y <= clip.max.y + 16.0
                && b.x_range().intersection(clip.x_range()).span() >= 0.6 * clip.width()
        }
    })
}

fn text_of(t: &TextShape, clip: Rect) -> Option<Text> {
    // turned texts (a chart's vertical axis title) are left alone: their box is not their glyphs'
    if t.angle != 0.0 || t.opacity_factor <= 0.0 {
        return None;
    }
    let text = t.galley.text().trim();
    if text.is_empty() {
        return None;
    }
    let ink = t.galley.mesh_bounds.translate(t.pos.to_vec2());
    if !ink.is_positive() || !ink.intersects(clip) {
        return None;
    }
    let size = t.galley.job.sections.first().map_or(0.0, |s| s.format.font_id.size);
    let lines = if t.galley.rows.len() > 1 {
        let at = t.pos.to_vec2();
        t.galley
            .rows
            .iter()
            .map(|r| r.rect_without_leading_space().translate(at).intersect(ink))
            .filter(|r| r.is_positive())
            .collect()
    } else {
        vec![ink]
    };
    Some(Text { text: key_of(text), ink, lines, clip, size, elided: t.galley.elided })
}

fn check(
    texts: &[Text],
    frames: &[(Rect, Rect)],
    bars: &[Rect],
    on_purpose: &HashSet<String>,
    body: f32,
    layer: &str,
    found: &mut Vec<Finding>,
) {
    let finding = |kind, t: &Text, other: &str, by: f32| Finding {
        kind,
        text: t.text.clone(),
        other: other.to_string(),
        by,
        rect: t.ink,
        layer: layer.to_string(),
    };
    for t in texts {
        let (ink, clip) = (t.ink, t.clip);
        // cut at a side: what of it is hidden there
        let left = clip.min.x - ink.min.x;
        let right = ink.max.x - clip.max.x;
        if left.max(right) > SLACK && !scrolls(clip, bars, false) {
            found.push(finding(Kind::Clipped, t, "", left.max(right)));
        }
        // taller than its place: cut at the top and the bottom both; else cut at the bottom only
        let (above, below) = (clip.min.y - ink.min.y, ink.max.y - clip.max.y);
        if above > SLACK && below > SLACK {
            found.push(finding(Kind::TooTall, t, "", ink.height() - clip.height()));
        } else if below > SLACK && !scrolls(clip, bars, true) {
            found.push(finding(Kind::CutBelow, t, "", below));
        }
        if t.elided && !on_purpose.contains(&t.text) {
            found.push(finding(Kind::Elided, t, "", 0.0));
        }
        // a size of its own, well under the body's once the setting makes the body large
        if body >= 18.0 && t.size > 0.0 && t.size < body * 0.62 {
            found.push(finding(Kind::Small, t, "", body - t.size));
        }
        // the smallest frame its middle is in, of a widget's size (not a panel's background), drawn in the same
        // clip: the text past its sides
        let c = ink.center();
        let frame = frames
            .iter()
            .filter(|(f, fclip)| {
                f.contains(c) && f.height() < ink.height() * 3.0 + 12.0 && f.width() < 900.0 && *fclip == clip
            })
            .min_by(|a, b| a.0.area().total_cmp(&b.0.area()));
        if let Some((f, _)) = frame {
            let past = (f.min.x - ink.min.x).max(ink.max.x - f.max.x).max(f.min.y - ink.min.y).max(ink.max.y - f.max.y);
            if past > SLACK {
                found.push(finding(Kind::OutOfFrame, t, "", past));
            }
        }
    }
    // a text over another: their visible lines crossing by more than a sliver (a shadow, the same text drawn
    // twice at a point's offset, is not)
    let visible: Vec<Rect> = texts.iter().map(|t| t.ink.intersect(t.clip)).collect();
    let mut order: Vec<usize> = (0..texts.len()).collect();
    order.sort_by(|&a, &b| visible[a].min.x.total_cmp(&visible[b].min.x));
    for (k, &i) in order.iter().enumerate() {
        let a = visible[i];
        for &j in &order[k + 1..] {
            let b = visible[j];
            if b.min.x > a.max.x {
                break;
            }
            let (ti, tj) = (&texts[i], &texts[j]);
            // where their lines cross most
            let x = ti
                .lines
                .iter()
                .flat_map(|p| tj.lines.iter().map(move |q| p.intersect(*q).intersect(a).intersect(b)))
                .filter(|x| x.is_positive())
                .max_by(|p, q| p.area().total_cmp(&q.area()));
            let Some(x) = x.filter(|x| x.width() >= 2.0 && x.height() >= 2.0) else { continue };
            let shadow = ti.text == tj.text && (ti.ink.min - tj.ink.min).length() < 3.0;
            let small = a.area().min(b.area());
            if !shadow && x.area() > 0.15 * small {
                found.push(finding(Kind::Overlap, ti, &tj.text, x.width().min(x.height())));
            }
        }
    }
}
