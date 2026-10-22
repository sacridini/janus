//! Tmask (zeit_bridge/tool_tmask.py): zeit.tmask's robust harmonic screening of the green and SWIR1 series, the
//! observations it flags (cloud or shadow) marked on the chart.

use crate::bands::{landsat_cube, missing_roles, series_overlay};
use crate::pixel::{Params, PixelInput, PixelResult, row};

const ROLES: [&str; 2] = ["green", "swir1"];

/// Python's date.toordinal() of 1970-01-01.
const UNIX_EPOCH_ORDINAL: i64 = 719163;

/// Why the tool cannot run on this series, if it cannot.
fn check(input: &PixelInput) -> Option<String> {
    let missing = missing_roles(input, &ROLES);
    if !missing.is_empty() {
        return Some(format!("map the bands first: {}", missing.join(", ")));
    }
    input.days.is_none().then(|| "needs real dates".into())
}

/// (flagged, valid) of each date: valid = both bands have a value the quality band keeps.
pub(crate) fn flags(p: &Params, input: &PixelInput) -> Result<(Vec<bool>, Vec<bool>), String> {
    let (cube, _) = landsat_cube(p, input, &ROLES, true)?;
    let (green, swir) = (&cube[0], &cube[1]);
    let t = green.len();
    let valid: Vec<bool> = (0..t).map(|i| cube.iter().all(|b| b[i].is_finite() && b[i] > 0.0)).collect();
    // zeit.tmask: the dates as Python ordinals, at most 5 valid observations left as they are
    let days: Vec<f64> = input.days.as_ref().unwrap().iter().map(|&d| (d + UNIX_EPOCH_ORDINAL) as f64).collect();
    let clear = zeit_core::tmask::tmask_batch(green, swir, &days, 1, 10000.0, 5, 0.04, 0.04, 1);
    Ok(((0..t).map(|i| valid[i] && clear[i] == 0).collect(), valid))
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if let Some(why) = check(input) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Tmask", why)] });
    }
    let (flagged, valid) = flags(p, input)?;
    let (n_valid, n_flag) = (valid.iter().filter(|&&v| v).count(), flagged.iter().filter(|&&f| f).count());
    let mut rows =
        vec![row("Valid observations", n_valid.to_string()), row("Flagged (cloud or shadow)", n_flag.to_string())];
    if n_valid <= 5 {
        rows.push(row("Tmask", "not screened (5 valid observations or fewer)"));
    }
    let shown: Vec<f64> = input.values.iter().zip(&flagged).map(|(&v, &f)| if f { v } else { f64::NAN }).collect();
    let overlays = if shown.iter().any(|v| v.is_finite()) {
        vec![series_overlay("Tmask: cloud or shadow", input, &shown, "markers")]
    } else {
        vec![]
    };
    Ok(PixelResult { overlays, rows })
}
