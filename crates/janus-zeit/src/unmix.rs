//! Spectral unmixing with endmembers of the image (zeit.unmix with a table of endmembers): the fraction of each
//! endmember, spectra taken from pins (or the ROI mean) on places of a known cover, in every pixel, and the RMSE
//! of the rebuilt spectrum. Two tools after the bridge, on zeit-core's sma (the same solver as zeit.unmix):
//!
//! - `unmix`, on a series of multiband files: every band of each date (the endmembers are the bands of a pin at
//!   one date), each observation unmixed; the fractions as series, their means as maps;
//! - `unmix_spectrum`, on a scene of one band per wavelength (EnMAP, PRISMA, EMIT...): the pixel's spectrum (the
//!   endmembers are the pins' spectra); the fractions as maps.

use serde_json::Value;
use zeit_core::sma;

use crate::bands::{series_overlay, shown_is_index};
use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// The endmembers of the "endmembers" parameter: their names and spectra (k x bands, in the data's units, NaN
/// where a band has no value).
pub struct Endmembers {
    pub names: Vec<String>,
    pub spectra: Vec<f64>,
    pub bands: usize,
    /// the bands unmixed: those with a value in every endmember (a scene's empty bands, EnMAP's 1331-1391 nm or
    /// the water vapour's, left out)
    pub used: Vec<usize>,
}

impl Endmembers {
    pub fn k(&self) -> usize {
        self.names.len()
    }

    /// What the data and the endmembers are divided by (zeit.unmix's scale): reflectance x 10000 when an
    /// endmember's value is above 2, as zeit's endmember table reads it, else 1.
    pub fn scale(&self) -> f64 {
        if self.spectra.iter().any(|&v| v > 2.0) { 10000.0 } else { 1.0 }
    }
}

/// The "endmembers" parameter: [{"name", "from", "values"}], each `values` a spectrum (null: no value).
pub fn endmembers(p: &Params) -> Result<Endmembers, String> {
    let list = p.0.get("endmembers").and_then(Value::as_array).cloned().unwrap_or_default();
    if list.len() < 2 {
        return Err("add at least two endmembers (pins on places of a known cover: vegetation, soil, water...)".into());
    }
    let mut out = Endmembers { names: vec![], spectra: vec![], bands: 0, used: vec![] };
    for (k, e) in list.iter().enumerate() {
        let name = e.get("name").and_then(Value::as_str).map_or_else(|| format!("Endmember {}", k + 1), str::to_string);
        let values: Vec<f64> = e
            .get("values")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(f64::NAN)).collect())
            .unwrap_or_default();
        if !values.iter().any(|v| v.is_finite()) {
            return Err(format!("the endmember {name:?} has no value: take it from another pin"));
        }
        if k == 0 {
            out.bands = values.len();
        } else if values.len() != out.bands {
            return Err(format!(
                "the endmember {name:?} has {} bands, the first {}: take them all from this layer",
                values.len(),
                out.bands
            ));
        }
        out.names.push(name);
        out.spectra.extend(values);
    }
    let (b, k) = (out.bands, out.k());
    out.used = (0..b).filter(|&i| (0..k).all(|j| out.spectra[j * b + i].is_finite())).collect();
    if out.used.len() < 2 {
        return Err("the endmembers have a value together in fewer than two bands: take them from other pins".into());
    }
    Ok(out)
}

/// The constraints of the "constraint" parameter: (sum to one, non-negative).
fn constraints(p: &Params) -> Result<(bool, bool), String> {
    match p.str("constraint") {
        "fcls" => Ok((true, true)),
        "nnls" => Ok((false, true)),
        "scls" => Ok((true, false)),
        "ols" => Ok((false, false)),
        c => Err(format!("ValueError: constraint {c:?}")),
    }
}

/// zeit.unmix of `n` spectra (n x bands, the data's units) on the endmembers' bands: n x (k + 1), the fractions
/// then the RMSE (in the scaled units). A spectrum with one of those bands missing gives NaN.
fn solve(p: &Params, e: &Endmembers, spectra: &[f64], n: usize, n_jobs: i32) -> Result<Vec<f64>, String> {
    let (sum_to_one, nonneg) = constraints(p)?;
    let s = e.scale();
    let (b, k, u) = (e.bands, e.k(), e.used.len());
    let em: Vec<f64> = (0..k).flat_map(|j| e.used.iter().map(move |&i| e.spectra[j * b + i] / s)).collect();
    let y: Vec<f64> = (0..n).flat_map(|r| e.used.iter().map(move |&i| spectra[r * b + i] / s)).collect();
    let mut out = sma::unmix_batch(&y, n, u, &em, k, sum_to_one, nonneg, 1000.0, n_jobs);
    for i in 0..n {
        if y[i * u..(i + 1) * u].iter().any(|v| !v.is_finite()) {
            out[i * (k + 1)..(i + 1) * (k + 1)].fill(f64::NAN);
        }
    }
    Ok(out)
}

/// A spectrum's fractions and RMSE as rows.
fn fraction_rows(e: &Endmembers, f: &[f64], rows: &mut Vec<[String; 2]>) {
    let k = e.k();
    for (j, name) in e.names.iter().enumerate() {
        rows.push(row(format!("{name} fraction"), g(f[j], 4)));
    }
    rows.push(row("Sum of the fractions", g(f[..k].iter().sum::<f64>(), 4)));
    let unit = if e.scale() == 1.0 { "" } else { " (reflectance)" };
    rows.push(row(format!("RMSE{unit}"), g(f[k], 4)));
}

// ---------------------------------------------------------------------------------------------------------------
// A series of multiband files
// ---------------------------------------------------------------------------------------------------------------

/// Each observation's spectrum (T x bands) from the bands of every date (b001, b002...: in their order), NaN where
/// the shown series has no value (no data, the quality band's mask).
fn observations(bands: &std::collections::BTreeMap<String, Vec<f64>>, shown: &[f64], t: usize, n: usize) -> Vec<f64> {
    let cols: Vec<&Vec<f64>> = bands.values().collect();
    let b = cols.len();
    let mut out = vec![f64::NAN; t * n * b];
    for i in 0..t * n {
        if shown[i].is_finite() {
            for (j, c) in cols.iter().enumerate() {
                out[i * b + j] = c[i];
            }
        }
    }
    out
}

fn check_bands(e: &Endmembers, bands: usize) -> Result<(), String> {
    if bands < 2 {
        return Err("needs several bands per date (a multiband file per date)".into());
    }
    if e.bands != bands {
        return Err(format!(
            "the endmembers have {} bands and this series {bands}: take them from pins on this layer",
            e.bands
        ));
    }
    Ok(())
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let e = match endmembers(p) {
        Ok(e) => e,
        Err(why) => return Ok(PixelResult { overlays: vec![], rows: vec![row("Unmixing", why)] }),
    };
    if let Err(why) = check_bands(&e, input.bands.len()) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Unmixing", why)] });
    }
    let t = input.values.len();
    let k = e.k();
    let out = solve(p, &e, &observations(&input.bands, &input.values, t, 1), t, 1)?;
    let used: Vec<usize> = (0..t).filter(|&i| out[i * (k + 1)].is_finite()).collect();
    let mut rows = vec![row("Observations unmixed", format!("{} of {t}", used.len()))];
    if used.is_empty() {
        return Ok(PixelResult { overlays: vec![], rows });
    }
    // the observation of the map's date, else the last one
    let at = input
        .map_year
        .and_then(|y| {
            used.iter().copied().min_by(|&a, &b| (input.years[a] - y).abs().total_cmp(&(input.years[b] - y).abs()))
        })
        .unwrap_or(used[used.len() - 1]);
    rows.push(row("At the date", g(input.years[at], 7)));
    fraction_rows(&e, &out[at * (k + 1)..(at + 1) * (k + 1)], &mut rows);
    let mean = |j: usize| used.iter().map(|&i| out[i * (k + 1) + j]).sum::<f64>() / used.len() as f64;
    for (j, name) in e.names.iter().enumerate() {
        rows.push(row(format!("Mean {name} fraction"), g(mean(j), 4)));
    }
    let mut overlays = vec![];
    if shown_is_index(input) {
        for (j, name) in e.names.iter().enumerate() {
            let v: Vec<f64> = (0..t).map(|i| out[i * (k + 1) + j]).collect();
            overlays.push(series_overlay(&format!("{name} fraction"), input, &v, "line"));
        }
    } else {
        rows.push(row("Fractions on the chart", "drawn when the chart shows an index (values in -1..1)"));
    }
    Ok(PixelResult { overlays, rows })
}

pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let e = endmembers(p).map_err(|w| format!("ValueError: {w}"))?;
    check_bands(&e, chunk.bands.len()).map_err(|w| format!("ValueError: {w}"))?;
    let (t, n, k) = (chunk.t, chunk.pixels(), e.k());
    let out = solve(p, &e, &observations(&chunk.bands, &chunk.stack, t, n), t * n, ctx.n_jobs)?;
    let mut res = ChunkOut::new();
    // [T][pixels] per endmember, and the mean over the dates of each pixel
    let at = |i: usize, j: usize| out[i * (k + 1) + j];
    for j in 0..=k {
        let series: Vec<f64> = (0..t * n).map(|i| at(i, j)).collect();
        let mean: Vec<f64> = (0..n)
            .map(|px| {
                let (mut s, mut c) = (0.0, 0usize);
                for i in 0..t {
                    let v = series[i * n + px];
                    if v.is_finite() {
                        s += v;
                        c += 1;
                    }
                }
                if c > 0 { s / c as f64 } else { f64::NAN }
            })
            .collect();
        if j < k {
            res.insert(format!("fraction_{}", j + 1), series);
            res.insert(format!("mean_{}", j + 1), mean);
        } else {
            res.insert("rmse".into(), mean);
        }
    }
    res.insert(
        "n_obs".into(),
        (0..n)
            .map(|px| {
                let c = (0..t).filter(|&i| at(i * n + px, 0).is_finite()).count();
                if c > 0 { c as f64 } else { f64::NAN }
            })
            .collect(),
    );
    Ok(res)
}

// ---------------------------------------------------------------------------------------------------------------
// A scene of one band per wavelength
// ---------------------------------------------------------------------------------------------------------------

fn check_spectrum(e: &Endmembers, bands: usize) -> Result<(), String> {
    if e.bands != bands {
        return Err(format!(
            "the endmembers have {} bands and this scene {bands}: take them from pins on this layer",
            e.bands
        ));
    }
    if e.k() > e.used.len() {
        return Err(format!("{} endmembers for {} bands: fewer endmembers than bands", e.k(), e.used.len()));
    }
    Ok(())
}

pub fn spectrum_pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let e = match endmembers(p) {
        Ok(e) => e,
        Err(why) => return Ok(PixelResult { overlays: vec![], rows: vec![row("Unmixing", why)] }),
    };
    if let Err(why) = check_spectrum(&e, input.values.len()) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Unmixing", why)] });
    }
    let out = solve(p, &e, &input.values, 1, 1)?;
    let k = e.k();
    if !out[0].is_finite() {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Unmixing", "bands without a value")] });
    }
    let mut rows = vec![];
    fraction_rows(&e, &out, &mut rows);
    // the spectrum the fractions rebuild, and each endmember's part of it
    let b = e.bands;
    // none on the bands left out, as the pin's spectrum breaks there
    let model: Vec<Option<f64>> = (0..b)
        .map(|i| Some((0..k).map(|j| out[j] * e.spectra[j * b + i]).sum::<f64>()).filter(|v| v.is_finite()))
        .collect();
    let mut overlays = vec![Overlay::new("line", "Rebuilt spectrum", input.years.clone(), model)];
    for (j, name) in e.names.iter().enumerate() {
        let part: Vec<Option<f64>> =
            (0..b).map(|i| Some(out[j] * e.spectra[j * b + i]).filter(|v| v.is_finite())).collect();
        let mut o = Overlay::new("line", &format!("{name} x {}", g(out[j], 3)), input.years.clone(), part);
        o.style = Some("dashed".into());
        o.weight = Some(1.2);
        o.alpha = Some(0.7);
        overlays.push(o);
    }
    Ok(PixelResult { overlays, rows })
}

pub fn spectrum_chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let e = endmembers(p).map_err(|w| format!("ValueError: {w}"))?;
    check_spectrum(&e, chunk.t).map_err(|w| format!("ValueError: {w}"))?;
    let (n, k) = (chunk.pixels(), e.k());
    let out = solve(p, &e, &chunk.by_pixel(&chunk.stack), n, ctx.n_jobs)?;
    let mut res = ChunkOut::new();
    for j in 0..k {
        res.insert(format!("fraction_{}", j + 1), (0..n).map(|px| out[px * (k + 1) + j]).collect());
    }
    res.insert("rmse".into(), (0..n).map(|px| out[px * (k + 1) + k]).collect());
    Ok(res)
}
