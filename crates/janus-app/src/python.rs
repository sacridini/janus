//! "Copy Python" (src/app_python.cpp): code that reproduces outside Janus the series on the chart (the cursor's,
//! a pin's or the ROI's) as Janus reads it (band or normalized difference, quality mask, nodata, scale/offset), its
//! statistics and the Zeit tool fitted on the chart, with the same parameters. Needs numpy and rasterio (xarray
//! and zeit-cdts for the tool).

use std::fmt::Write as _;

use janus_cube::QaRule;

use crate::app::State;

/// A Python string literal (paths with backslashes, names with quotes).
fn py_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out + "\""
}

fn num(v: f64) -> String {
    if v.is_nan() { "float(\"nan\")".into() } else { janus_cube::multidim::format_g(v, 10) }
}

fn py_band(b: i32) -> String {
    if b > 0 { b.to_string() } else { "None".into() }
}

const QA_FUNCTIONS: &str = r#"
def usable(q, rule):
    """Observations Janus keeps, from the quality band's codes (nodata: none)."""
    ok = np.isfinite(q) & (q >= 0)
    c = np.where(ok, q, 0).astype(np.int64)
    if rule == "fmask":          # 0 clear land, 1 water
        return ok & ((c == 0) | (c == 1))
    if rule == "landsat_c2":     # QA_PIXEL bits 0 fill, 1 dilated cloud, 3 cloud, 4 shadow, 5 snow
        return ok & ((c & 0x3B) == 0)
    return ok & (c != 0)         # a 0/1 mask
"#;

const READ_FUNCTIONS: &str = r#"

def read(path, band, window, out_shape=None, raw=False):
    """A band over a window: nodata -> NaN, the band's scale and offset applied."""
    with rasterio.open(path) as src:
        kw = {"out_shape": out_shape, "resampling": Resampling.nearest} if out_shape else {}
        a = src.read(band, window=window, masked=True, **kw).astype("float64")
        if not raw:
            a = a * src.scales[band - 1] + src.offsets[band - 1]
        return np.ma.filled(a, np.nan)


def shown(layer, window, out_shape=None):
    """What the series shows at a date: the band, or the normalized difference, masked by the quality band."""
    path, band, nd_band, qa_band = layer
    v = read(path, band, window, out_shape)
    if nd_band:
        b = read(path, nd_band, window, out_shape)
        with np.errstate(invalid="ignore", divide="ignore"):
            v = np.where(v + b != 0, (v - b) / (v + b), np.nan)
    if qa_band:
        v = np.where(usable(read(path, qa_band, window, out_shape, raw=True), qa_rule), v, np.nan)
    return v
"#;

/// Which series: the cursor's, a pin's, the ROI's.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Cursor,
    Pin(u32),
    Roi,
}

impl State {
    /// The code for a series of the chart (None if it is not there).
    pub(crate) fn python_snippet(&mut self, which: Which) -> Option<String> {
        let info = self.session.as_ref()?.info.clone();
        let t_n = info.t();
        let view = match which {
            Which::Cursor => self.series.hover.clone(),
            Which::Pin(id) => self.series.pins.iter().find(|p| p.id == id).cloned(),
            Which::Roi => None,
        };
        let roi = (which == Which::Roi && self.series.roi.complete()).then(|| self.series.roi.data.clone()).flatten();
        if view.is_none() && roi.is_none() {
            return None;
        }
        let name = self.layers.get(self.active).map_or("the series".to_string(), |l| l.name.clone());
        let what = match which {
            Which::Roi if self.series.roi_poly.is_empty() => "the ROI (a rectangle)".to_string(),
            Which::Roi => "the ROI (a polygon)".to_string(),
            Which::Cursor => "the cursor's pixel".to_string(),
            Which::Pin(id) => format!("pin {id}"),
        };
        // the tool on the chart: its calls
        let fit_key = match which {
            Which::Cursor => crate::zeit::Which::Hover,
            Which::Pin(id) => crate::zeit::Which::Pin(id),
            Which::Roi => crate::zeit::Which::Roi,
        };
        // the tools fitted on this series, in the chart's order
        let fitted: Vec<String> = match (&view, which) {
            (_, Which::Roi) => self.roi_fits(),
            (Some(v), _) => self.zeit.fits_of(info.id, fit_key, v.x, v.y),
            _ => vec![],
        }
        .into_iter()
        .filter(|f| f.result.is_some_and(|r| r.is_ok()))
        .map(|f| f.tool.id.clone())
        .collect();
        let tool_calls: Vec<(String, String, Vec<&str>)> = fitted
            .into_iter()
            .filter_map(|id| {
                let tool = janus_zeit::manifest().tool(&id)?;
                let params = janus_zeit::Params::with_defaults(&id, self.zeit.params(tool)).unwrap_or_default();
                let values = view.as_ref().map_or_else(|| self.series.roi.mean.clone(), |v| v.values.clone());
                let roles: std::collections::BTreeMap<String, Vec<f64>> =
                    self.zeit.roles.keys().map(|r| (r.clone(), vec![])).collect();
                let input = janus_zeit::PixelInput {
                    years: (0..t_n).map(|t| info.decimal_year(t)).collect(),
                    days: info.time_is_date.then(|| (0..t_n).map(|t| info.unix_day(t)).collect()),
                    values: values.iter().map(|&v| f64::from(v)).collect(),
                    bands: roles,
                    ..Default::default()
                };
                let (code, inputs) = janus_zeit::python::calls(&id, &params, &input)?;
                Some((tool.name.clone(), code, inputs))
            })
            .collect();

        let mut o = String::new();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
        let (y, m, d) = janus_zeit::numeric::civil(now.div_euclid(86400));
        let _ = write!(
            o,
            "\"\"\"Janus {} ({y:04}-{m:02}-{d:02}): {what} of {name},\nreproduced outside Janus. Needs: pip install numpy rasterio",
            env!("CARGO_PKG_VERSION")
        );
        if !tool_calls.is_empty() {
            o += " xarray zeit-cdts";
        }
        o += "\n\"\"\"\nimport numpy as np\nimport rasterio\nfrom rasterio.windows import Window\n";
        if roi.is_some() && !self.series.roi_poly.is_empty() {
            o += "from rasterio.features import rasterize\n";
        }
        o += "from rasterio.enums import Resampling\n";

        // the series: each date's file and band
        let sel = info.sel;
        let selection = info.selection_text();
        let _ = writeln!(
            o,
            "\n# {}{}",
            info.description,
            if selection.is_empty() { String::new() } else { format!(": {selection}") }
        );
        o += "# Each date: (file, band, band B of a normalized difference (A - B) / (A + B), quality band).\nlayers = [\n";
        let qa_on = sel.qa_band > 0 && sel.qa_rule != QaRule::None;
        for l in &info.layers {
            let base = l.band_base;
            let _ = writeln!(
                o,
                "    ({}, {}, {}, {}),",
                py_str(&l.path),
                l.band,
                py_band(if sel.nd_band > 0 { sel.nd_band + base } else { 0 }),
                py_band(if qa_on { sel.qa_band + base } else { 0 })
            );
        }
        o += "]\n";
        if info.time_is_date {
            let dates: Vec<String> =
                info.layers.iter().map(|l| format!("\"{}\"", &l.label[..l.label.len().min(10)])).collect();
            let _ = writeln!(o, "dates = np.array([{}], dtype=\"datetime64[D]\")", dates.join(", "));
        }
        if info.spectral {
            let w: Vec<String> = info.layers.iter().map(|l| num(l.time)).collect();
            let _ = writeln!(o, "wavelengths_nm = np.array([{}])", w.join(", "));
        }
        o += "# Time in years since the first date, as Janus fits trends (365.2425-day years).\n";
        let yrs: Vec<String> = (0..t_n).map(|t| num(info.years_from_start(t))).collect();
        let _ = writeln!(o, "years = np.array([{}])", yrs.join(", "));
        let _ = writeln!(o, "qa_rule = {}", py_str(sel.qa_rule.id()));
        o += QA_FUNCTIONS;
        o += READ_FUNCTIONS;
        if let Some(r) = &roi {
            let (w, h) = (r.x1 - r.x0, r.y1 - r.y0);
            let _ =
                write!(o, "\n# The ROI: the window x {}..{}, y {}..{} of the files' pixels", r.x0, r.x1, r.y0, r.y1);
            if r.sampled {
                let _ = write!(
                    o,
                    ",\n# read at {} x {} (Janus subsamples ROIs above 4 Mpx, nearest neighbour)",
                    r.bw, r.bh
                );
            }
            let _ = writeln!(o, ".\nwindow = Window({}, {}, {w}, {h})\nshape = ({}, {})", r.x0, r.y0, r.bh, r.bw);
            if self.series.roi_poly.is_empty() {
                o += "inside = np.ones(shape, dtype=bool)\n";
            } else {
                // the polygon in the window's cells: pixels whose centres are inside count
                let (kx, ky) = (r.bw as f64 / w as f64, r.bh as f64 / h as f64);
                o += "# Its polygon (rings in the window's cells, even-odd: holes are rings inside others);\n\
                      # a cell counts when its centre is inside.\nrings = [\n";
                for ring in &self.series.roi_poly {
                    let pts: Vec<String> = ring
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|v| format!("({}, {})", num((v[0] - r.x0 as f64) * kx), num((v[1] - r.y0 as f64) * ky)))
                        .collect();
                    let _ = writeln!(o, "    [{}],", pts.join(", "));
                }
                o += "]\ninside = np.zeros(shape, dtype=bool)\n\
                      for ring in rings:  # each ring flips what it covers (even-odd)\n    \
                      inside ^= rasterize([{\"type\": \"Polygon\", \"coordinates\": [ring + ring[:1]]}], out_shape=shape,\n                        \
                      fill=0, default_value=1, dtype=\"uint8\").astype(bool)\n";
            }
            o += r#"
mean, p10, p90 = (np.full(len(layers), np.nan) for _ in range(3))
for t, layer in enumerate(layers):
    v = shown(layer, window, shape)[inside]
    v = v[np.isfinite(v)]
    if v.size:
        mean[t] = v.mean()
        p10[t], p90[t] = np.percentile(v, [10, 90])  # numpy's percentiles: Janus' may differ slightly
values = mean
"#;
        } else if let Some(v) = &view {
            o += "\n# The pixel (column, row of the files)";
            if let Some((gx, gy)) = info.pixel_to_geo(v.x as f64 + 0.5, v.y as f64 + 0.5) {
                let crs = if info.crs_authority.is_empty() { &info.crs_name } else { &info.crs_authority };
                let _ = write!(o, ", its centre at x {}, y {} ({crs})", num(gx), num(gy));
            }
            let _ = writeln!(
                o,
                ".\ncol, row = {}, {}\nvalues = np.array([shown(layer, Window(col, row, 1, 1))[0, 0] for layer in layers])",
                v.x, v.y
            );
        }
        o += r#"
ok = np.isfinite(values)
y, t = values[ok], years[ok]
print("n", ok.sum(), "mean", y.mean(), "std", y.std(ddof=1) if y.size > 1 else np.nan)
if y.size >= 2:
    slope, intercept = np.polyfit(t, y, 1)                  # OLS, per year
    i, j = np.triu_indices(y.size, 1)
    keep = t[j] != t[i]
    sen = np.median((y[j] - y[i])[keep] / (t[j] - t[i])[keep])  # Sen's slope, per year
    print("OLS slope", slope, "Sen's slope", sen)
"#;
        if !tool_calls.is_empty() {
            let names: Vec<&str> = tool_calls.iter().map(|c| c.0.as_str()).collect();
            let inputs: Vec<&str> = tool_calls.iter().flat_map(|c| c.2.iter().copied()).collect();
            let _ = writeln!(
                o,
                "\n# {}, fitted on the chart with these parameters: the same Zeit calls Janus makes\n\
                 # (its model is drawn from their results).\n{}import xarray as xr\nimport zeit\n",
                names.join(", "),
                if inputs.contains(&"bands_all") || inputs.contains(&"spectrum") {
                    "import pandas as pd\n"
                } else {
                    ""
                }
            );
            let time = if info.time_is_date { "dates.astype(\"datetime64[ns]\")" } else { "np.arange(len(values))" };
            if inputs.contains(&"series") {
                let _ = writeln!(
                    o,
                    "series = xr.DataArray(values[:, None, None], dims=(\"time\", \"y\", \"x\"), coords={{\"time\": {time}}})"
                );
            }
            if inputs.contains(&"bands_cube") {
                let roles: Vec<String> = self.zeit.roles.iter().map(|(r, b)| format!("{}: {b}", py_str(r))).collect();
                let _ = writeln!(
                    o,
                    "# The bands by role (bands of each date's file), as reflectance x 10000 (Zeit's convention:\n\
                     # 0-1 reflectance x 10000; Landsat C2 L2 numbers x 0.0000275 - 0.2, x 10000; set `units`).\n\
                     roles = {{{}}}",
                    roles.join(", ")
                );
                o += "order = [r for r in (\"blue\", \"green\", \"red\", \"nir\", \"swir1\", \"swir2\", \"thermal\") if r in roles]\n";
                o += "raw = {r: np.array([read(layer[0], roles[r], Window(col, row, 1, 1))[0, 0] for layer in layers])\n       for r in order}\n";
                o += "optical = np.concatenate([raw[r] for r in order if r != \"thermal\"])\nmed = np.nanmedian(optical)\n\
                      units = \"unit\" if med <= 2 else \"landsat_c2\" if med >= 6500 else \"x10000\"\n\
                      to_x10000 = {\"unit\": lambda v: v * 10000, \"landsat_c2\": lambda v: (v * 2.75e-5 - 0.2) * 10000,\n             \"x10000\": lambda v: v}[units]\n\
                      cube = np.stack([to_x10000(raw[r]) for r in order])  # (band, time); the thermal band: deg C x 100 in Zeit\n";
                let _ = writeln!(
                    o,
                    "bands_cube = xr.DataArray(cube.T[:, :, None, None], dims=(\"time\", \"band\", \"y\", \"x\"),\n                          coords={{\"time\": {time}, \"band\": order}})"
                );
                o += "# Fmask codes (0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 no observation) from the quality band.\ncomplete = np.all(np.isfinite(cube), axis=0)\n";
                if qa_on {
                    o += "q = np.array([read(path, qa, Window(col, row, 1, 1), raw=True)[0, 0] for path, _, _, qa in layers])\n\
                          c = np.where(np.isfinite(q) & (q >= 0), q, 255).astype(np.int64)\n\
                          if qa_rule == \"fmask\":\n    f = c.astype(np.int32)\n\
                          elif qa_rule == \"landsat_c2\":\n    f = np.zeros(c.shape, np.int32)\n    \
                          f[(c & 128) != 0] = 1; f[(c & 32) != 0] = 3; f[(c & 16) != 0] = 2; f[(c & 10) != 0] = 4; f[(c & 1) != 0] = 255\n\
                          else:\n    f = np.where(c != 0, 0, 4).astype(np.int32)\nfmask_codes = np.where(complete, f, 255)\n";
                } else {
                    o += "fmask_codes = np.where(complete, 0, 255)  # no quality band: every complete observation is clear\n";
                }
                o += "fmask = fmask_codes.astype(np.int32)[:, None, None]\n";
            }
            if inputs.contains(&"bands_all") {
                let bases: Vec<String> = info.layers.iter().map(|l| l.band_base.to_string()).collect();
                let _ = writeln!(
                    o,
                    "# Every band of each date (b001, b002...: the bands of its file), left out where the shown series is.\n\
                     n_bands = {}\nbases = [{}]  # each date's first band in its file, less 1\n\
                     cube = np.array([[read(layer[0], base + b, Window(col, row, 1, 1))[0, 0] for b in range(1, n_bands + 1)]\n                 \
                     for layer, base in zip(layers, bases)])\n\
                     cube[~np.isfinite(values)] = np.nan\n\
                     bands_all = xr.DataArray(cube[:, :, None, None], dims=(\"time\", \"band\", \"y\", \"x\"),\n                         \
                     coords={{\"time\": {time}, \"band\": [f\"b{{b:03d}}\" for b in range(1, n_bands + 1)]}})",
                    info.bands_per_date,
                    bases.join(", ")
                );
            }
            if inputs.contains(&"spectrum") {
                o += "# The spectrum: one value per wavelength (b001 the first).\n\
                      spectrum = xr.DataArray(values[:, None, None], dims=(\"band\", \"y\", \"x\"),\n                        \
                      coords={\"band\": [f\"b{i + 1:03d}\" for i in range(len(values))]})\n";
            }
            for (name, code, _) in &tool_calls {
                if tool_calls.len() > 1 {
                    let _ = writeln!(o, "\n# {name}");
                }
                o += code;
                o += "\nprint(result1)\n";
            }
        }
        Some(o)
    }

    /// The chart's "Copy Python" menu.
    pub(crate) fn copy_python_ui(&mut self, ui: &mut egui::Ui) {
        let any = self.series.hover.is_some() || !self.series.pins.is_empty() || self.series.roi.complete();
        let mut chosen = None;
        const HELP: &str = "Python (numpy, rasterio; xarray and zeit for a tool) that reads the same series\nfrom the \
                            files, computes its statistics and, with a Zeit tool on the chart,\nmakes the same Zeit \
                            calls with the same parameters";
        // the button itself in the toolbar's row (in a child Ui of its own, it could neither move to the next row
        // nor fit the end of this one)
        if !any {
            ui.add_enabled(false, egui::Button::new("Copy Python")).on_disabled_hover_text(HELP);
            return;
        }
        ui.menu_button("Copy Python", |ui| {
            if let Some(h) = &self.series.hover
                && ui.button(format!("Cursor ({}, {})", h.x, h.y)).clicked()
            {
                chosen = Some(Which::Cursor);
            }
            for p in &self.series.pins {
                if ui.button(format!("Pin {} ({}, {})", p.id, p.x, p.y)).clicked() {
                    chosen = Some(Which::Pin(p.id));
                }
            }
            if self.series.roi.complete() && ui.button("ROI (rectangle)").clicked() {
                chosen = Some(Which::Roi);
            }
            let tools: Vec<String> = self.zeit.chart_tools().iter().map(|(t, _)| t.name.clone()).collect();
            if !tools.is_empty() {
                ui.weak(format!("with {}", tools.join(", ")));
            }
        })
        .response
        .on_hover_text(HELP);
        if let Some(w) = chosen
            && let Some(code) = self.python_snippet(w)
        {
            ui.ctx().copy_text(code);
        }
    }
}
