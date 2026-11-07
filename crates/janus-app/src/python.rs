//! "Copy Python" (src/app_python.cpp): code that reproduces outside Janus the series on the chart (the cursor's,
//! a pin's or the ROI's) as Janus reads it (band or normalized difference, quality mask, nodata, scale/offset), its
//! statistics and the Zeit tool fitted on the chart, with the same parameters. Needs numpy and rasterio (xarray
//! and zeit-cdts for the tool).

use std::fmt::Write as _;

use janus_cube::QaRule;

use crate::app::State;

/// A Python string literal (paths with backslashes, names with quotes).
pub(crate) fn py_str(s: &str) -> String {
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
    if rule == "scl":            # Sentinel-2 SCL: 4 vegetation, 5 bare soil, 6 water
        return ok & (c >= 4) & (c <= 6)
    if rule == "hls_fmask":      # HLS Fmask bits 1 cloud, 2 adjacent, 3 shadow, 4 snow; 255 fill
        return ok & (c != 255) & ((c & 0x1E) == 0)
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
"#;

const SHOWN_FUNCTION: &str = r#"

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

/// The series as the code reads it: each date's file and band (or the bands of its expression), its dates, `years`,
/// the quality rule and the functions `read` and `shown` (a date over a window).
pub(crate) fn series_code(info: &janus_cube::CubeInfo) -> String {
    let mut o = String::new();
    let t_n = info.t();
    let sel = info.sel;
    let selection = info.selection_text();
    let _ = writeln!(
        o,
        "\n# {}{}",
        info.description,
        if selection.is_empty() { String::new() } else { format!(": {selection}") }
    );
    let qa_on = sel.qa_band > 0 && sel.qa_rule != QaRule::None;
    if let Some(e) = &info.expr {
        o += "# Each date: (file, the bands its expression reads, quality band, its date's year, month, day,\n\
              # day of the year, decimal year and place).\nlayers = [\n";
        for (t, l) in info.layers.iter().enumerate() {
            let bands: Vec<String> = e.bands.iter().map(|b| (b + l.band_base).to_string()).collect();
            let vars: Vec<String> =
                janus_cube::calc::plan::date_vars(info, t).0.iter().map(|&x| num(f64::from(x))).collect();
            let _ = writeln!(
                o,
                "    ({}, [{}], {}, ({})),",
                py_str(&crate::provenance::absolute(&l.path)),
                bands.join(", "),
                py_band(if qa_on { sel.qa_band + l.band_base } else { 0 }),
                vars.join(", ")
            );
        }
    } else {
        o += "# Each date: (file, band, band B of a normalized difference (A - B) / (A + B), quality band).\n\
              layers = [\n";
    }
    for l in info.layers.iter().filter(|_| info.expr.is_none()) {
        let base = l.band_base;
        let _ = writeln!(
            o,
            "    ({}, {}, {}, {}),",
            py_str(&crate::provenance::absolute(&l.path)),
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
    if let Some(e) = &info.expr {
        o += janus_cube::calc::python::NUMPY_PRELUDE;
        let (stmts, r) = janus_cube::calc::python::numpy(&e.program, &|k| format!("b[{k}]"), "    ");
        let _ = write!(
            o,
            "\n\ndef expression(b, year, month, day, doy, decyear, t):\n    \
             \"\"\"{}: the bands read (in the order of the layers' lists) to the value shown.\"\"\"\n\
             {stmts}    return _out({r})\n\n\n\
             def shown(layer, window, out_shape=None):\n    \
             \"\"\"What the series shows at a date: the expression over its bands, masked by the quality band.\"\"\"\n    \
             path, bands, qa_band, date = layer\n    \
             with np.errstate(all=\"ignore\"):\n        \
             v = expression([read(path, k, window, out_shape) for k in bands], *date)\n    \
             if qa_band:\n        \
             v = np.where(usable(read(path, qa_band, window, out_shape, raw=True), qa_rule), v, np.nan)\n    \
             return v\n",
            e.text.replace('\n', "; ").replace('"', "'")
        );
    } else {
        o += SHOWN_FUNCTION;
    }
    o
}

/// Which series: a pin's, the ROI's.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Pin(u32),
    Roi,
}

impl State {
    /// The code for a series of the chart (None if it is not there).
    pub(crate) fn python_snippet(&mut self, which: Which) -> Option<String> {
        let info = self.session.as_ref()?.info.clone();
        let t_n = info.t();
        let view = match which {
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
            Which::Pin(id) => format!("pin {id}"),
        };
        // the tool on the chart: its calls
        let fit_key = match which {
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
                    sensors: crate::zeit::sensors_for(&id, &info),
                    granules: crate::zeit::granules_of(&id, &params, &info),
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

        let sel = info.sel;
        let qa_on = sel.qa_band > 0 && sel.qa_rule != QaRule::None;
        o += &series_code(&info);
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
                          elif qa_rule == \"hls_fmask\":\n    f = np.zeros(c.shape, np.int32)\n    \
                          f[(c & 32) != 0] = 1; f[(c & 16) != 0] = 3; f[(c & 8) != 0] = 2; f[(c & 6) != 0] = 4; f[c == 255] = 255\n\
                          else:\n    f = np.where(c != 0, 0, 4).astype(np.int32)\nfmask_codes = np.where(complete, f, 255)\n";
                } else {
                    o += "fmask_codes = np.where(complete, 0, 255)  # no quality band: every complete observation is clear\n";
                }
                o += "fmask = fmask_codes.astype(np.int32)[:, None, None]\n";
            }
            if inputs.contains(&"nbar_cube")
                && tool_calls.iter().any(|c| c.1.contains(SENTINEL2_ANGLES))
                && view.is_some()
            {
                // the angles from Sentinel-2's metadata: no angle bands, the cube placed on the files' grid
                let roles: Vec<String> = self.zeit.roles.iter().map(|(r, b)| format!("{}: {b}", py_str(r))).collect();
                let _ = writeln!(
                    o,
                    "# The bands to normalize by role (bands of each date's file); the angles come from the metadata.\n\
                     roles = {{{}}}\n\
                     bands = [r for r in {NBAR_ROLES} if r in roles]\n\
                     raw = {{r: np.array([read(layer[0], roles[r], Window(col, row, 1, 1))[0, 0] for layer in layers])\n       \
                     for r in bands}}\n{}\
                     nbar_cube = xr.DataArray(np.stack([raw[r] for r in bands], axis=1)[:, :, None, None],\n                          \
                     dims=(\"time\", \"band\", \"y\", \"x\"),\n                          \
                     coords={{\"time\": {time}, \"band\": bands, \"y\": y, \"x\": x}}).rio.write_crs(crs)",
                    roles.join(", "),
                    grid_lines(&info, "np.array([col])", "np.array([row])")
                );
            } else if inputs.contains(&"nbar_cube") {
                let roles: Vec<String> = self.zeit.roles.iter().map(|(r, b)| format!("{}: {b}", py_str(r))).collect();
                let _ = writeln!(
                    o,
                    "# The bands to normalize and the four angle bands by role (bands of each date's file), as the files\n\
                     # hold them: the tool's call scales the angles.\n\
                     roles = {{{}}}\n\
                     bands = [r for r in {NBAR_ROLES} if r in roles]\n\
                     raw = {{r: np.array([read(layer[0], roles[r], Window(col, row, 1, 1))[0, 0] for layer in layers])\n       \
                     for r in (*bands, \"sza\", \"saa\", \"vza\", \"vaa\")}}\n\
                     nbar_cube = xr.DataArray(np.stack([raw[r] for r in bands], axis=1)[:, :, None, None],\n                          \
                     dims=(\"time\", \"band\", \"y\", \"x\"), coords={{\"time\": {time}, \"band\": bands}})\n\
                     angles_raw = xr.Dataset({{a.upper(): ((\"time\", \"y\", \"x\"), raw[a][:, None, None])\n                          \
                     for a in (\"sza\", \"saa\", \"vza\", \"vaa\")}}, coords={{\"time\": {time}}})",
                    roles.join(", ")
                );
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

    /// The chart's "Python..." menu: the code of a pin's or the ROI's series in the Python history's editor.
    pub(crate) fn copy_python_ui(&mut self, ui: &mut egui::Ui) {
        let any = !self.series.pins.is_empty() || self.series.roi.complete();
        let mut chosen = None;
        const HELP: &str = "Python that reproduces outside Janus the series on the chart: it reads it from the\n\
                            files as Janus does (band or index, quality mask, nodata, scale), computes its\n\
                            statistics and, with a Zeit tool on the chart, makes the same Zeit calls.\n\
                            Shown in an editor: read it, copy it, save it as a .py or a notebook.\n\
                            Needs numpy and rasterio (xarray and zeit for a tool).";
        // the button itself in the toolbar's row (in a child Ui of its own, it could neither move to the next row
        // nor fit the end of this one)
        if !any {
            ui.add_enabled(false, egui::Button::new("Python..."))
                .on_disabled_hover_text(format!("{HELP}\n\nDrop a pin (a click on the map) or draw an ROI first"));
            return;
        }
        ui.menu_button("Python...", |ui| {
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
            let layer = self.layers.get(self.active).map_or(String::new(), |l| format!(" of {}", l.name));
            let what = match w {
                Which::Pin(id) => format!("Pin {id}'s series"),
                Which::Roi => "The ROI's series".to_string(),
            };
            self.history.show_snippet(&format!("{what}{layer}"), code);
        }
    }
}

/// What marks the Copy Python of NBAR with the angles from Sentinel-2's metadata.
const SENTINEL2_ANGLES: &str = "zeit.sentinel2_angles";

/// The lines that give a cube its place (`zeit.sentinel2_angles` puts the angles on the cube's CRS and pixel centres):
/// the files' geotransform, the centres of the columns and rows given (numpy expressions) and the CRS.
fn grid_lines(info: &janus_cube::CubeInfo, cols: &str, rows: &str) -> String {
    let gt = info.geo_transform.unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, -1.0]);
    let gt: Vec<String> = gt.iter().map(|&v| num(v)).collect();
    format!(
        "import rioxarray  # noqa: F401 (the .rio accessor)\n\
         # The cube's place: the files' CRS and the pixel centres of the cells read.\n\
         crs = {}\ngt = ({})\nx = gt[0] + ({cols} + 0.5) * gt[1]\ny = gt[3] + ({rows} + 0.5) * gt[5]\n",
        py_str(&info.crs_wkt),
        gt.join(", ")
    )
}

/// The roles NBAR normalizes (zeit-core's), as a Python tuple.
const NBAR_ROLES: &str = "(\"coastal\", \"blue\", \"green\", \"red\", \"rededge1\", \"rededge2\", \"rededge3\", \"nir\", \"nir08\", \
                          \"swir1\", \"swir2\")";

/// Code that writes every map of the results (`result1`, `result2`...) as a GeoTIFF on the window's grid.
const SAVE_RESULTS: &str = r#"
os.makedirs(out_dir, exist_ok=True)
with rasterio.open(layers[0][0]) as src:
    crs, transform = src.crs, src.window_transform(window)


def save(name, a):
    """A map (y, x), or a stack of them (each a band), as float32 with NaN where it has no value."""
    a = np.asarray(a)
    if a.dtype.kind not in "biuf":
        print("left out:", name, a.dtype)
        return
    a = a.astype("float32").reshape(-1, a.shape[-2], a.shape[-1])
    path = os.path.join(out_dir, name + ".tif")
    with rasterio.open(path, "w", driver="GTiff", width=a.shape[2], height=a.shape[1], count=a.shape[0],
                       dtype="float32", crs=crs, transform=transform, nodata=float("nan"), compress="deflate") as dst:
        dst.write(a)
    print(path)


results = [(k, v) for k, v in sorted(globals().items()) if k.startswith("result") and k[6:].isdigit()]
for key, r in results:
    ds = r.to_dataset(name=key) if isinstance(r, xr.DataArray) else r
    for var, a in getattr(ds, "data_vars", {}).items():
        if a.ndim >= 2 and a.dims[-2:] == ("y", "x"):
            save(var if len(results) == 1 else f"{key}_{var}", a.values)
"#;

/// The Zeit calls of `tool` with the run's parameters (`calls`, its `n_jobs` the run's threads) and the inputs they
/// need, over the series `info` (the bands by `roles`).
pub(crate) fn tool_calls(
    info: &janus_cube::CubeInfo,
    tool: &janus_zeit::Tool,
    params: &std::collections::BTreeMap<String, serde_json::Value>,
    roles: &std::collections::BTreeMap<String, i32>,
    threads: Option<i64>,
) -> Result<(String, Vec<&'static str>), String> {
    let t_n = info.t();
    let p = janus_zeit::Params::with_defaults(&tool.id, params).ok_or("its parameters are not the tool's")?;
    let input = janus_zeit::PixelInput {
        years: (0..t_n).map(|t| info.decimal_year(t)).collect(),
        days: info.time_is_date.then(|| (0..t_n).map(|t| info.unix_day(t)).collect()),
        values: vec![0.0; t_n],
        bands: roles.keys().map(|r| (r.clone(), vec![])).collect(),
        sensors: crate::zeit::sensors_for(&tool.id, info),
        granules: crate::zeit::granules_of(&tool.id, &p, info),
        ..Default::default()
    };
    let (code, inputs) = janus_zeit::python::calls(&tool.id, &p, &input)
        .ok_or_else(|| format!("{} has no Python of its raster run", tool.name))?;
    Ok((code.replace("n_jobs=1", &format!("n_jobs={}", threads.filter(|&n| n > 0).unwrap_or(-1))), inputs))
}

/// The start of a raster run's script: its docstring (`what` it reproduces), the imports, the series read from its
/// files and the run's window.
pub(crate) fn script_head(info: &janus_cube::CubeInfo, what: &str, tables: bool, window: [usize; 4]) -> String {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let mut o = String::new();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = janus_zeit::numeric::civil(now.div_euclid(86400));
    let _ = write!(
        o,
        "\"\"\"Janus {} ({y:04}-{m:02}-{d:02}): {what} reproduced outside Janus: the series read from its files\n\
         over the run's window, the same Zeit calls with the run's parameters, and every map they return written as a\n\
         GeoTIFF (Janus wrote the maps of its outputs from the same calls). The whole window is read into memory.\n\
         Needs: pip install numpy rasterio xarray zeit-cdts{}\n\"\"\"\n\
         import os\nimport numpy as np\nimport rasterio\nimport xarray as xr\nimport zeit\n\
         from rasterio.windows import Window\nfrom rasterio.enums import Resampling\n{}",
        env!("CARGO_PKG_VERSION"),
        if tables { " pandas" } else { "" },
        if tables { "import pandas as pd\n" } else { "" }
    );
    o += &series_code(info);
    let _ = writeln!(
        o,
        "\n# The run's window: columns {x0}..{x1}, rows {y0}..{y1} of the files' pixels ({w} x {h}).\n\
         window = Window({x0}, {y0}, {w}, {h})\n\
         values = np.stack([shown(layer, window) for layer in layers])  # (time, y, x)"
    );
    o
}

/// The coordinate of the series' time dimension in the script: its dates, or its years, or its positions.
fn time_coord(info: &janus_cube::CubeInfo) -> String {
    let t_n = info.t();
    let whole_years = (0..t_n).all(|t| info.decimal_year(t).fract() == 0.0);
    if info.time_is_date {
        "dates.astype(\"datetime64[ns]\")".to_string()
    } else if whole_years {
        let v: Vec<String> = (0..t_n).map(|t| format!("{}", info.decimal_year(t) as i64)).collect();
        format!("np.array([{}])", v.join(", "))
    } else {
        "np.arange(len(layers))".to_string()
    }
}

/// The line that makes the DataArray `series` (time, y, x) out of `values`.
pub(crate) fn series_line(info: &janus_cube::CubeInfo) -> String {
    format!("series = xr.DataArray(values, dims=(\"time\", \"y\", \"x\"), coords={{\"time\": {}}})\n", time_coord(info))
}

/// The end of a raster run's script: every map of the results written as a GeoTIFF in `out_dir`. `names` (a chain's
/// steps: the result's variable and the name its files take) put another name than `result3` before each file's.
pub(crate) fn save_code(out_dir: &str, names: &[(String, String)]) -> String {
    let mut o = format!("\nout_dir = {}\n", py_str(out_dir));
    let mut save = SAVE_RESULTS.to_string();
    if !names.is_empty() {
        let list: Vec<String> = names.iter().map(|(k, n)| format!("{}: {}", py_str(k), py_str(n))).collect();
        let _ = writeln!(o, "names = {{{}}}  # the step each result is of, in the files' names", list.join(", "));
        save = save.replace("f\"{key}_{var}\"", "f\"{names.get(key, key)}_{var}\"");
    }
    o += &save;
    o
}

/// "Copy Python" of a raster run (its provenance's rerun.py): the series read from its files over the run's window,
/// the same Zeit calls with the run's parameters, and every map they return written as a GeoTIFF in `out_dir`.
pub(crate) fn run_code(
    info: &janus_cube::CubeInfo,
    tool: &janus_zeit::Tool,
    params: &std::collections::BTreeMap<String, serde_json::Value>,
    roles: &std::collections::BTreeMap<String, i32>,
    window: [usize; 4],
    threads: Option<i64>,
    out_dir: &str,
) -> Result<String, String> {
    let (code, inputs) = tool_calls(info, tool, params, roles, threads)?;
    let tables = inputs.contains(&"bands_all") || inputs.contains(&"spectrum");
    let mut o = script_head(info, &format!("the run of {}", tool.name), tables, window);
    let time = time_coord(info);
    if inputs.contains(&"series") {
        o += &series_line(info);
    }
    if inputs.contains(&"bands_cube") {
        let sel = info.sel;
        let qa_on = sel.qa_band > 0 && sel.qa_rule != QaRule::None;
        let rs: Vec<String> = roles.iter().map(|(r, b)| format!("{}: {b}", py_str(r))).collect();
        let _ = writeln!(
            o,
            "# The bands by role (bands of each date's file), as reflectance x 10000 (Zeit's convention).\n\
             roles = {{{}}}\n\
             order = [r for r in (\"blue\", \"green\", \"red\", \"nir\", \"swir1\", \"swir2\", \"thermal\") if r in roles]\n\
             raw = {{r: np.stack([read(layer[0], roles[r], window) for layer in layers]) for r in order}}\n\
             med = np.nanmedian(np.concatenate([raw[r].ravel() for r in order if r != \"thermal\"]))\n\
             units = \"unit\" if med <= 2 else \"landsat_c2\" if med >= 6500 else \"x10000\"\n\
             to_x10000 = {{\"unit\": lambda v: v * 10000, \"landsat_c2\": lambda v: (v * 2.75e-5 - 0.2) * 10000,\n             \
             \"x10000\": lambda v: v}}[units]\n\
             cube = np.stack([to_x10000(raw[r]) for r in order])  # (band, time, y, x)\n\
             bands_cube = xr.DataArray(cube.transpose(1, 0, 2, 3), dims=(\"time\", \"band\", \"y\", \"x\"),\n                          \
             coords={{\"time\": {time}, \"band\": order}})\n\
             complete = np.all(np.isfinite(cube), axis=0)",
            rs.join(", ")
        );
        if qa_on {
            o += "q = np.stack([read(layer[0], layer[3], window, raw=True) for layer in layers])\n\
                  c = np.where(np.isfinite(q) & (q >= 0), q, 255).astype(np.int64)\n\
                  if qa_rule == \"fmask\":\n    f = c.astype(np.int32)\n\
                  elif qa_rule == \"landsat_c2\":\n    f = np.zeros(c.shape, np.int32)\n    \
                  f[(c & 128) != 0] = 1; f[(c & 32) != 0] = 3; f[(c & 16) != 0] = 2; f[(c & 10) != 0] = 4; f[(c & 1) != 0] = 255\n\
                  elif qa_rule == \"hls_fmask\":\n    f = np.zeros(c.shape, np.int32)\n    \
                  f[(c & 32) != 0] = 1; f[(c & 16) != 0] = 3; f[(c & 8) != 0] = 2; f[(c & 6) != 0] = 4; f[c == 255] = 255\n\
                  else:\n    f = np.where(c != 0, 0, 4).astype(np.int32)\n\
                  fmask = np.where(complete, f, 255).astype(np.int32)\n";
        } else {
            o += "fmask = np.where(complete, 0, 255).astype(np.int32)  # no quality band: every complete observation clear\n";
        }
    }
    if inputs.contains(&"nbar_cube") && code.contains(SENTINEL2_ANGLES) {
        // the angles from Sentinel-2's metadata: no angle bands, the cube placed on the files' grid
        let rs: Vec<String> = roles.iter().map(|(r, b)| format!("{}: {b}", py_str(r))).collect();
        let [x0, y0, x1, y1] = window;
        let _ = writeln!(
            o,
            "# The bands to normalize by role (bands of each date's file); the angles come from the metadata.\n\
             roles = {{{}}}\n\
             bands = [r for r in {NBAR_ROLES} if r in roles]\n\
             raw = {{r: np.stack([read(layer[0], roles[r], window) for layer in layers]) for r in bands}}\n{}\
             nbar_cube = xr.DataArray(np.stack([raw[r] for r in bands], axis=1), dims=(\"time\", \"band\", \"y\", \"x\"),\n                          \
             coords={{\"time\": {time}, \"band\": bands, \"y\": y, \"x\": x}}).rio.write_crs(crs)",
            rs.join(", "),
            grid_lines(info, &format!("np.arange({x0}, {x1})"), &format!("np.arange({y0}, {y1})"))
        );
    } else if inputs.contains(&"nbar_cube") {
        let rs: Vec<String> = roles.iter().map(|(r, b)| format!("{}: {b}", py_str(r))).collect();
        let _ = writeln!(
            o,
            "# The bands to normalize and the four angle bands by role (bands of each date's file), as the files hold\n\
             # them: the tool's call scales the angles.\n\
             roles = {{{}}}\n\
             bands = [r for r in {NBAR_ROLES} if r in roles]\n\
             raw = {{r: np.stack([read(layer[0], roles[r], window) for layer in layers])\n       \
             for r in (*bands, \"sza\", \"saa\", \"vza\", \"vaa\")}}\n\
             nbar_cube = xr.DataArray(np.stack([raw[r] for r in bands], axis=1), dims=(\"time\", \"band\", \"y\", \"x\"),\n                          \
             coords={{\"time\": {time}, \"band\": bands}})\n\
             angles_raw = xr.Dataset({{a.upper(): ((\"time\", \"y\", \"x\"), raw[a]) for a in (\"sza\", \"saa\", \"vza\", \"vaa\")}},\n                         \
             coords={{\"time\": {time}}})",
            rs.join(", ")
        );
    }
    if inputs.contains(&"bands_all") {
        let bases: Vec<String> = info.layers.iter().map(|l| l.band_base.to_string()).collect();
        let _ = writeln!(
            o,
            "# Every band of each date (b001, b002...), left out where the shown series is.\n\
             n_bands = {}\nbases = [{}]  # each date's first band in its file, less 1\n\
             cube = np.stack([np.stack([read(layer[0], base + b, window) for b in range(1, n_bands + 1)])\n                 \
             for layer, base in zip(layers, bases)])  # (time, band, y, x)\n\
             cube[np.broadcast_to(~np.isfinite(values)[:, None], cube.shape)] = np.nan\n\
             bands_all = xr.DataArray(cube, dims=(\"time\", \"band\", \"y\", \"x\"),\n                         \
             coords={{\"time\": {time}, \"band\": [f\"b{{b:03d}}\" for b in range(1, n_bands + 1)]}})",
            info.bands_per_date,
            bases.join(", ")
        );
    }
    if inputs.contains(&"spectrum") {
        o += "spectrum = xr.DataArray(values, dims=(\"band\", \"y\", \"x\"),\n                        \
              coords={\"band\": [f\"b{i + 1:03d}\" for i in range(len(layers))]})\n";
    }
    let _ = writeln!(o, "\n# {}, with the run's parameters\n{code}", tool.name);
    o += &save_code(out_dir, &[]);
    Ok(o)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use gdal::Metadata;
    use janus_cube::{BandSelection, open_cube};
    use janus_zeit::job::run_job;
    use janus_zeit::manifest;
    use serde_json::{Value, json};

    use crate::zeit::{base_spec, series_inputs};

    const W: usize = 10;
    const H: usize = 8;
    const T: usize = 6;
    /// The bands of each date's file: three to normalize and the four angles (hundredths of a degree).
    const BANDS: [&str; 7] = ["blue", "nir", "swir1", "sza", "saa", "vza", "vaa"];

    /// A series of `T` dates, a file each with `BANDS`: reflectance of 0.05-0.5 with gaps, a solar zenith of the date
    /// that varies a little over the scene, a view zenith of 0-4 degrees, a pixel outside the model's domain.
    fn series(dir: &Path) -> janus_cube::CubeInfo {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut rng = 4321u64;
        let mut next = move || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as f32 / (1u64 << 31) as f32
        };
        for k in 0..T {
            let day = 18300 + 16 * k as i64;
            let (y, m, d) = janus_zeit::numeric::civil(day);
            let path = dir.join(format!("n_{y:04}-{m:02}-{d:02}.tif"));
            let mut ds = drv.create_with_band_type::<f32, _>(&path, W, H, BANDS.len()).unwrap();
            ds.set_geo_transform(&[500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).unwrap();
            for (b, name) in BANDS.iter().enumerate() {
                let data: Vec<f32> = (0..W * H)
                    .map(|p| {
                        let (x, row) = ((p % W) as f32, (p / W) as f32);
                        match *name {
                            "sza" if p == 0 => 9500.0,
                            "sza" => 100.0 * (30.0 + 4.0 * k as f32 + 0.1 * row + 0.2 * next()),
                            "saa" => 100.0 * (140.0 + k as f32 + 0.1 * x),
                            "vza" => 100.0 * ((x - 4.5).abs() * 0.8 + 0.1 * next()),
                            "vaa" => 100.0 * if x < 4.5 { 280.0 } else { 100.0 },
                            _ if next() < 0.08 => -9999.0,
                            _ => (0.05 + 0.45 * next()) * (b as f32 + 1.0) / 3.0,
                        }
                    })
                    .collect();
                let mut band = ds.rasterband(b + 1).unwrap();
                band.set_no_data_value(Some(-9999.0)).unwrap();
                band.set_description(name).unwrap();
                band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
            }
        }
        open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap()
    }

    /// A raster's bands as float32 values over the run's window (the file is that window's), NoData as NaN.
    fn read(path: &str) -> Vec<Vec<f32>> {
        let ds = gdal::Dataset::open(path).unwrap();
        let (w, h) = ds.raster_size();
        (1..=ds.raster_count())
            .map(|b| {
                let band = ds.rasterband(b).unwrap();
                let v = band.read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap();
                v.data().to_vec()
            })
            .collect()
    }

    /// The raster run's Copy Python (the rerun script) run in Zeit (the Python of JANUS_PYTHON, with zeit, rasterio
    /// and xarray; the test is left out without it) gives the maps `run_job` wrote: the bands read by role, the
    /// angles scaled by the parameter, the scene's solar zenith over the window.
    #[test]
    fn the_nbar_script_runs_in_zeit_and_gives_the_runs_values() {
        let Ok(python) = std::env::var("JANUS_PYTHON") else {
            println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio and xarray");
            return;
        };
        let base = std::env::temp_dir().join("janus-nbar-test-script");
        let info = series(&base.join("series"));
        let tool = manifest().tool("nbar").unwrap();
        let roles: BTreeMap<String, i32> =
            BANDS.iter().enumerate().map(|(k, r)| (r.to_string(), k as i32 + 1)).collect();
        let params: BTreeMap<String, Value> =
            serde_json::from_value(json!({ "angle_scale": "hundredths", "cfactor": true })).unwrap();
        let window = [1, 1, W - 1, H - 1];
        let mut spec = base_spec(&info, tool, &params, T - 1, 2);
        series_inputs(&info, tool, &roles, &base.join("in"), &mut spec).unwrap();
        spec.window = Some(window);
        spec.output_dir = base.join("out").to_string_lossy().into_owned();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        // the run's outputs: a series per band and per c-factor
        let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["nbar_blue", "nbar_nir", "nbar_swir1", "cfactor_blue", "cfactor_nir", "cfactor_swir1"]);

        let out = base.join("python");
        let code = super::run_code(&info, tool, &params, &roles, window, Some(2), &out.to_string_lossy()).unwrap();
        assert!(code.contains("zeit.nbar(nbar_cube, angles, sza='scene'"), "{code}");
        let script = base.join("nbar.py");
        std::fs::write(&script, code).unwrap();
        run_python(&python, &script);
        // the script's stacks are (date, band) in the core's order; the run's files a band per date
        let (mut values, mut worst) = (0, 0f32);
        for (r, prefix) in [("result1", "nbar"), ("result2", "cfactor")] {
            let got = read(&out.join(format!("{r}_{r}.tif")).to_string_lossy());
            assert_eq!(got.len(), T * 3);
            for (b, role) in ["blue", "nir", "swir1"].iter().enumerate() {
                let id = format!("{prefix}_{role}");
                let o = result.outputs.iter().find(|o| o["id"] == id.as_str()).unwrap();
                let want = read(o["path"].as_str().unwrap());
                assert_eq!(want.len(), T);
                for k in 0..T {
                    for (g, w) in got[k * 3 + b].iter().zip(&want[k]) {
                        assert!(
                            (g.is_nan() && w.is_nan()) || (g - w).abs() <= 1e-5 * w.abs(),
                            "{prefix} {role} date {k}: {g} vs {w}"
                        );
                        if g.is_finite() {
                            worst = worst.max((g - w).abs() / w.abs().max(1e-12));
                            values += 1;
                        }
                    }
                }
            }
        }
        println!("{values} values of the script compared with the run's, the largest relative difference {worst:e}");
        assert!(values > 500);
    }

    /// The dates of the Sentinel-2 series below.
    const S2_T: usize = 3;

    /// A script run by the Python of JANUS_PYTHON on the PATH as an activated conda environment has it and Windows'
    /// own (the other DLLs shadow the environment's); it must succeed.
    fn run_python(python: &str, script: &Path) {
        let root = Path::new(python).parent().unwrap().to_path_buf();
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let mut path: Vec<String> = ["", "Library\\mingw-w64\\bin", "Library\\usr\\bin", "Library\\bin", "Scripts"]
            .iter()
            .map(|d| root.join(d).to_string_lossy().into_owned())
            .collect();
        path.extend([format!("{system}\\system32"), system.clone()]);
        let run = std::process::Command::new(python)
            .arg(script)
            .env_clear()
            .envs(["SystemRoot", "TEMP", "TMP"].iter().filter_map(|k| Some((*k, std::env::var(k).ok()?))))
            .env("PATH", path.join(";"))
            .env("PYTHONNOUSERSITE", "1")
            .env("OMP_NUM_THREADS", "2")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    /// A series of Sentinel-2 dates in tile 23LKC (UTM 23S, pixels of 1.6 km across the east edge of the partial
    /// swath of 2023-07-26), `BANDS`-like files of three bands (no angle bands), each carrying its granule's metadata
    /// as the VRTs of a STAC series do (the fixtures of Zeit's tests).
    fn s2_series(dir: &Path) -> janus_cube::CubeInfo {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../janus-zeit/tests/data/s2angles");
        let srs = gdal::spatial_ref::SpatialRef::from_epsg(32723).unwrap();
        let mut rng = 99u64;
        let mut next = move || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as f32 / (1u64 << 31) as f32
        };
        for (date, xml) in [
            ("2023-07-23", "s2_23LKC_20230723_MTD_TL.xml"),
            ("2023-07-24", "s2_23LKC_20230723_MTD_TL.xml"),
            ("2023-07-26", "s2_23LKC_20230726_partial_MTD_TL.xml"),
        ] {
            let path = dir.join(format!("s2_{date}.tif"));
            let mut ds = drv.create_with_band_type::<f32, _>(&path, W, H, 3).unwrap();
            ds.set_geo_transform(&[199980.0 + 12500.0, 1600.0, 0.0, 8300020.0 - 3000.0, 0.0, -1600.0]).unwrap();
            ds.set_spatial_ref(&srs).unwrap();
            ds.set_metadata_item("STAC_GRANULE_METADATA", &fixtures.join(xml).to_string_lossy(), "").unwrap();
            for (b, name) in ["blue", "nir", "swir1"].iter().enumerate() {
                let data: Vec<f32> = (0..W * H)
                    .map(|_| if next() < 0.05 { -9999.0 } else { (0.05 + 0.4 * next()) * (b as f32 + 1.0) / 3.0 })
                    .collect();
                let mut band = ds.rasterband(b + 1).unwrap();
                band.set_no_data_value(Some(-9999.0)).unwrap();
                band.set_description(name).unwrap();
                band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
            }
        }
        open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap()
    }

    /// With the angles from Sentinel-2's metadata the spec carries each date's files (found from the files'
    /// metadata), the run needs no angle bands, and its Copy Python (`zeit.sentinel2_angles` on the cube placed on the
    /// files' grid, then `zeit.nbar`) run in Zeit gives the maps `run_job` wrote.
    #[test]
    fn the_nbar_sentinel2_script_runs_in_zeit_and_gives_the_runs_values() {
        let Ok(python) = std::env::var("JANUS_PYTHON") else {
            println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio, rioxarray and xarray");
            return;
        };
        let base = std::env::temp_dir().join("janus-nbar-s2-test-script");
        let info = s2_series(&base.join("series"));
        let tool = manifest().tool("nbar").unwrap();
        let roles: BTreeMap<String, i32> =
            ["blue", "nir", "swir1"].iter().enumerate().map(|(k, r)| (r.to_string(), k as i32 + 1)).collect();
        let params: BTreeMap<String, Value> =
            serde_json::from_value(json!({ "angles": "sentinel2", "view_band": "B06", "cfactor": true })).unwrap();
        let window = [1, 1, W - 1, H - 1];
        let mut spec = base_spec(&info, tool, &params, S2_T - 1, 2);
        series_inputs(&info, tool, &roles, &base.join("in"), &mut spec).unwrap();
        assert_eq!(spec.granules.len(), 3);
        assert!(spec.granules.iter().all(|g| g.len() == 1 && g[0].ends_with("_MTD_TL.xml")), "{:?}", spec.granules);
        spec.window = Some(window);
        spec.output_dir = base.join("out").to_string_lossy().into_owned();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["nbar_blue", "nbar_nir", "nbar_swir1", "cfactor_blue", "cfactor_nir", "cfactor_swir1"]);

        let out = base.join("python");
        let code = super::run_code(&info, tool, &params, &roles, window, Some(2), &out.to_string_lossy()).unwrap();
        assert!(code.contains("angles = zeit.sentinel2_angles(sources, like=nbar_cube, view_band='B06')"), "{code}");
        assert!(
            code.contains("MTD_TL.xml") && code.contains(".rio.write_crs(crs)") && !code.contains("angles_raw"),
            "{code}"
        );
        let script = base.join("nbar.py");
        std::fs::write(&script, code).unwrap();
        run_python(&python, &script);
        let (mut values, mut worst) = (0, 0f32);
        for (r, prefix) in [("result1", "nbar"), ("result2", "cfactor")] {
            let got = read(&out.join(format!("{r}_{r}.tif")).to_string_lossy());
            assert_eq!(got.len(), S2_T * 3);
            for (b, role) in ["blue", "nir", "swir1"].iter().enumerate() {
                let id = format!("{prefix}_{role}");
                let o = result.outputs.iter().find(|o| o["id"] == id.as_str()).unwrap();
                let want = read(o["path"].as_str().unwrap());
                assert_eq!(want.len(), S2_T);
                for k in 0..S2_T {
                    for (g, w) in got[k * 3 + b].iter().zip(&want[k]) {
                        assert!(
                            (g.is_nan() && w.is_nan()) || (g - w).abs() <= 1e-5 * w.abs(),
                            "{prefix} {role} date {k}: {g} vs {w}"
                        );
                        if g.is_finite() {
                            worst = worst.max((g - w).abs() / w.abs().max(1e-12));
                            values += 1;
                        }
                    }
                }
            }
        }
        println!(
            "{values} values of the Sentinel-2 script compared with the run's, the largest relative difference {worst:e}"
        );
        assert!(values > 300);
    }
}
