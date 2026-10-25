//! The raster calculator's language and engine (phase 39): band math over a layer's bands (an index of the
//! Awesome Spectral Indices, an expression typed in the Display panel) evaluated by the reader, and expressions
//! over the open layers (Tools > Raster calculator) written as VRTs whose derived bands a pixel function of ours
//! computes, so every part of Janus reads them as any other series.
//!
//! ```text
//! (N - R) / (N + R)                      bands by role, name (B08, nir) or place (band4)
//! ndvi = nd(N, R); where(ndvi > 0.3, ndvi, nan)
//! img > 0 = 1                            rules: where the condition holds, the result takes the value
//! img == nan = 0
//! "S2 L2A".N - landsat.nir               layers by name or short name
//! max(series), median(ndvi), count(ndvi < 0.3)   over time: a series to one image
//! ```

pub mod canonical;
pub mod compile;
pub mod editor;
pub mod funcs;
pub mod indices;
pub mod lex;
pub mod names;
pub mod parse;
pub mod plan;
pub mod program;
pub mod python;
pub mod vrt;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

pub use compile::{Body, Compiled, Member, RedFn, Scope, Value, compile};
pub use names::{BandNames, Resolved};
pub use parse::{CalcError, parse};
pub use program::{Builder, DateVars, Program};

/// An expression of a band selection, kept by its text in a registry so that a selection stays a small value
/// that can be copied (0: none).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ExprId(u32);

/// (name, text) of each expression interned, from id 1.
static REGISTRY: LazyLock<Mutex<Vec<(String, String)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

impl ExprId {
    pub const NONE: ExprId = ExprId(0);

    /// The id of an expression (the same text and name, the same id).
    pub fn intern(name: &str, text: &str) -> ExprId {
        let mut r = REGISTRY.lock().unwrap();
        if let Some(i) = r.iter().position(|(n, t)| n == name && t == text) {
            return ExprId(i as u32 + 1);
        }
        r.push((name.to_string(), text.to_string()));
        ExprId(r.len() as u32)
    }

    pub fn is_some(self) -> bool {
        self.0 > 0
    }

    pub fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Its text ("" for none).
    pub fn text(self) -> String {
        if self.0 == 0 {
            return String::new();
        }
        REGISTRY.lock().unwrap().get(self.0 as usize - 1).map(|e| e.1.clone()).unwrap_or_default()
    }

    /// Its name ("EVI"; "" when typed without one).
    pub fn name(self) -> String {
        if self.0 == 0 {
            return String::new();
        }
        REGISTRY.lock().unwrap().get(self.0 as usize - 1).map(|e| e.0.clone()).unwrap_or_default()
    }

    /// The name, else the text.
    pub fn label(self) -> String {
        let n = self.name();
        if n.is_empty() { self.text() } else { n }
    }
}

fn layer_error(r: Resolved, id: &str, names: &BandNames, span: &Range<usize>) -> Option<Result<u32, CalcError>> {
    match r {
        Resolved::Band(b) => Some(Ok(b as u32)),
        Resolved::MissingRole(code) => {
            let what = names::role(code).map_or(code, |r| r.name);
            Some(Err(CalcError::new(
                span.clone(),
                format!("no band of this layer is {what} ({code}): name one of its bands (band1..band{})", names.len()),
            )))
        }
        Resolved::Ambiguous(v) => Some(Err(CalcError::new(
            span.clone(),
            format!("'{id}' could be {}: write one of them", names.candidates(&v)),
        ))),
    }
}

/// The bands of one layer (the Display panel's expressions): inputs are bands of the date (1-based).
pub struct LayerScope<'a> {
    pub names: &'a BandNames,
}

impl Scope for LayerScope<'_> {
    type K = i32;

    fn name(&mut self, b: &mut Builder<i32>, name: &str, span: &Range<usize>) -> Option<Result<Value, CalcError>> {
        let r = self.names.resolve(name)?;
        Some(layer_error(r, name, self.names, span)?.map(|band| Value { node: b.input(band as i32), series: Some(0) }))
    }

    fn suggestions(&self) -> Vec<String> {
        self.names.suggestions()
    }
}

/// A layer's expression compiled over its bands.
#[derive(Debug, PartialEq)]
pub struct LayerExpr {
    pub text: String,
    pub program: Program,
    /// each input's band of the date (1-based)
    pub bands: Vec<i32>,
    /// the name the text gives its result, if any
    pub result_name: Option<String>,
}

impl LayerExpr {
    pub fn compile(text: &str, names: &BandNames) -> Result<LayerExpr, CalcError> {
        let ast = parse(text)?;
        let c = compile(&ast, &mut LayerScope { names })?;
        Ok(LayerExpr {
            text: text.to_string(),
            program: c.program(),
            bands: c.builder.inputs.clone(),
            result_name: c.name,
        })
    }

    /// The highest band it reads.
    pub fn max_band(&self) -> i32 {
        self.bands.iter().copied().max().unwrap_or(1)
    }
}

/// The inputs of a canonical text: `$1..$n`, date variables, nothing else.
struct CanonicalScope {
    n: u32,
}

impl Scope for CanonicalScope {
    type K = u32;

    fn name(&mut self, _: &mut Builder<u32>, _: &str, _: &Range<usize>) -> Option<Result<Value, CalcError>> {
        None
    }

    fn input(&mut self, b: &mut Builder<u32>, k: u32, span: &Range<usize>) -> Result<Value, CalcError> {
        if k == 0 || k > self.n {
            return Err(CalcError::new(span.clone(), format!("${k}: there are {} inputs", self.n)));
        }
        Ok(Value { node: b.input(k - 1), series: None })
    }
}

/// A canonical text compiled over `n` inputs, `$k` being input k - 1.
pub fn compile_canonical(text: &str, n: usize) -> Result<Program, CalcError> {
    let ast = parse(text)?;
    let mut b: Builder<u32> = Builder::default();
    for k in 0..n as u32 {
        b.input(k);
    }
    let c = compile::compile_with(&ast, &mut CanonicalScope { n: n as u32 }, b)?;
    let mut p = c.program();
    p.inputs = n;
    Ok(p)
}

/// The canonical texts compiled so far (the pixel function compiles each once).
type Programs = HashMap<(String, usize), Arc<Program>>;

static PROGRAMS: LazyLock<Mutex<Programs>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn cached_canonical(text: &str, n: usize) -> Result<Arc<Program>, CalcError> {
    let key = (text.to_string(), n);
    if let Some(p) = PROGRAMS.lock().unwrap().get(&key) {
        return Ok(p.clone());
    }
    let p = Arc::new(compile_canonical(text, n)?);
    let mut m = PROGRAMS.lock().unwrap();
    if m.len() > 256 {
        m.clear();
    }
    m.insert(key, p.clone());
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn landsat() -> BandNames {
        let names = ["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR 2", "QA_PIXEL"];
        BandNames::new(names.iter().map(|s| s.to_string()).collect(), &[])
    }

    fn run(text: &str, bands: &[f32]) -> f32 {
        let e = LayerExpr::compile(text, &landsat()).unwrap_or_else(|e| panic!("{text}: {e:?}"));
        let inputs: Vec<f32> = e.bands.iter().map(|&b| bands[b as usize - 1]).collect();
        e.program.eval_one(&inputs, &DateVars::new(Some(1594684800.0), 0))
    }

    #[test]
    fn indices_over_bands() {
        let px = [0.04, 0.07, 0.05, 0.35, 0.18, 0.09, 21824.0];
        let ndvi = (0.35f32 - 0.05) / (0.35 + 0.05);
        assert!((run("(N - R) / (N + R)", &px) - ndvi).abs() < 1e-6);
        assert!((run("nd(nir, red)", &px) - ndvi).abs() < 1e-6);
        let evi = 2.5 * (0.35 - 0.05) / (0.35 + 6.0 * 0.05 - 7.5 * 0.04 + 1.0);
        assert!((run("2.5 * (N - R) / (N + 6 * R - 7.5 * B + 1)", &px) - evi).abs() < 1e-6);
        assert_eq!(run("ndvi = nd(N, R)\nwhere(ndvi > 0.9, 1, 0)", &px), 0.0);
        // Landsat's cloud bit
        assert_eq!(run("bit(QA_PIXEL, 3)", &px), 0.0);
        // the date: 2020-07-14
        assert_eq!(run("month", &px), 7.0);
        let e = LayerExpr::compile("(N - R) / (N + R)", &landsat()).unwrap();
        assert_eq!((e.bands.clone(), e.max_band()), (vec![4, 3], 4));
    }

    #[test]
    fn rules() {
        let px = [3.0, 7.0, f32::NAN, 0.0, 0.0, 0.0, 0.0];
        // img > 0 = 1 on the first band's value
        assert_eq!(run("blue > 0 = 1", &px), 1.0);
        assert_eq!(run("blue > 5 = 1", &px), 3.0);
        assert_eq!(run("band1 > 1 && band2 >= 5 = 0", &px), 0.0);
        assert_eq!(run("band1 > 1 && band2 >= 8 = 0", &px), 3.0);
        // no data: the rule does not apply, then == nan fills it
        assert!(run("red > 0 = 1", &px).is_nan());
        assert_eq!(run("red > 0 = 1\nred == nan = -1", &px), -1.0);
        assert_eq!(run("result = band2\nresult == 7 = nan\nresult == NaN = 0", &px), 0.0);
        assert_eq!(run("in(band2, 5, 7, 9)", &px), 1.0);
    }

    #[test]
    fn errors() {
        let e = |t: &str| LayerExpr::compile(t, &landsat()).unwrap_err();
        assert!(e("RE1 - R").message.contains("Red edge 1"));
        assert!(e("swir - R").message.contains("could be S1 (SWIR 1) or S2 (SWIR 2)"));
        let x = e("(N - Rd)");
        assert_eq!(x.span, 5..7);
        assert!(x.message.contains("did you mean 'R'") || x.message.contains("did you mean 'red'"), "{x:?}");
        assert!(e("sqrt(N, R)").message.contains("takes 1 argument"));
        assert!(e("max(N)").message.contains("Raster calculator"));
        assert!(e("\"S2\".N").message.contains("Raster calculator"));
        assert!(e("where = 1").message.contains("function"));
        assert!(e("foo(N)").message.contains("not a function"));
    }

    #[test]
    fn canonical_round_trip() {
        let names = landsat();
        for text in [
            "(N - R) / (N + R)",
            "2.5 * (N - R) / (N + 6 * R - 7.5 * B + 1)",
            "x = N - R; y = x * x; where(y > 0.01 and not isnan(S1), -y, coalesce(S2, 0.5))",
            "qa(N, QA_PIXEL, \"landsat_c2\") ^ 2 % 3",
            "clamp(nd(N, S1), -0.5, 0.5) + bits(QA_PIXEL, 8, 2) + month / 12 + t",
            "N > 0.3 = 1\nN == nan = -1e-10",
        ] {
            let e = LayerExpr::compile(text, &names).unwrap();
            let canon = canonical::canonical(&e.program);
            let p = compile_canonical(&canon, e.bands.len()).unwrap_or_else(|x| panic!("{canon}: {x:?}"));
            for px in [[0.04, 0.07, 0.05, 0.35, 0.18, 0.09, 21824.0], [0.1, 0.1, 0.4, 0.2, f32::NAN, 0.3, 1.0]] {
                let inputs: Vec<f32> = e.bands.iter().map(|&b| px[b as usize - 1]).collect();
                let vars = DateVars::new(Some(1594684800.0), 5);
                let (a, b) = (e.program.eval_one(&inputs, &vars), p.eval_one(&inputs, &vars));
                assert!(a == b || (a.is_nan() && b.is_nan()), "{text} -> {canon}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn registry() {
        let a = ExprId::intern("EVI", "2.5 * (N - R)");
        assert_eq!(a, ExprId::intern("EVI", "2.5 * (N - R)"));
        assert_ne!(a, ExprId::intern("", "2.5 * (N - R)"));
        assert_eq!((a.name(), a.text(), a.label()), ("EVI".into(), "2.5 * (N - R)".into(), "EVI".into()));
        assert!(ExprId::NONE.is_none() && ExprId::NONE.text().is_empty());
    }
}
