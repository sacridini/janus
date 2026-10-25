//! What the calculator's editor shows of a text: each token's class (its colour), the completions at the cursor,
//! the description of the name under the pointer, and the layers' and indices' texts it inserts.

use std::ops::Range;

use super::funcs::{self, CONSTANTS, DATE_VARS, FUNCS, RESULT};
use super::indices::SpectralIndex;
use super::lex::{Punct, Tok, Token, tokens};
use super::names::{BandNames, Resolved, role};
use super::plan::{CalcLayer, LayerKind, kind, member_names};

/// What a token is, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Number,
    Str,
    Comment,
    Operator,
    Bracket,
    /// and, or, not
    Keyword,
    Function,
    Constant,
    DateVar,
    /// a layer's short name (or its quoted name before a dot)
    Layer,
    /// a band of a layer (after the dot, or of the primary layer)
    Band,
    /// a name given a value in the text
    Local,
    /// `result`
    Result,
    /// `$3`
    Input,
    /// a name of nothing known
    Unknown,
}

/// The names the text gives values (`ndvi = ...` at the start of a statement).
fn locals(toks: &[Token], text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = true;
    for (i, t) in toks.iter().enumerate() {
        match t.tok {
            Tok::Newline | Tok::Semi => start = true,
            Tok::Comment => {}
            Tok::Ident if start => {
                if matches!(toks.get(i + 1).map(|n| &n.tok), Some(Tok::Punct(Punct::Assign))) {
                    let name = text[t.span.clone()].to_string();
                    if name != RESULT && !out.contains(&name) {
                        out.push(name);
                    }
                }
                start = false;
            }
            _ => start = false,
        }
    }
    out
}

/// The tokens without comments and newlines' neighbours (what comes before and after a token).
fn next_tok(toks: &[Token], i: usize) -> Option<&Tok> {
    toks[i + 1..].iter().map(|t| &t.tok).find(|t| !matches!(t, Tok::Comment))
}

fn prev_tok(toks: &[Token], i: usize) -> Option<&Token> {
    toks[..i].iter().rev().find(|t| !matches!(t.tok, Tok::Comment))
}

fn layer_named<'a>(layers: &'a [CalcLayer], name: &str) -> Option<&'a CalcLayer> {
    layers
        .iter()
        .find(|l| l.alias == name || l.name == name)
        .or_else(|| layers.iter().find(|l| l.alias.eq_ignore_ascii_case(name) || l.name.eq_ignore_ascii_case(name)))
}

/// The layer before the dot at token i (`s2.nir`, `"my layer".nir`).
fn base_layer<'a>(toks: &[Token], i: usize, text: &str, layers: &'a [CalcLayer]) -> Option<&'a CalcLayer> {
    let dot = prev_tok(toks, i)?;
    if dot.tok != Tok::Punct(Punct::Dot) {
        return None;
    }
    let k = toks.iter().position(|t| t.span == dot.span)?;
    let base = prev_tok(toks, k)?;
    match &base.tok {
        Tok::Ident => layer_named(layers, &text[base.span.clone()]),
        Tok::Str { text: s, .. } => layer_named(layers, s),
        _ => None,
    }
}

/// Each token's class (comments included; spaces and newlines left out).
pub fn classify(text: &str, layers: &[CalcLayer], primary: Option<usize>) -> Vec<(Range<usize>, Class)> {
    let toks = tokens(text);
    let locals = locals(&toks, text);
    let primary_names = primary.and_then(|p| layers.get(p)).map(|l| member_names(&l.info));
    let mut out = Vec::with_capacity(toks.len());
    for (i, t) in toks.iter().enumerate() {
        let c = match &t.tok {
            Tok::Num(_) => Class::Number,
            Tok::Str { .. } => {
                if next_tok(&toks, i) == Some(&Tok::Punct(Punct::Dot)) {
                    Class::Layer
                } else {
                    Class::Str
                }
            }
            Tok::Comment => Class::Comment,
            Tok::Input(_) => Class::Input,
            Tok::Punct(Punct::LParen | Punct::RParen | Punct::LBracket | Punct::RBracket) => Class::Bracket,
            Tok::Punct(_) | Tok::Semi => Class::Operator,
            Tok::Unknown(_) => Class::Unknown,
            Tok::Newline | Tok::Eof => continue,
            Tok::Ident => {
                let name = &text[t.span.clone()];
                let after_dot = prev_tok(&toks, i).is_some_and(|p| p.tok == Tok::Punct(Punct::Dot));
                if after_dot {
                    match base_layer(&toks, i, text, layers) {
                        Some(l) if matches!(member_names(&l.info).resolve(name), Some(Resolved::Band(_))) => {
                            Class::Band
                        }
                        Some(_) => Class::Unknown,
                        None => Class::Band,
                    }
                } else if matches!(name, "and" | "or" | "not") {
                    Class::Keyword
                } else if name == RESULT {
                    Class::Result
                } else if locals.iter().any(|l| l == name) {
                    Class::Local
                } else if next_tok(&toks, i) == Some(&Tok::Punct(Punct::LParen)) && funcs::func(name).is_some() {
                    Class::Function
                } else if layers.iter().any(|l| l.alias == name) {
                    Class::Layer
                } else if super::program::DateVar::of(name).is_some() {
                    Class::DateVar
                } else if funcs::constant(name).is_some() {
                    Class::Constant
                } else if primary_names.as_ref().is_some_and(|n| n.resolve(name).is_some()) {
                    Class::Band
                } else if funcs::func(name).is_some() {
                    Class::Function
                } else {
                    Class::Unknown
                }
            }
        };
        out.push((t.span.clone(), c));
    }
    out
}

/// A completion offered at the cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    /// what the list shows
    pub label: String,
    /// what replaces the word typed
    pub insert: String,
    /// a note beside it (a band's name, a function's arguments)
    pub detail: String,
    pub class: Class,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The members of a layer to complete: its roles, then its bands by name, then by place.
fn members(l: &CalcLayer) -> Vec<Completion> {
    let names = member_names(&l.info);
    let mut out: Vec<Completion> = Vec::new();
    let mut push = |label: String, detail: String| {
        if !out.iter().any(|c| c.label == label) {
            out.push(Completion { insert: label.clone(), label, detail, class: Class::Band });
        }
    };
    for &(code, b) in &names.roles {
        let what = role(code).map_or("", |r| r.name);
        push(code.to_string(), format!("{what}: {}", names.describe(b)));
    }
    for (k, n) in names.names.iter().enumerate() {
        let word: String = n.split(|c: char| !is_ident_char(c)).find(|w| !w.is_empty()).unwrap_or("").to_string();
        if !word.is_empty()
            && word.chars().next().is_some_and(char::is_alphabetic)
            && matches!(names.resolve(&word), Some(Resolved::Band(b)) if b == k as i32 + 1)
        {
            push(word, names.describe(k as i32 + 1));
        }
    }
    for k in 1..=names.len() {
        push(format!("band{k}"), names.describe(k as i32));
    }
    out
}

/// The completions of the word ending at `cursor` (a byte offset): the range it replaces and the candidates, the
/// best first. None when the cursor is not after a word (or a layer's dot).
pub fn complete(
    text: &str,
    cursor: usize,
    layers: &[CalcLayer],
    primary: Option<usize>,
) -> Option<(Range<usize>, Vec<Completion>)> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let before = &text[..cursor];
    let start = before.char_indices().rev().take_while(|(_, c)| is_ident_char(*c)).last().map_or(cursor, |(i, _)| i);
    let word = &text[start..cursor];
    // no completion inside a comment or a string
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    if line.contains('#') || line.matches('"').count() % 2 == 1 {
        return None;
    }
    if word.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut cands: Vec<Completion> = Vec::new();
    if before[..start].ends_with('.') {
        // a layer's members
        let base_end = start - 1;
        let base = &text[..base_end];
        let layer = if let Some(quoted) = base.strip_suffix('"') {
            let open = quoted.rfind('"')?;
            layer_named(layers, &quoted[open + 1..])
        } else {
            let b0 = base.char_indices().rev().take_while(|(_, c)| is_ident_char(*c)).last().map(|(i, _)| i)?;
            layer_named(layers, &base[b0..])
        }?;
        cands = members(layer);
    } else {
        if word.is_empty() {
            return None;
        }
        let toks = tokens(text);
        for l in locals(&toks, text) {
            if l != word {
                cands.push(Completion {
                    insert: l.clone(),
                    label: l,
                    detail: "defined above".into(),
                    class: Class::Local,
                });
            }
        }
        for l in layers {
            cands.push(Completion {
                label: l.alias.clone(),
                insert: l.alias.clone(),
                detail: layer_kind_text(l),
                class: Class::Layer,
            });
        }
        if let Some(p) = primary.and_then(|p| layers.get(p)) {
            cands.extend(members(p).into_iter().filter(|c| !c.label.starts_with("band")));
        }
        for f in FUNCS {
            cands.push(Completion {
                label: f.name.into(),
                insert: format!("{}(", f.name),
                detail: format!("{}({})", f.name, f.args),
                class: Class::Function,
            });
        }
        for (v, doc) in DATE_VARS {
            cands.push(Completion {
                label: (*v).into(),
                insert: (*v).into(),
                detail: (*doc).into(),
                class: Class::DateVar,
            });
        }
        for (c, _, doc) in CONSTANTS {
            cands.push(Completion {
                label: (*c).into(),
                insert: (*c).into(),
                detail: (*doc).into(),
                class: Class::Constant,
            });
        }
        cands.push(Completion {
            label: RESULT.into(),
            insert: RESULT.into(),
            detail: "the value so far (rules)".into(),
            class: Class::Result,
        });
    }
    let lw = word.to_lowercase();
    let mut scored: Vec<(u8, usize, Completion)> = cands
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let ll = c.label.to_lowercase();
            let s = if c.label.starts_with(word) {
                0
            } else if ll.starts_with(&lw) {
                1
            } else if !lw.is_empty() && ll.contains(&lw) {
                2
            } else {
                return None;
            };
            Some((s, i, c))
        })
        .collect();
    // the word as typed is not offered again
    scored.retain(|s| s.2.label != word || s.2.insert != word);
    scored.sort_by_key(|s| (s.0, s.1));
    let mut seen: Vec<String> = Vec::new();
    let list: Vec<Completion> = scored
        .into_iter()
        .map(|s| s.2)
        .filter(|c| {
            let new = !seen.contains(&c.label);
            seen.push(c.label.clone());
            new
        })
        .take(40)
        .collect();
    (!list.is_empty()).then_some((start..cursor, list))
}

/// "series of 23 dates × 7 bands", "image", "stack of 6 bands".
pub fn layer_kind_text(l: &CalcLayer) -> String {
    let i = &l.info;
    match kind(i) {
        LayerKind::Bands if i.t() > 1 => format!("series of {} dates × {} bands", i.t(), i.bands_per_date),
        LayerKind::Bands => format!("image of {} bands", i.bands_per_date),
        LayerKind::Stack => format!("stack of {} bands", i.t()),
        LayerKind::Plain if i.t() > 1 => format!("series of {} dates", i.t()),
        LayerKind::Plain => "image".into(),
    }
}

/// A layer described: its kind, dates, grid.
pub fn layer_summary(l: &CalcLayer) -> String {
    let i = &l.info;
    let mut s = format!("{} \"{}\": {}", l.alias, l.name, layer_kind_text(l));
    if i.t() > 1 && i.time_is_date && kind(i) != LayerKind::Stack {
        s += &format!(", {} to {}", i.format_time(i.layers[0].time), i.format_time(i.layers[i.t() - 1].time));
    }
    s += &format!("\n{} × {} cells", i.width, i.height);
    if let Some(g) = i.geo_transform {
        s += &format!(" of {}", super::super::multidim::format_g(g[1].abs(), 6));
    }
    if !i.crs_name.is_empty() {
        s += &format!(", {}", i.crs_name);
    }
    s
}

fn member_text(l: &CalcLayer, names: &BandNames, name: &str) -> String {
    match names.resolve(name) {
        Some(Resolved::Band(b)) => {
            let nm = l.info.band_nm.get(b as usize - 1).copied().filter(|w| *w > 0.0);
            let what = match kind(&l.info) {
                LayerKind::Stack => "one image of the stack",
                _ if l.info.t() > 1 => "at each date",
                _ => "",
            };
            let mut s = format!("{}.{name}: {}", l.alias, names.describe(b));
            if let Some(w) = nm {
                s += &format!(", {w:.0} nm");
            }
            if !what.is_empty() {
                s += &format!(" ({what})");
            }
            s
        }
        Some(Resolved::MissingRole(c)) => {
            format!("\"{}\" has no band that is {} ({c})", l.name, role(c).map_or(c, |r| r.name))
        }
        Some(Resolved::Ambiguous(v)) => format!("'{name}' could be {} of \"{}\"", names.candidates(&v), l.name),
        None => format!("\"{}\" has no band '{name}'", l.name),
    }
}

/// The description of what is at `offset` (a byte offset): the token's range and its text.
pub fn describe_at(
    text: &str,
    offset: usize,
    layers: &[CalcLayer],
    primary: Option<usize>,
) -> Option<(Range<usize>, String)> {
    let toks = tokens(text);
    let i = toks.iter().position(|t| t.span.start <= offset && offset < t.span.end)?;
    let t = &toks[i];
    let name = &text[t.span.clone()];
    let d = match &t.tok {
        Tok::Str { text: s, .. } => layer_named(layers, s).map(layer_summary)?,
        Tok::Ident => {
            let after_dot = prev_tok(&toks, i).is_some_and(|p| p.tok == Tok::Punct(Punct::Dot));
            if after_dot {
                let l = base_layer(&toks, i, text, layers)?;
                member_text(l, &member_names(&l.info), name)
            } else if name == RESULT {
                "result: the value so far; a rule `cond = value` sets it where cond holds".into()
            } else if locals(&toks, text).iter().any(|l| l == name) {
                format!("{name}: a value defined in the text")
            } else if let Some(f) = funcs::func(name) {
                format!("{}({})\n{}", f.name, f.args, f.doc)
            } else if let Some(l) = layers.iter().find(|l| l.alias == name) {
                layer_summary(l)
            } else if let Some((v, doc)) = DATE_VARS.iter().find(|v| v.0 == name) {
                format!("{v}: {doc}")
            } else if let Some((c, _, doc)) =
                CONSTANTS.iter().find(|c| c.0 == name || (c.0 == "nan" && name.eq_ignore_ascii_case("nan")))
            {
                format!("{c}: {doc}")
            } else if matches!(name, "and" | "or" | "not") {
                format!(
                    "{name}: logic (also {})",
                    match name {
                        "and" => "&&",
                        "or" => "||",
                        _ => "!",
                    }
                )
            } else {
                let l = primary.and_then(|p| layers.get(p))?;
                let names = member_names(&l.info);
                names.resolve(name)?;
                member_text(l, &names, name)
            }
        }
        _ => return None,
    };
    Some((t.span.clone(), d))
}

/// An index's formula for a layer: its roles as the layer's bands (`s2.N`, or `N` when the layer's bands are the
/// bare names), constants and wavelengths in place. None if the layer lacks a band it needs.
pub fn index_text(ix: &SpectralIndex, l: &CalcLayer, bare: bool) -> Option<String> {
    let names = member_names(&l.info);
    if !ix.usable(&names) {
        return None;
    }
    let f = ix.formula_for(&names, &l.info.band_nm)?;
    if bare {
        return Some(f);
    }
    let mut out = String::with_capacity(f.len() + 16);
    let mut at = 0;
    for t in tokens(&f) {
        if t.tok == Tok::Ident && role(&f[t.span.clone()]).is_some() {
            out += &f[at..t.span.start];
            out += &l.alias;
            out.push('.');
            out += &f[t.span.clone()];
            at = t.span.end;
        }
    }
    out += &f[at..];
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CubeInfo, Layer};
    use std::sync::Arc;

    fn layer(alias: &str, names: &[&str], dates: usize) -> CalcLayer {
        let info = CubeInfo {
            layers: (0..dates)
                .map(|t| Layer {
                    path: format!("{alias}{t}.tif"),
                    band: 1,
                    time: t as f64 * 86400.0,
                    ..Default::default()
                })
                .collect(),
            width: 10,
            height: 10,
            bands_per_date: names.len(),
            band_names: names.iter().map(|s| s.to_string()).collect(),
            time_is_date: true,
            ..Default::default()
        };
        CalcLayer { name: alias.into(), alias: alias.into(), info: Arc::new(info) }
    }

    fn layers() -> Vec<CalcLayer> {
        vec![layer("s2", &["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR 2"], 3), layer("dem", &["Elevation"], 1)]
    }

    #[test]
    fn classes() {
        let ls = layers();
        let text = "ndvi = (s2.nir - s2.red) / (s2.N + R) # x\nwhere(ndvi > 0.5 and dem, month, nan) + foo";
        let c = classify(text, &ls, Some(0));
        let of = |s: &str| c.iter().find(|(r, _)| &text[r.clone()] == s).map(|x| x.1);
        assert_eq!(of("ndvi"), Some(Class::Local));
        assert_eq!(of("s2"), Some(Class::Layer));
        assert_eq!(of("nir"), Some(Class::Band));
        assert_eq!(of("R"), Some(Class::Band));
        assert_eq!(of("# x"), Some(Class::Comment));
        assert_eq!(of("where"), Some(Class::Function));
        assert_eq!(of("and"), Some(Class::Keyword));
        assert_eq!(of("month"), Some(Class::DateVar));
        assert_eq!(of("nan"), Some(Class::Constant));
        assert_eq!(of("foo"), Some(Class::Unknown));
        assert_eq!(of("0.5"), Some(Class::Number));
    }

    #[test]
    fn completions() {
        let ls = layers();
        let (r, c) = complete("s2.n", 4, &ls, None).unwrap();
        assert_eq!(r, 3..4);
        assert!(c.iter().any(|c| c.label == "nir" || c.label == "NIR"), "{c:?}");
        assert!(c.iter().any(|c| c.label == "N"));
        let (r, c) = complete("x = me", 6, &ls, None).unwrap();
        assert_eq!(r, 4..6);
        assert_eq!(c[0].label, "mean");
        assert_eq!(c[0].insert, "mean(");
        let (_, c) = complete("d", 1, &ls, None).unwrap();
        assert!(c.iter().any(|c| c.label == "dem" && c.class == Class::Layer));
        assert!(complete("s2.red # me", 11, &ls, None).is_none());
        assert!(complete("1", 1, &ls, None).is_none());
        // after the dot, every member
        let (_, c) = complete("s2.", 3, &ls, None).unwrap();
        assert!(c.len() >= 6);
    }

    #[test]
    fn descriptions() {
        let ls = layers();
        let text = "max(s2.nir) - dem";
        let (r, d) = describe_at(text, 8, &ls, None).unwrap();
        assert_eq!(&text[r], "nir");
        assert!(d.contains("band 4") && d.contains("NIR"), "{d}");
        assert!(describe_at(text, 1, &ls, None).unwrap().1.starts_with("max("));
        assert!(describe_at(text, 15, &ls, None).unwrap().1.contains("image"));
        let ix = super::super::indices::find("NDVI").unwrap();
        assert_eq!(index_text(ix, &ls[0], false).unwrap(), "(s2.N - s2.R)/(s2.N + s2.R)");
        assert!(index_text(super::super::indices::find("NDREI").unwrap(), &ls[0], false).is_none());
    }
}
