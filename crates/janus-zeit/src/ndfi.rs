//! NDFI (zeit_bridge/tool_ndfi.py): zeit.unmix's spectral mixture analysis of every observation with the
//! endmembers of Souza et al. (2005), the fractions summarised and the NDFI drawn when the chart shows an index.

use crate::bands::{LANDSAT_ROLES, landsat_cube, missing_roles, series_overlay, shown_is_index};
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

/// zeit.unmix of the mapped bands: NaN where the quality band or the cloud fraction leaves an observation out.
pub(crate) fn unmix(p: &Params, input: &PixelInput) -> Result<Fractions, String> {
    let (cube, _) = landsat_cube(p, input, &LANDSAT_ROLES, true)?;
    let ct = p.f64("cloud_threshold");
    Ok(unmix_souza(&cube, (ct < 1.0).then_some(ct)))
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
