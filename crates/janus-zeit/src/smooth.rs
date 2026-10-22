//! Smoothing (zeit_bridge/tool_smooth.py): zeit.smooth's Whittaker curve (zeit-core) or Savitzky-Golay filter
//! (scipy's, in upstream.rs) over the chart.

use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};
use crate::upstream::{np_interp, np_sum, savgol};

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let vals = &input.values;
    let t = vals.len();
    let n = vals.iter().filter(|v| v.is_finite()).count();
    if n < 3 {
        return Ok(PixelResult {
            overlays: vec![],
            rows: vec![row("Smoothing", format!("not enough data ({n} valid)"))],
        });
    }
    if t < 3 {
        return Err(format!("ValueError: smooth needs at least 3 dates, got {t}"));
    }
    let gaps = t - n;
    // zeit.smooth's time axis: days since the first date (consecutive days without real dates)
    let days: Vec<f64> = match &input.days {
        Some(d) => d.iter().map(|&x| (x - d[0]) as f64).collect(),
        None => (0..t).map(|i| i as f64).collect(),
    };
    if days.windows(2).any(|w| w[1] <= w[0]) {
        return Err("ValueError: smooth needs increasing, distinct dates".into());
    }
    let method = p.str("method");
    let (sm, label) = if method == "savgol" {
        let wl = p.int("window");
        let wl =
            (if wl % 2 != 0 { wl } else { wl + 1 }).min(if !t.is_multiple_of(2) { t as i64 } else { t as i64 - 1 });
        let po = p.int("polyorder").min(wl - 1).max(0);
        if wl < 3 {
            return Ok(PixelResult {
                overlays: vec![],
                rows: vec![row("Smoothing", "series too short for Savitzky-Golay")],
            });
        }
        // the gaps interpolated linearly in time first (zeit/_smooth.py `_savgol_rows`)
        let row: Vec<f64> = if n == t {
            vals.clone()
        } else {
            let (xp, fp): (Vec<f64>, Vec<f64>) =
                days.iter().zip(vals).filter(|(_, v)| v.is_finite()).map(|(&d, &v)| (d, v)).unzip();
            days.iter().map(|&d| np_interp(d, &xp, &fp)).collect()
        };
        (savgol(&row, wl as usize, po as usize), format!("Smoothed (Savitzky-Golay {wl}/{po})"))
    } else {
        let lmbda = p.f64("lmbda");
        let z = zeit_core::whittaker::batch(vals, 1, &days, None, lmbda, 1).map_err(|e| format!("ValueError: {e}"))?;
        (z, format!("Smoothed (Whittaker λ={})", g(lmbda, 6)))
    };
    let res2: Vec<f64> = vals.iter().zip(&sm).filter(|(v, _)| v.is_finite()).map(|(v, s)| (v - s).powi(2)).collect();
    let rmse = (np_sum(&res2) / res2.len() as f64).sqrt();
    let mut rows = vec![
        row("Residual RMSE", if rmse.is_finite() { g(rmse, 4) } else { "-".into() }),
        row(
            format!("Gaps {}", if method == "whittaker" { "filled by the curve" } else { "interpolated" }),
            gaps.to_string(),
        ),
    ];
    // the first largest smoothed value
    let mut best: Option<usize> = None;
    for (k, &v) in sm.iter().enumerate() {
        if v.is_finite() && best.is_none_or(|b| v > sm[b]) {
            best = Some(k);
        }
    }
    if let Some(k) = best {
        rows.push(row("Smoothed max", format!("{} ({:.2})", g(sm[k], 4), input.years[k])));
    }
    let (x, y): (Vec<f64>, Vec<Option<f64>>) =
        input.years.iter().zip(&sm).filter(|(_, v)| v.is_finite()).map(|(&x, &v)| (x, Some(v))).unzip();
    Ok(PixelResult { overlays: vec![Overlay::new("line", &label, x, y)], rows })
}
