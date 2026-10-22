//! Surface reflectance for the multiband tools (CCDC, NDFI, CODED, Tmask): the bridge's band context (band_ctx,
//! to_fmask) and zeit_common's reflectance units and Landsat cube.

use crate::numeric::median;
use crate::pixel::{Overlay, Params, PixelInput};

pub const LANDSAT_ROLES: [&str; 6] = ["blue", "green", "red", "nir", "swir1", "swir2"];

/// Fmask codes of each date (0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 no observation) from the quality
/// band; without one, 0 where every band has a value and 255 elsewhere (the bridge's to_fmask).
pub fn fmask(input: &PixelInput) -> Option<Vec<i32>> {
    if input.bands.is_empty() {
        return None;
    }
    let t = input.values.len();
    let ok: Vec<bool> = (0..t).map(|i| input.bands.values().all(|b| b.get(i).is_some_and(|v| v.is_finite()))).collect();
    let rule = input.qa_rule.as_deref().unwrap_or("none");
    let Some(qa) = input.qa.as_ref().filter(|_| rule != "none") else {
        return Some(ok.iter().map(|&o| if o { 0 } else { 255 }).collect());
    };
    Some(
        (0..t)
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

/// The mapped bands as reflectance x 10000 (per role, each date), observations the quality band rejects (Fmask
/// cloud, shadow, snow or no observation) NaN when `clear_only`; and the unit mode (zeit_common.landsat_cube).
pub fn landsat_cube(
    p: &Params,
    input: &PixelInput,
    roles: &[&str],
    clear_only: bool,
) -> Result<(Vec<Vec<f64>>, String), String> {
    if input.days.is_none() {
        return Err("ValueError: needs real dates (none were found in the file names or band descriptions)".into());
    }
    let bands: Vec<&[f64]> = roles
        .iter()
        .map(|r| input.bands.get(*r).map(Vec::as_slice).ok_or_else(|| format!("KeyError: '{r}'")))
        .collect::<Result<_, _>>()?;
    let mode = reflectance_units(p, &bands);
    let fm = if clear_only { fmask(input) } else { None };
    let cube = bands
        .iter()
        .map(|b| {
            b.iter()
                .enumerate()
                .map(|(t, &v)| {
                    let v = to_x10000(v, &mode);
                    match &fm {
                        Some(f) if f[t] > 1 => f64::NAN,
                        _ => v,
                    }
                })
                .collect()
        })
        .collect();
    Ok((cube, mode))
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
