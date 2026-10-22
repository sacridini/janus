//! NDFI (zeit_bridge/tool_ndfi.py): zeit.unmix's spectral mixture analysis of every observation with the
//! endmembers of Souza et al. (2005), the fractions summarised and the NDFI drawn when the chart shows an index.

use crate::bands::{LANDSAT_ROLES, landsat_chunk, missing_roles, series_overlay, shown_is_index};
use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::{Params, PixelInput, PixelResult, row};
use crate::upstream::{Fractions, np_sum, unmix_souza};

const FRACTIONS: [(&str, &str); 4] =
    [("gv", "GV fraction"), ("npv", "NPV fraction"), ("soil", "Soil fraction"), ("shade", "Shade fraction")];

/// Why the tool cannot run on this series, if it cannot.
pub(crate) fn check(input: &PixelInput) -> Option<String> {
    let missing = missing_roles(input, &LANDSAT_ROLES);
    if !missing.is_empty() {
        return Some(format!("map the bands first: {}", missing.join(", ")));
    }
    input.days.is_none().then(|| "needs real dates".into())
}

/// zeit.unmix of a chunk's mapped bands ([T][pixels]): NaN where the quality band or the cloud fraction leaves an
/// observation out.
pub(crate) fn unmix_chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<Fractions, String> {
    let (cube, _) = landsat_chunk(p, chunk, ctx, &LANDSAT_ROLES, true)?;
    let ct = p.f64("cloud_threshold");
    Ok(unmix_souza(&cube, (ct < 1.0).then_some(ct)))
}

/// A pixel's unmixing.
pub(crate) fn unmix(p: &Params, input: &PixelInput) -> Result<Fractions, String> {
    unmix_chunk(p, &input.as_chunk(), &JobCtx::for_pixel(input))
}

/// A value as Python's f"{v:.3f}".
pub(crate) fn f3(v: f64) -> String {
    if v.is_nan() { "nan".into() } else { format!("{v:.3}") }
}

/// np.nanmean: numpy's sum of the values with NaN as 0, over the count of the others.
pub(crate) fn nanmean(v: &[f64]) -> f64 {
    let zeroed: Vec<f64> = v.iter().map(|&x| if x.is_nan() { 0.0 } else { x }).collect();
    np_sum(&zeroed) / v.iter().filter(|x| !x.is_nan()).count() as f64
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if let Some(why) = check(input) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("NDFI", why)] });
    }
    let fr = unmix(p, input)?;
    let ndfi: Vec<f64> = fr.ndfi.iter().map(|&v| f64::from(v)).collect();
    let ok: Vec<f64> = ndfi.iter().copied().filter(|v| v.is_finite()).collect();
    let mut rows = vec![row("Observations used", format!("{} of {}", ok.len(), ndfi.len()))];
    if ok.is_empty() {
        rows.push(row("NDFI", "no clear observation"));
        return Ok(PixelResult { overlays: vec![], rows });
    }
    rows.push(row("Mean NDFI", f3(np_sum(&ok) / ok.len() as f64)));
    rows.push(row("Lowest NDFI", f3(ok.iter().copied().fold(f64::INFINITY, f64::min))));
    rows.push(row("Highest NDFI", f3(ok.iter().copied().fold(f64::NEG_INFINITY, f64::max))));
    for (f, name) in FRACTIONS {
        let v: Vec<f64> = fr.get(f).iter().map(|&x| f64::from(x)).collect();
        rows.push(row(format!("Mean {name}"), f3(nanmean(&v))));
    }
    let mut overlays = vec![];
    if shown_is_index(input) {
        overlays.push(series_overlay("NDFI", input, &ndfi, "line"));
    } else {
        rows.push(row("NDFI on the chart", "drawn when the chart shows an index (values in -1..1)"));
    }
    Ok(PixelResult { overlays, rows })
}

pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let with_bands = PixelInput { bands: chunk.bands.clone(), ..ctx.input.clone() };
    if let Some(why) = check(&with_bands) {
        return Err(format!("ValueError: NDFI: {why}"));
    }
    let fr = unmix_chunk(p, chunk, ctx)?;
    let (t, n) = (chunk.t, chunk.pixels());
    let ndfi: Vec<f64> = fr.ndfi.iter().map(|&v| f64::from(v)).collect();
    let (mut mean, mut min, mut count) = (vec![f64::NAN; n], vec![f64::NAN; n], vec![f64::NAN; n]);
    for px in 0..n {
        // np.nanmean / np.nanmin over the dates (a reduction across rows: in date order)
        let (mut sum, mut k, mut lo) = (0.0, 0usize, f64::NAN);
        for i in 0..t {
            let v = ndfi[i * n + px];
            if !v.is_nan() {
                sum += v;
                k += 1;
                lo = if lo.is_nan() || v < lo { v } else { lo };
            }
        }
        mean[px] = sum / k as f64;
        min[px] = lo;
        if k > 0 {
            count[px] = k as f64;
        }
    }
    let mut res = ChunkOut::new();
    res.insert("ndfi_mean".into(), mean);
    res.insert("ndfi_min".into(), min);
    res.insert("n_obs".into(), count);
    res.insert("ndfi".into(), ndfi);
    if p.bool("fractions") {
        for (f, _) in FRACTIONS {
            res.insert(f.into(), fr.get(f).iter().map(|&v| f64::from(v)).collect());
        }
    }
    Ok(res)
}
