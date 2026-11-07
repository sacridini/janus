//! Tasseled Cap (FORCE) brightness, greenness, wetness and disturbance: zeit.tasseled_cap, a fixed linear map of
//! the six reflective bands (blue, green, red, NIR, SWIR1, SWIR2) of each date, computed by zeit-core's
//! `tasseled_batch` (FORCE 3.11's float32 arithmetic, the six products added in the band order). The bands are
//! reflectance x 10000 as the other multiband tools read them (`units`; the observations the quality band rejects
//! left out), so the components are in that scale, as Zeit's on the Python snippet's `bands_cube`.
//!
//! The matrices are Zeit's (zeit/_tasseled.py). Crist (1985) is the one FORCE applies to every sensor; the others
//! are per sensor, and `coefficients = "sensor"` takes each date's from its sensor (`sensors`).

use zeit_core::tasseled::{BANDS, tasseled_batch};

use crate::bands::{LANDSAT_ROLES, landsat_chunk, missing_roles};
use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::{Params, PixelInput, PixelResult, g, row};
use crate::sensors::date_text;

/// The components of the published matrices, in the order of the matrices' rows, and the one derived from them.
const COMPONENTS: [&str; 3] = ["brightness", "greenness", "wetness"];
const DISTURBANCE: &str = "disturbance";

/// Each table: component -> coefficients of blue, green, red, NIR, SWIR1, SWIR2.
type Table = [[f64; BANDS]; 3];

/// Crist (1985, RSE 17:301-306), "A TM Tasseled Cap equivalent transformation for reflectance factor data",
/// Landsat 4-5 TM surface reflectance. It is the matrix FORCE applies to every sensor.
const CRIST_1985: Table = [
    [0.2043, 0.4158, 0.5524, 0.5741, 0.3124, 0.2303],
    [-0.1603, -0.2819, -0.4934, 0.7940, -0.0002, -0.1446],
    [0.0315, 0.2021, 0.3102, 0.1594, -0.6806, -0.6109],
];
/// Huang, Wylie, Yang, Homer and Zylstra (2002), IJRS 23(8):1741-1748, Landsat 7 ETM+ at-satellite reflectance.
const HUANG_2002: Table = [
    [0.3561, 0.3972, 0.3904, 0.6966, 0.2286, 0.1596],
    [-0.3344, -0.3544, -0.4556, 0.6966, -0.0242, -0.2630],
    [0.2626, 0.2141, 0.0926, 0.0656, -0.7629, -0.5388],
];
/// Baig, Zhang, Shuai and Tong (2014), Remote Sensing Letters 5(5):423-431, Landsat 8 OLI top-of-atmosphere
/// reflectance (used for Landsat 9's OLI-2 too).
const BAIG_2014: Table = [
    [0.3029, 0.2786, 0.4733, 0.5599, 0.5080, 0.1872],
    [-0.2941, -0.2430, -0.5424, 0.7276, 0.0713, -0.1608],
    [0.1511, 0.1973, 0.3283, 0.3407, -0.7117, -0.4559],
];

fn table(name: &str) -> &'static Table {
    match name {
        "huang2002" => &HUANG_2002,
        "baig2014" => &BAIG_2014,
        _ => &CRIST_1985,
    }
}

/// `coefficients = "sensor"`: the table of each sensor (FORCE's names, `SEN2` any Sentinel-2 unit). Sentinel-2
/// takes Crist (1985), as FORCE does: the matrix published for MSI (Shi and Xu 2019) is for all 13 bands of the L1C
/// top-of-atmosphere product, B1, B9 and B10 included, which a surface reflectance series does not have.
fn sensor_table(sensor: &str) -> Option<&'static str> {
    Some(match sensor {
        "LND04" | "LND05" | "SEN2" => "crist1985",
        "LND07" => "huang2002",
        "LND08" | "LND09" => "baig2014",
        _ => return None,
    })
}

fn citation(table: &str) -> &'static str {
    match table {
        "huang2002" => "Huang et al. (2002), IJRS 23:1741-1748",
        "baig2014" => "Baig et al. (2014), Remote Sensing Letters 5:423-431",
        _ => "Crist (1985), RSE 17:301-306",
    }
}

/// What the components asked for are made of, as zeit.tasseled_cap works them out.
struct Plan {
    /// the components, in the order asked
    names: Vec<String>,
    /// the matrix rows: the published components asked for, then those of brightness, greenness and wetness the
    /// disturbance needs
    rows_needed: Vec<&'static str>,
    /// how many of `rows_needed` are returned
    published: usize,
    /// the rows of brightness, greenness and wetness, when the disturbance is asked for
    disturbance: Option<[usize; 3]>,
}

fn plan(p: &Params) -> Result<Plan, String> {
    let names: Vec<String> =
        p.0.get("components")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
    if names.is_empty() {
        return Err(
            "ValueError: components is empty: choose at least one of brightness, greenness, wetness, disturbance"
                .into(),
        );
    }
    for (i, n) in names.iter().enumerate() {
        if !COMPONENTS.contains(&n.as_str()) && n != DISTURBANCE {
            return Err(format!(
                "ValueError: unknown component {n:?}: use brightness, greenness, wetness or disturbance"
            ));
        }
        if names[..i].contains(n) {
            return Err(format!("ValueError: components names a component twice: {names:?}"));
        }
    }
    let published: Vec<&'static str> =
        names.iter().filter_map(|n| COMPONENTS.iter().copied().find(|c| *c == n.as_str())).collect();
    let disturbance = names.iter().any(|n| n == DISTURBANCE);
    let mut rows_needed = published.clone();
    if disturbance {
        rows_needed.extend(COMPONENTS.iter().copied().filter(|c| !published.contains(c)));
    }
    let base = disturbance.then(|| COMPONENTS.map(|c| rows_needed.iter().position(|r| *r == c).unwrap_or(0)));
    Ok(Plan { names, rows_needed, published: published.len(), disturbance: base })
}

/// The matrices of every date, (time, row, 6) float32 (the coefficients rounded to float32 as numpy does), and the
/// table each date got.
fn matrices(p: &Params, plan: &Plan, t: usize, input: &PixelInput) -> Result<(Vec<f32>, Vec<&'static str>), String> {
    let coefficients = p.str("coefficients");
    let mut tables: Vec<&'static str> = Vec::with_capacity(t);
    if coefficients == "sensor" {
        if input.sensors.len() != t {
            return Err("ValueError: tasseled_cap needs each date's sensor for coefficients=\"sensor\" (Janus reads it from \
                 the scene a date came from: its STAC platform or its file's name): choose a matrix for every date instead"
                .into());
        }
        for (k, s) in input.sensors.iter().enumerate() {
            let table = sensor_table(s).ok_or_else(|| {
                format!(
                    "ValueError: no published Tasseled Cap for the sensor {:?} (date {}): those of Landsat 4, 5, 7, 8, 9 \
                     and Sentinel-2 are known; choose a matrix for every date instead",
                    s,
                    date_text(input, k)
                )
            })?;
            tables.push(table);
        }
    } else {
        let name = match coefficients {
            "crist1985" | "huang2002" | "baig2014" => coefficients,
            c => {
                return Err(format!("ValueError: coefficients {c:?}: use crist1985, sensor, huang2002 or baig2014"));
            }
        };
        tables = vec![table_name(name); t];
    }
    let nm = plan.rows_needed.len();
    let mut m = Vec::with_capacity(t * nm * BANDS);
    for name in &tables {
        let tab = table(name);
        for r in &plan.rows_needed {
            let k = COMPONENTS.iter().position(|c| c == r).unwrap_or(0);
            m.extend(tab[k].iter().map(|&c| c as f32));
        }
    }
    Ok((m, tables))
}

fn table_name(name: &str) -> &'static str {
    match name {
        "huang2002" => "huang2002",
        "baig2014" => "baig2014",
        _ => "crist1985",
    }
}

/// What `compute` gives: the plan, the components and the table each date got.
type Computed = (Plan, Vec<Vec<f64>>, Vec<&'static str>);

/// The components of a chunk ([T][pixels] each, in the order asked, NaN where a band is missing) and the table
/// each date got.
fn compute(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<Computed, String> {
    let plan = plan(p)?;
    let (t, n) = (chunk.t, chunk.pixels());
    let (cube, _) = landsat_chunk(p, chunk, ctx, &LANDSAT_ROLES, true)?;
    let (m, tables) = matrices(p, &plan, t, &ctx.input)?;
    // [role][time][pixels] -> (time, band, pixels)
    let mut x = Vec::with_capacity(t * BANDS * n);
    for k in 0..t {
        for band in &cube {
            x.extend_from_slice(&band[k * n..(k + 1) * n]);
        }
    }
    let ncomp = plan.published + usize::from(plan.disturbance.is_some());
    let rows: Vec<usize> = (0..plan.published).collect();
    let mut out = vec![0.0f32; t * ncomp * n];
    tasseled_batch(
        &x,
        BANDS,
        n,
        [0, 1, 2, 3, 4, 5],
        &m,
        plan.rows_needed.len(),
        &rows,
        plan.disturbance,
        &[],
        &mut out,
        ctx.n_jobs,
    );
    // the block has the disturbance last; it may have been asked for earlier
    let mut block_names: Vec<&str> = plan.rows_needed[..plan.published].to_vec();
    if plan.disturbance.is_some() {
        block_names.push(DISTURBANCE);
    }
    let comps = plan
        .names
        .iter()
        .map(|name| {
            let c = block_names.iter().position(|b| b == name).unwrap_or(0);
            (0..t)
                .flat_map(|k| out[(k * ncomp + c) * n..(k * ncomp + c + 1) * n].iter().map(|&v| f64::from(v)))
                .collect()
        })
        .collect();
    Ok((plan, comps, tables))
}

/// Why the tool cannot run on this series, if it cannot.
fn check(input: &PixelInput) -> Option<String> {
    let missing = missing_roles(input, &LANDSAT_ROLES);
    if !missing.is_empty() {
        return Some(format!("map the bands first: {}", missing.join(", ")));
    }
    input.days.is_none().then(|| "needs real dates".into())
}

fn label(name: &str) -> &'static str {
    match name {
        "brightness" => "Brightness",
        "greenness" => "Greenness",
        "wetness" => "Wetness",
        _ => "Disturbance (B - G - W)",
    }
}

/// How many dates each table was applied to, as "Crist (1985) (12 dates), ...".
fn describe(tables: &[&str]) -> String {
    let mut seen: Vec<(&str, usize)> = vec![];
    for t in tables {
        match seen.iter_mut().find(|(s, _)| s == t) {
            Some((_, k)) => *k += 1,
            None => seen.push((t, 1)),
        }
    }
    seen.iter()
        .map(|(t, k)| format!("{} ({k} date{})", citation(t), if *k == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if let Some(why) = check(input) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Tasseled Cap", why)] });
    }
    let (plan, comps, tables) = compute(p, &input.as_chunk(), &JobCtx::for_pixel(input))?;
    let valid = comps[0].iter().filter(|v| v.is_finite()).count();
    let mut rows =
        vec![row("Valid observations", format!("{valid} of {}", input.values.len())), row("Matrix", describe(&tables))];
    let mut overlays = vec![];
    for (name, values) in plan.names.iter().zip(&comps) {
        let ok: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
        if ok.is_empty() {
            rows.push(row(format!("Mean {}", label(name).to_lowercase()), "no clear observation"));
            continue;
        }
        rows.push(row(format!("Mean {}", label(name).to_lowercase()), g(ok.iter().sum::<f64>() / ok.len() as f64, 5)));
        overlays.push(crate::bands::series_overlay(label(name), input, values, "line"));
    }
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: one series per component ([T][pixels]), under the id `tc_{component}`.
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let with_bands = PixelInput { bands: chunk.bands.clone(), ..ctx.input.clone() };
    if let Some(why) = check(&with_bands) {
        return Err(format!("ValueError: Tasseled Cap: {why}"));
    }
    let (plan, comps, _) = compute(p, chunk, ctx)?;
    let mut res = ChunkOut::new();
    for (name, values) in plan.names.into_iter().zip(comps) {
        res.insert(format!("tc_{name}"), values);
    }
    Ok(res)
}
