//! Surface reflectance for the multiband tools (CCDC, NDFI, CODED, Tmask): the bridge's band context (band_ctx,
//! to_fmask) and zeit_common's reflectance units and Landsat cube.

use crate::numeric::median;
use crate::pixel::{Overlay, Params, PixelInput};

pub const LANDSAT_ROLES: [&str; 6] = ["blue", "green", "red", "nir", "swir1", "swir2"];

/// Fmask codes of each date (0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 no observation) from the quality
/// band; without one, 0 where every band has a value and 255 elsewhere (the bridge's to_fmask). Element by
/// element: the bands and the codes in any layout, the same for all.
pub fn fmask_codes(
    bands: &std::collections::BTreeMap<String, Vec<f64>>,
    qa: Option<&[f64]>,
    rule: Option<&str>,
) -> Option<Vec<i32>> {
    let len = bands.values().next()?.len();
    let ok: Vec<bool> = (0..len).map(|i| bands.values().all(|b| b.get(i).is_some_and(|v| v.is_finite()))).collect();
    let rule = rule.unwrap_or("none");
    let Some(qa) = qa.filter(|_| rule != "none") else {
        return Some(ok.iter().map(|&o| if o { 0 } else { 255 }).collect());
    };
    Some(
        (0..len)
            .map(|i| {
                let q = qa[i];
                let fin = q.is_finite() && q >= 0.0;
                let code = if fin { q as i64 } else { 255 };
                let f = match rule {
                    "fmask" => code as i32,
                    "landsat_c2" => {
                        let mut f = 0;
                        if code & (1 << 7) != 0 {
                            f = 1; // water
                        }
                        if code & (1 << 5) != 0 {
                            f = 3; // snow
                        }
                        if code & (1 << 4) != 0 {
                            f = 2; // cloud shadow
                        }
                        if code & ((1 << 1) | (1 << 3)) != 0 {
                            f = 4; // dilated cloud, cloud
                        }
                        if code & 1 != 0 {
                            f = 255; // fill
                        }
                        f
                    }
                    _ => {
                        if code != 0 {
                            0
                        } else {
                            4
                        }
                    }
                };
                if fin && ok[i] { f } else { 255 }
            })
            .collect(),
    )
}

/// A pixel's Fmask codes.
pub fn fmask(input: &PixelInput) -> Option<Vec<i32>> {
    fmask_codes(&input.bands, input.qa.as_deref(), input.qa_rule.as_deref())
}

pub fn missing_roles(input: &PixelInput, roles: &[&str]) -> Vec<String> {
    roles.iter().filter(|r| !input.bands.contains_key(**r)).map(|r| r.to_string()).collect()
}

/// The unit mode of the optical bands: the parameter, else from the median of their values (<= 2: reflectance
/// 0-1; >= 6500: Landsat Collection 2 digital numbers; else reflectance x 10000).
pub fn reflectance_units(p: &Params, optical: &[&[f64]]) -> String {
    let given = p.str("units");
    if given != "auto" {
        return given.to_string();
    }
    let mut v: Vec<f64> = optical.iter().flat_map(|b| b.iter().copied()).filter(|x| x.is_finite()).collect();
    if v.len() > 200_000 {
        let step = v.len() / 200_000;
        v = v.into_iter().step_by(step).collect();
    }
    if v.is_empty() {
        return "x10000".into();
    }
    let med = median(&v);
    if med <= 2.0 {
        "unit".into()
    } else if med >= 6500.0 {
        "landsat_c2".into()
    } else {
        "x10000".into()
    }
}

pub fn to_x10000(v: f64, mode: &str) -> f64 {
    match mode {
        "unit" => v * 10000.0,
        "landsat_c2" => (v * 2.75e-5 - 0.2) * 10000.0,
        _ => v,
    }
}

pub fn from_x10000(v: f64, mode: &str) -> f64 {
    match mode {
        "unit" => v / 10000.0,
        "landsat_c2" => (v / 10000.0 + 0.2) / 2.75e-5,
        _ => v,
    }
}

/// The mapped bands as reflectance x 10000 (per role, element by element as the bands are laid out),
/// observations the quality band rejects (Fmask cloud, shadow, snow or no observation) NaN when `clear_only`;
/// and the unit mode, from `units` of the optical bands (zeit_common.landsat_cube).
pub fn landsat_arrays(
    bands: &std::collections::BTreeMap<String, Vec<f64>>,
    fm: Option<&[i32]>,
    roles: &[&str],
    units: impl FnOnce(&[&[f64]]) -> String,
) -> Result<(Vec<Vec<f64>>, String), String> {
    let optical: Vec<&[f64]> = roles
        .iter()
        .map(|r| bands.get(*r).map(Vec::as_slice).ok_or_else(|| format!("KeyError: '{r}'")))
        .collect::<Result<_, _>>()?;
    let mode = units(&optical);
    let cube = optical
        .iter()
        .map(|b| {
            b.iter()
                .enumerate()
                .map(|(t, &v)| match fm {
                    Some(f) if f[t] > 1 => f64::NAN,
                    _ => to_x10000(v, &mode),
                })
                .collect()
        })
        .collect();
    Ok((cube, mode))
}

const NO_DATES: &str = "ValueError: needs real dates (none were found in the file names or band descriptions)";

/// A pixel's Landsat cube: its reflectance units from its own values.
pub fn landsat_cube(
    p: &Params,
    input: &PixelInput,
    roles: &[&str],
    clear_only: bool,
) -> Result<(Vec<Vec<f64>>, String), String> {
    if input.days.is_none() {
        return Err(NO_DATES.into());
    }
    let fm = if clear_only { fmask(input) } else { None };
    landsat_arrays(&input.bands, fm.as_deref(), roles, |o| reflectance_units(p, o))
}

/// A chunk's Landsat cube ([T][pixels] per role): its reflectance units decided once per job, from the first
/// chunk (zeit_common's ctx["cache"]).
pub fn landsat_chunk(
    p: &Params,
    chunk: &crate::job::Chunk,
    ctx: &crate::job::JobCtx,
    roles: &[&str],
    clear_only: bool,
) -> Result<(Vec<Vec<f64>>, String), String> {
    if ctx.input.days.is_none() {
        return Err(NO_DATES.into());
    }
    let fm =
        if clear_only { fmask_codes(&chunk.bands, chunk.qa.as_deref(), ctx.input.qa_rule.as_deref()) } else { None };
    landsat_arrays(&chunk.bands, fm.as_deref(), roles, |o| ctx.once("units", || reflectance_units(p, o)))
}

/// Janus shows a normalized difference (values in -1..1): an index such as the NDFI can be drawn on its axis.
pub fn shown_is_index(input: &PixelInput) -> bool {
    if input.shown.as_ref().and_then(|s| s.nd.as_ref()).is_some_and(|nd| nd.len() == 2) {
        return true;
    }
    let v: Vec<f64> = input.values.iter().copied().filter(|x| x.is_finite()).collect();
    !v.is_empty()
        && v.iter().copied().fold(f64::INFINITY, f64::min) >= -1.0
        && v.iter().copied().fold(f64::NEG_INFINITY, f64::max) <= 1.0
}

/// An overlay of one value per date of the series (NaN dates left out).
pub fn series_overlay(label: &str, input: &PixelInput, values: &[f64], kind: &str) -> Overlay {
    let (x, y): (Vec<f64>, Vec<Option<f64>>) =
        input.years.iter().zip(values).filter(|(_, v)| v.is_finite()).map(|(&x, &v)| (x, Some(v))).unzip();
    Overlay::new(kind, label, x, y)
}
