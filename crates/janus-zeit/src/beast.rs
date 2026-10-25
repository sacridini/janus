//! BEAST (zeit.beast): the Bayesian estimator of abrupt change, seasonal change and trend (Zhao et al. 2019), Zeit's
//! port of Rbeast's C code: the series' trend and season averaged over the models of a reversible-jump MCMC, with
//! the probability of a changepoint at each date and the most probable changepoints. A tool after the bridge: its
//! reference is zeit-core's beast module, which zeit.beast calls. Dated series are on decimal years (a harmonic
//! season of one year), aggregated to a regular grid as zeit.beast does (`deltat` 0: Rbeast's choice).

use zeit_core::beast::{self, Layout, Options, PrecType, TimeAxis};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::numeric::decimal_year;
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// Whether the season is fitted: "auto" when the series has dates and four or more a year.
fn season(p: &Params, input: &PixelInput) -> bool {
    match p.str("season") {
        "harmonic" => true,
        "none" => false,
        _ => input.days.is_some() && input.per_year() >= 4,
    }
}

/// The times of the series (decimal years of the dates, else its years as given) and whether they are dates.
fn times(input: &PixelInput) -> (Vec<f64>, bool) {
    match &input.days {
        Some(d) => (d.iter().map(|&x| decimal_year(x)).collect(), true),
        None => (input.years.clone(), false),
    }
}

/// zeit.beast's options of the tool's parameters.
pub(crate) fn options(p: &Params, input: &PixelInput) -> Result<Options, String> {
    let season = season(p, input);
    if season && input.days.is_none() {
        return Err("ValueError: a harmonic season needs the dates of the series (or set Season to none)".into());
    }
    let positive = |id: &str| -> Result<u32, String> {
        let v = p.int(id);
        if v < 1 { Err(format!("ValueError: {id} must be at least 1, got {v}")) } else { Ok(v as u32) }
    };
    Ok(Options {
        season,
        period: 1.0,
        start: f32::NAN,
        dt: f32::NAN,
        scp: (0, p.int("scp_max").max(0) as i32),
        sorder: (0, 5),
        tcp: (0, p.int("tcp_max").max(0) as i32),
        torder: (0, p.int("torder_max").clamp(0, 5) as i32),
        sseg_min: None,
        tseg_min: None,
        samples: positive("samples")?,
        burnin: p.int("burnin").max(0) as u32,
        chains: positive("chains")?,
        thin: positive("thin")?,
        seed: p.int("seed").max(0) as u64,
        prec_value: 1.5,
        prec_type: PrecType::ComponentWise,
        ci: true,
        max_missing_rate: 0.75,
    })
}

/// The regular grid of the series (`deltat` 0: as Rbeast chooses it).
pub(crate) fn axis(p: &Params, input: &PixelInput) -> Result<TimeAxis, String> {
    let (t, dates) = times(input);
    let dt = p.f64("deltat");
    TimeAxis::irregular(&t, f32::NAN, if dt > 0.0 { dt as f32 } else { f32::NAN }, 1.0, dates)
        .map_err(|e| format!("ValueError: {e}"))
}

/// BEAST of each of `pixels` series of `n_raw` values: the layout and the outputs, `layout.width` per series.
fn run(p: &Params, input: &PixelInput, values: &[f32], n_jobs: i32) -> Result<(Layout, Vec<f32>, TimeAxis), String> {
    let opts = options(p, input)?;
    let ax = axis(p, input)?;
    let (layout, out) =
        beast::batch(values, input.years.len(), &ax, &opts, n_jobs).map_err(|e| format!("ValueError: {e}"))?;
    Ok((layout, out, ax))
}

/// A field of one series' outputs.
fn field<'a>(layout: &Layout, row: &'a [f32], name: &str) -> Option<&'a [f32]> {
    let (_, off, shape) = layout.fields.iter().find(|f| f.0 == name)?;
    Some(&row[*off..*off + shape.iter().product::<usize>()])
}

fn finite(v: f32) -> Option<f64> {
    v.is_finite().then_some(f64::from(v))
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let values: Vec<f32> = input.values.iter().map(|&v| v as f32).collect();
    let (layout, out, ax) = run(p, input, &values, 1)?;
    let get = |name: &str| field(&layout, &out, name);
    let grid: Vec<f64> = (0..ax.n).map(|i| f64::from(ax.start) + f64::from(ax.dt) * i as f64).collect();
    let r2 = get("r2").map_or(f32::NAN, |v| v[0]);
    if r2.is_nan() {
        return Ok(PixelResult {
            overlays: vec![],
            rows: vec![row("BEAST", "not fitted: too many dates without a value (more than 75%)")],
        });
    }
    let trend = get("trend").unwrap_or_default();
    let mut rows = vec![
        row(
            "Grid",
            format!("{} dates, every {} years from {}", ax.n, g(f64::from(ax.dt), 4), g(f64::from(ax.start), 6)),
        ),
        row("R²", g(f64::from(r2), 4)),
        row("RMSE", g(f64::from(get("rmse").map_or(f32::NAN, |v| v[0])), 4)),
    ];
    let mut overlays = vec![];
    let line = |label: &str, y: Vec<Option<f64>>| Overlay::new("line", label, grid.clone(), y);
    if let Some(s) = get("season") {
        overlays.push(line("BEAST fit (trend + season)", trend.iter().zip(s).map(|(a, b)| finite(a + b)).collect()));
    }
    overlays.push(line("BEAST trend", trend.iter().map(|&v| finite(v)).collect()));
    if let Some(ci) = get("trend_ci") {
        // the core's interval of a component: the upper bound, then the lower
        for (k, label) in [(1, "Trend 95% interval (lower)"), (0, "Trend 95% interval (upper)")] {
            let mut o = line(label, ci[k * ax.n..(k + 1) * ax.n].iter().map(|&v| finite(v)).collect());
            o.style = Some("dashed".into());
            o.alpha = Some(0.6);
            overlays.push(o);
        }
    }
    // the changepoints, most probable first; drawn where more likely than not
    let mut drawn = vec![];
    for comp in ["trend", "season"] {
        let (Some(ncp), Some(cp), Some(pr)) =
            (get(&format!("{comp}_ncp")), get(&format!("{comp}_cp")), get(&format!("{comp}_cp_prob")))
        else {
            continue;
        };
        let title = if comp == "trend" { "Trend" } else { "Season" };
        rows.push(row(format!("{title} changepoints"), format!("{} on average", g(f64::from(ncp[0]), 3))));
        let abrupt = get(&format!("{comp}_cp_abrupt")).unwrap_or_default();
        for (k, (&c, &pp)) in cp.iter().zip(pr).enumerate().take_while(|(_, (c, _))| c.is_finite()).take(5) {
            let jump = abrupt.get(k).filter(|v| v.is_finite()).map(|&v| format!(", jump {}", g(f64::from(v), 3)));
            rows.push(row(
                format!("  {}", g(f64::from(c), 6)),
                format!("probability {:.0}%{}", 100.0 * pp, jump.unwrap_or_default()),
            ));
            if pp >= 0.5 && comp == "trend" {
                drawn.push(f64::from(c));
            }
        }
    }
    if let (Some(s), Some(pos)) = (get("trend_slope"), get("trend_slope_pos_prob")) {
        let last = ax.n - 1;
        rows.push(row(
            "Slope at the end",
            format!("{} per year, P(positive) {:.0}%", g(f64::from(s[last]), 3), 100.0 * pos[last]),
        ));
    }
    if !drawn.is_empty() {
        overlays.push(Overlay::new("vlines", "BEAST trend changepoints (p > 0.5)", drawn, vec![]));
    }
    Ok(PixelResult { overlays, rows })
}

pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let (t, n) = (chunk.t, chunk.pixels());
    // pixels x T, as zeit.beast's rows
    let mut values = Vec::with_capacity(n * t);
    for px in 0..n {
        values.extend((0..t).map(|i| chunk.stack[i * n + px] as f32));
    }
    let (layout, out, _) = run(p, &ctx.input, &values, ctx.n_jobs)?;
    let names = ["trend_ncp", "cp_date", "cp_prob", "cp_abrupt", "season_ncp", "r2"];
    let mut cols: Vec<Vec<f64>> = vec![Vec::with_capacity(n); names.len()];
    for px in 0..n {
        let r = &out[px * layout.width..(px + 1) * layout.width];
        let first = |name: &str| field(&layout, r, name).map_or(f64::NAN, |v| f64::from(v[0]));
        let values = [
            first("trend_ncp"),
            first("trend_cp"),
            first("trend_cp_prob"),
            first("trend_cp_abrupt"),
            first("season_ncp"),
            first("r2"),
        ];
        for (c, v) in cols.iter_mut().zip(values) {
            c.push(v);
        }
    }
    Ok(names.iter().map(|n| n.to_string()).zip(cols).collect())
}
