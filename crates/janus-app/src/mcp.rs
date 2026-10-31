//! `janus mcp`: Janus as a server of the Model Context Protocol (MCP), on the standard input and output. An
//! assistant (Claude Desktop, Claude Code, any client of MCP) opens series, reads their values at points and over
//! areas, writes and runs the raster calculator's expressions, runs Zeit's tools (on a series or on maps) and the
//! series' and maps' tools (regularize, speckle, reproject, zonal statistics, areas, polygonize, rasterize), trains
//! a Random Forest, TempCNN or L-TAE on samples and classifies a series: the engines the interface runs, without
//! it. Results are files (GeoTIFFs, VRTs, GeoPackages, CSVs, models), in the folder the call names or under Janus's
//! results folder (`mcp/`), shown to the user in the window already open (inbox.rs) or a new one. A long call
//! tells the client how it goes (progress notifications, when it gave a token) and stops when the client cancels
//! it.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_cube::calc::plan::{self, CalcLayer, DateMatch, Options};
use janus_cube::{BandSelection, CubeInfo};
use janus_ml::forest::ForestHyper;
use janus_ml::{Calendar, InputBand, TrainConfig};
use janus_zeit::manifest;
use serde_json::{Value, json};

use crate::ml::Site;

/// The versions of the protocol spoken (the newest first): the client's, when it is one of them.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Janus explores and analyses raster time series (a folder of GeoTIFFs, one per date, or \
a multiband file; NetCDF, Zarr, STAC VRTs). A series is given as a path (a folder, a file, a wildcard pattern) or \
as {\"paths\": [...], \"band\": N, \"expression\": \"NDVI\" or a formula over the bands, \"qa_band\": N}. Start with \
describe_series. Places are {\"col\", \"row\"} (pixels of the series), {\"x\", \"y\"} (its CRS) or {\"lon\", \"lat\"}; \
areas a \"window\" [col0, row0, col1, row1] or a \"bbox\" [xmin, ymin, xmax, ymax] in the CRS, or \"bbox_lonlat\". \
The raster calculator (check_expression, then calculate) takes Janus's language over layers named by alias: \
the alias alone is the quantity the layer shows (its expression), `s2.nir` a band by name or role; lines such as \
`ndvi = (s2.nir - s2.red) / (s2.nir + s2.red)`, then `ndvi - mean(ndvi)`, `at(ndvi, 2020)`, rules \
`ndvi < 0.2 = 0`, `max(ndvi) by year`. list_indices gives the spectral indices' formulas. Zeit's tools \
(list_tools, describe_tool, run_tool) write maps of change, trends, phenology, classes. Every result is files on \
disk, their paths in the answer; open_in_janus shows them to the user in Janus.";

/// The server's output, shared by the calls that answer from their threads: a message per line.
type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, v: &Value) {
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

/// What a tool's call fails with: the client sees it as the tool's error (isError), not the protocol's.
type R<T> = Result<T, String>;

/// The series opened, by their description (opening a folder of hundreds of files takes a moment).
#[derive(Default)]
struct Server {
    series: Mutex<HashMap<String, Arc<CubeInfo>>>,
    threads: usize,
}

/// Serves until the input ends; the exit code.
pub fn serve(threads: usize) -> i32 {
    let threads = threads.max(1);
    let _ = rayon::ThreadPoolBuilder::new().num_threads(threads).build_global();
    let server = Arc::new(Server { threads, ..Default::default() });
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let calls: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> = Arc::default();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let m: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                send(&out, &failure(Value::Null, -32700, &format!("not JSON: {e}")));
                continue;
            }
        };
        let all = match m {
            Value::Array(a) => a,
            m => vec![m],
        };
        for m in all {
            if m["method"] == "notifications/cancelled" {
                if let Some(stop) = calls.lock().unwrap().get(&m["params"]["requestId"].to_string()) {
                    stop.store(true, Ordering::Relaxed);
                }
                continue;
            }
            if m["method"] == "tools/call" && !m["id"].is_null() {
                // each call on a thread of its own: the others answered meanwhile, this one cancelled
                let (server, out, calls) = (server.clone(), out.clone(), calls.clone());
                let key = m["id"].to_string();
                let stop = Arc::new(AtomicBool::new(false));
                calls.lock().unwrap().insert(key.clone(), stop.clone());
                std::thread::spawn(move || {
                    let token = m["params"]["_meta"]["progressToken"].clone();
                    let ctx = Call { out: out.clone(), token, sent: Mutex::new((0, Instant::now())), stop };
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.call(&m["params"], &ctx)))
                        .unwrap_or_else(|_| Err("the tool failed (a panic): please report it".into()));
                    calls.lock().unwrap().remove(&key);
                    send(&out, &json!({ "jsonrpc": "2.0", "id": m["id"], "result": tool_result(r) }));
                });
                continue;
            }
            if let Some(r) = server.answer(&m) {
                send(&out, &r);
            }
        }
    }
    0
}

/// A call under way: its progress told to the client (at most every 0.25 s), its stop.
struct Call {
    out: Out,
    token: Value,
    sent: Mutex<(u64, Instant)>,
    stop: Arc<AtomicBool>,
}

impl Call {
    fn progress(&self, fraction: f64, message: &str) {
        if self.token.is_null() {
            return;
        }
        let mut s = self.sent.lock().unwrap();
        if s.0 > 0 && s.1.elapsed().as_secs_f64() < 0.25 && fraction < 1.0 {
            return;
        }
        *s = (s.0 + 1, Instant::now());
        send(
            &self.out,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": { "progressToken": self.token, "progress": (fraction.clamp(0.0, 1.0) * 1000.0).round(), "total": 1000, "message": message },
            }),
        );
    }
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool's answer: its JSON as text (and as structured content), or its error.
fn tool_result(r: R<Value>) -> Value {
    match r {
        Ok(v) => json!({
            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
            "structuredContent": v,
        }),
        Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
    }
}

impl Server {
    /// The answer to a request other than a tool's call; none to a notification.
    fn answer(&self, m: &Value) -> Option<Value> {
        let id = m.get("id").cloned()?;
        let method = m.get("method").and_then(Value::as_str)?;
        let params = m.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or("");
                let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "janus", "title": "Janus", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": catalogue() })),
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            _ => Err((-32601, format!("no method {method:?}"))),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => failure(id, code, &message),
        })
    }

    fn call(&self, params: &Value, c: &Call) -> R<Value> {
        let name = params["name"].as_str().unwrap_or_default();
        let a = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        match name {
            "describe_series" => self.describe_series(&a),
            "list_indices" => Ok(list_indices(&a)),
            "pixel_series" => self.pixel_series(&a),
            "region_series" => self.region_series(&a),
            "check_expression" => self.check_expression(&a),
            "calculate" => self.calculate(&a, c),
            "list_tools" => Ok(list_tools()),
            "describe_tool" => describe_tool(&a),
            "run_tool" => self.run_tool(&a, c),
            "regularize" => self.regularize(&a, c),
            "warp" => self.warp(&a, c),
            "zonal" => self.zonal(&a, c),
            "train_classifier" => self.train_classifier(&a, c),
            "classify" => self.classify(&a, c),
            "open_in_janus" => open_in_janus(&a),
            "polygonize" => self.polygonize(&a, c),
            "rasterize" => self.rasterize(&a, c),
            "speckle" => self.speckle(&a, c),
            "areas" => self.areas(&a, c),
            _ => Err(format!("no tool {name:?}")),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The catalogue
// ---------------------------------------------------------------------------------------------------------------

fn series_schema() -> Value {
    json!({
        "description": "A series: a path (folder, file, wildcard) or {paths, band, expression, nd_band, qa_band}",
        "anyOf": [
            { "type": "string" },
            { "type": "object", "properties": {
                "paths": { "type": "array", "items": { "type": "string" } },
                "path": { "type": "string" },
                "band": { "type": "integer", "description": "the band shown, of each date's file (1-based)" },
                "expression": { "type": "string", "description": "an index of the library by name (NDVI, EVI, NBR...) or a formula over the bands (b4, nir...)" },
                "nd_band": { "type": "integer", "description": "the normalized difference (band - nd_band) / (band + nd_band)" },
                "qa_band": { "type": "integer", "description": "a quality band masking clouds (Fmask, QA_PIXEL, SCL codes)" }
            } }
        ]
    })
}

fn place_schema() -> Value {
    json!({
        "type": "object",
        "description": "{col, row} (pixels), {x, y} (the series' CRS) or {lon, lat}",
        "properties": {
            "col": { "type": "number" }, "row": { "type": "number" },
            "x": { "type": "number" }, "y": { "type": "number" },
            "lon": { "type": "number" }, "lat": { "type": "number" },
            "class": { "type": "string" }
        }
    })
}

fn area_props() -> Value {
    json!({
        "window": { "type": "array", "items": { "type": "integer" }, "description": "[col0, row0, col1, row1] in the series' pixels (col1, row1 excluded)" },
        "bbox": { "type": "array", "items": { "type": "number" }, "description": "[xmin, ymin, xmax, ymax] in the series' CRS" },
        "bbox_lonlat": { "type": "array", "items": { "type": "number" }, "description": "[lon_min, lat_min, lon_max, lat_max]" }
    })
}

fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    base
}

fn tool(name: &str, title: &str, description: &str, props: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required },
        "annotations": { "readOnlyHint": read_only, "openWorldHint": false },
    })
}

fn catalogue() -> Vec<Value> {
    let out_dir = json!({ "type": "string", "description": "the folder of the results (default: Janus's results folder, mcp/<tool>_<time>)" });
    vec![
        tool(
            "describe_series",
            "Describe a series",
            "Opens a series and tells its size, CRS, pixel size, extent (also in longitude and latitude), bands, dates and what is shown (band, index, quality mask).",
            json!({ "series": series_schema() }),
            &["series"],
            true,
        ),
        tool(
            "list_indices",
            "List spectral indices",
            "The spectral indices Janus knows by name (Awesome Spectral Indices and radar ones): name, long name, formula, bands it needs. Filter by a text or a domain (vegetation, water, burn, soil, urban, snow, radar).",
            json!({ "filter": { "type": "string" }, "domain": { "type": "string" } }),
            &[],
            true,
        ),
        tool(
            "pixel_series",
            "Values of places through time",
            "The series' values at places (pixels, coordinates or longitude and latitude): one value a date each, with the dates.",
            json!({ "series": series_schema(), "places": { "type": "array", "items": place_schema() }, "bands": { "type": "array", "items": { "type": "integer" }, "description": "also these bands of each date (raw)" } }),
            &["series", "places"],
            true,
        ),
        tool(
            "region_series",
            "Statistics of an area through time",
            "Each date's statistics of an area of the series (valid pixels, mean, std, min, p10, median, p90, max); a large area is sampled on a regular grid.",
            with(
                json!({ "series": series_schema(), "max_pixels": { "type": "integer", "description": "pixels read at most (default 200000)" } }),
                area_props(),
            ),
            &["series"],
            true,
        ),
        tool(
            "check_expression",
            "Check a calculator expression",
            "Plans a raster calculator text over named layers without computing it: the result's dates (or an image), grid and size, the layers and bands it reads; or the error with its line and column. Use before calculate.",
            json!({
                "text": { "type": "string", "description": "the calculator's text: lines `name = expression`, rules `condition = value`, layers by alias (`s2.nir`, `ndvi`)" },
                "layers": { "type": "object", "additionalProperties": series_schema(), "description": "the layers by alias, e.g. {\"s2\": \"D:/s2\", \"dem\": \"D:/dem.tif\"}" },
                "options": { "type": "object", "description": "dates_from, grid_from (aliases), matching (\"same\", \"nearest:8\", \"month\", \"year\"), resampling, apply_qa" }
            }),
            &["text", "layers"],
            true,
        ),
        tool(
            "calculate",
            "Run a calculator expression",
            "Computes a raster calculator text over named layers: a GeoTIFF per date of the result (cloud-optimized), or VRTs computed when read (format \"vrt\").",
            json!({
                "text": { "type": "string" },
                "layers": { "type": "object", "additionalProperties": series_schema() },
                "options": { "type": "object" },
                "format": { "type": "string", "enum": ["cog", "vrt"] },
                "output_dir": out_dir
            }),
            &["text", "layers"],
            false,
        ),
        tool(
            "list_tools",
            "List Zeit's tools",
            "Zeit's tools Janus runs on a series (change detection: BFAST, CCDC, LandTrendr, EWMACD, CODED...; trends, phenology, TWDTW and SOM classification, segmentation...): id, name, category, what it needs.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "describe_tool",
            "Describe a Zeit tool",
            "A tool's description, parameters (type, default, range, options, help), outputs and needs.",
            json!({ "tool": { "type": "string" } }),
            &["tool"],
            true,
        ),
        tool(
            "run_tool",
            "Run a Zeit tool on a series",
            "Runs one of Zeit's tools on the series (or an area of it): its maps as GeoTIFFs. TWDTW takes `samples` (places with a class) instead of patterns: each class's mean series becomes its pattern.",
            with(
                json!({
                    "tool": { "type": "string" },
                    "series": series_schema(),
                    "params": { "type": "object", "description": "the tool's parameters by id (describe_tool); the rest at their defaults" },
                    "roles": { "type": "object", "description": "multiband tools: the band of each role, e.g. {\"red\": 3, \"nir\": 4}" },
                    "samples": { "type": "array", "items": place_schema(), "description": "TWDTW: places with a class" },
                    "maps": { "type": "array", "items": { "type": "string" }, "description": "tools on maps (agreement...): two or more rasters, the first's grid" },
                    "output_dir": out_dir
                }),
                area_props(),
            ),
            &["tool"],
            false,
        ),
        tool(
            "regularize",
            "Regularize a series",
            "The series at a fixed step: each period's median, medoid or clearest date, empty periods filled (linearly or by a Whittaker curve); a COG per regular date.",
            with(
                json!({
                    "series": series_schema(),
                    "step": { "type": "string", "description": "\"16d\", \"8d\", \"month\", \"year\" or a number of days" },
                    "composite": { "type": "string", "enum": ["median", "medoid", "clearest"] },
                    "fill": { "type": "string", "description": "\"none\", \"linear\" or \"whittaker:LAMBDA\"" },
                    "max_gap_days": { "type": "number" },
                    "all_bands": { "type": "boolean" },
                    "output_dir": out_dir
                }),
                area_props(),
            ),
            &["series", "step"],
            false,
        ),
        tool(
            "warp",
            "Reproject, resample, clip a raster",
            "A raster (a map, a date) through gdalwarp: another CRS, pixel size, extent, a vector's polygons as cutline; a COG.",
            json!({
                "src": { "type": "string" },
                "dst": { "type": "string" },
                "t_srs": { "type": "string", "description": "EPSG:4326, a WKT, a PROJ string" },
                "pixel": { "type": "number" },
                "extent": { "type": "array", "items": { "type": "number" }, "description": "[xmin, ymin, xmax, ymax] in t_srs (or extent_srs)" },
                "extent_srs": { "type": "string" },
                "cutline": { "type": "string", "description": "a vector file whose polygons cut the result" },
                "resampling": { "type": "string", "enum": ["near", "bilinear", "cubic", "average", "mode", "max", "min", "med"] },
                "format": { "type": "string", "enum": ["cog", "gtiff", "vrt"] }
            }),
            &["src"],
            false,
        ),
        tool(
            "zonal",
            "Zonal statistics",
            "Each polygon's statistics (n, mean, std, min, percentiles, max) of every map given, or its area of each class (kind \"classes\") or of each change year (kind \"years\"); a CSV beside the answer.",
            json!({
                "vector": { "type": "string" },
                "layer": { "type": "string" },
                "id_field": { "type": "string" },
                "maps": { "type": "array", "items": { "type": "string" }, "description": "rasters on one grid (band 1 of each, or every band of one file: a band per date)" },
                "kind": { "type": "string", "enum": ["continuous", "classes", "years"] },
                "all_touched": { "type": "boolean" },
                "output_dir": out_dir
            }),
            &["vector", "maps"],
            false,
        ),
        tool(
            "train_classifier",
            "Train a classifier",
            "Trains a Random Forest (Breiman 2001), TempCNN or L-TAE on samples of the series: places with a class, or a vector file's points or polygons and its class field. Validated on whole places held out, out of bag too; a .janusmodel file for classify (and the interface).",
            json!({
                "series": series_schema(),
                "samples": { "type": "array", "items": place_schema(), "description": "places with a \"class\"" },
                "vector": { "type": "string", "description": "a vector file of samples (points, or polygons: their pixels up to max_pixels)" },
                "field": { "type": "string", "description": "its class field" },
                "max_pixels": { "type": "integer" },
                "bands": { "type": "array", "items": { "type": "integer" }, "description": "also these bands of each date (besides the series shown)" },
                "method": { "type": "string", "enum": ["random_forest", "tempcnn", "ltae"], "description": "the Random Forest (default: quick, on the CPU), or a network as sits builds it (TempCNN, L-TAE; on the GPU when there is one)" },
                "trees": { "type": "integer" },
                "epochs": { "type": "integer", "description": "a network's epochs at most (sits' default otherwise)" },
                "gpu": { "type": "boolean" },
                "validation_share": { "type": "number" },
                "min_valid": { "type": "number", "description": "% of the dates a sample needs a value on (default 50)" },
                "seed": { "type": "integer" },
                "output": { "type": "string", "description": "the model's file (.janusmodel)" }
            }),
            &["series"],
            false,
        ),
        tool(
            "classify",
            "Classify a series with a model",
            "Applies a .janusmodel (a Random Forest or a network trained in Janus) to the series or an area: the class map, its probability and, when the model kept its samples, the area of applicability.",
            with(
                json!({
                    "model": { "type": "string" },
                    "series": series_schema(),
                    "aoa": { "type": "boolean" },
                    "output_dir": out_dir
                }),
                area_props(),
            ),
            &["model", "series"],
            false,
        ),
        tool(
            "polygonize",
            "Polygonize a map",
            "A map of classes or dates into polygons (a GeoPackage): each patch of one value a polygon with its area; small patches merged into a neighbour, the polygons simplified.",
            json!({
                "src": { "type": "string" },
                "band": { "type": "integer" },
                "eight": { "type": "boolean", "description": "patches joined by their corners too" },
                "min_area_ha": { "type": "number" },
                "simplify_m": { "type": "number" },
                "years": { "type": "boolean", "description": "the values are decimal years (a date column)" },
                "dst": { "type": "string", "description": "the GeoPackage (default: in the results folder)" }
            }),
            &["src"],
            false,
        ),
        tool(
            "rasterize",
            "Rasterize a vector layer",
            "A vector layer burned on a raster's grid (a series', a map's): 1 inside, each feature's number, or an attribute (a number as it is, a text as classes); a GeoTIFF.",
            json!({
                "vector": { "type": "string" },
                "layer": { "type": "string" },
                "like": { "description": "a raster or a series whose grid it takes", "anyOf": [{ "type": "string" }, { "type": "object" }] },
                "burn": { "type": "string", "description": "one (default), index or an attribute's name" },
                "all_touched": { "type": "boolean" },
                "dst": { "type": "string" }
            }),
            &["vector", "like"],
            false,
        ),
        tool(
            "speckle",
            "Filter a radar series' speckle",
            "A radar series (Sentinel-1 VV, VH) through Quegan's multitemporal filter or Lee's: less speckle, a COG a date with every band.",
            with(
                json!({
                    "series": series_schema(),
                    "filter": { "type": "string", "description": "quegan (default), lee or lee:LOOKS" },
                    "size": { "type": "integer", "description": "the window's side in pixels (odd, default 7)" },
                    "output_dir": out_dir
                }),
                area_props(),
            ),
            &["series"],
            false,
        ),
        tool(
            "areas",
            "Areas of each value",
            "The area of each class (or each change year, with floor) of maps on one grid, each pixel's own area (geodesic): pixels, hectares, share; a CSV.",
            json!({
                "maps": { "type": "array", "items": { "type": "string" }, "description": "rasters on one grid (band 1 each, or every band of one file)" },
                "window": { "type": "array", "items": { "type": "integer" } },
                "floor": { "type": "boolean", "description": "the values' whole part (decimal years by year), else rounded" },
                "output_dir": out_dir
            }),
            &["maps"],
            false,
        ),
        tool(
            "open_in_janus",
            "Open in Janus",
            "Shows files (a series' folder, result maps, vector layers) to the user in Janus: as layers of the window already open (it comes to the front), else in a new window; places with col and row become pins (with their class).",
            json!({
                "window": { "type": "string", "enum": ["auto", "open", "new"], "description": "auto (default): the open window if there is one" },
                "paths": { "type": "array", "items": { "type": "string" } },
                "expression": { "type": "string", "description": "the index or formula shown (NDVI...)" },
                "places": { "type": "array", "items": place_schema() }
            }),
            &["paths"],
            false,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------------------------
// Series, places, areas
// ---------------------------------------------------------------------------------------------------------------

impl Server {
    /// The series a value names (opened once).
    fn series(&self, v: &Value) -> R<Arc<CubeInfo>> {
        let key = v.to_string();
        if let Some(s) = self.series.lock().unwrap().get(&key) {
            return Ok(s.clone());
        }
        let (paths, o): (Vec<String>, Value) = match v {
            Value::String(p) => (vec![p.clone()], json!({})),
            Value::Object(o) => {
                let mut p: Vec<String> = o
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                p.extend(o.get("path").and_then(Value::as_str).map(String::from));
                (p, v.clone())
            }
            _ => return Err("a series is a path or an object {paths, band, expression...}".into()),
        };
        if paths.is_empty() {
            return Err("the series names no file".into());
        }
        let int = |k: &str| o.get(k).and_then(Value::as_i64).map(|n| n as i32);
        let mut sel = BandSelection { band: int("band").unwrap_or(1).max(1), ..Default::default() };
        if let Some(b) = int("nd_band") {
            sel.nd_band = b;
            sel.chosen = true;
        }
        if let Some(b) = int("qa_band") {
            sel.qa_band = b;
            sel.chosen = true;
        }
        if let Some(e) = o.get("expression").and_then(Value::as_str).filter(|e| !e.trim().is_empty()) {
            let name = janus_cube::calc::indices::find(e.trim()).map_or(String::new(), |i| i.name.clone());
            sel.expr = janus_cube::calc::ExprId::intern(&name, e.trim());
        }
        let info = Arc::new(janus_cube::open_cube(&paths, &sel)?);
        self.series.lock().unwrap().insert(key, info.clone());
        Ok(info)
    }
}

/// Longitude and latitude to the series' CRS, or back.
fn lonlat(info: &CubeInfo, pts: &mut [(f64, f64)], to_lonlat: bool) -> R<()> {
    let mut crs =
        SpatialRef::from_wkt(&info.crs_wkt).map_err(|e| format!("the series' CRS is not understood ({e})"))?;
    let mut ll = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
    crs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    ll.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = if to_lonlat { CoordTransform::new(&crs, &ll) } else { CoordTransform::new(&ll, &crs) }
        .map_err(|e| format!("no transformation between the series' CRS and longitude and latitude: {e}"))?;
    let (mut xs, mut ys): (Vec<f64>, Vec<f64>) = pts.iter().copied().unzip();
    let mut zs = vec![0.0; pts.len()];
    ct.transform_coords(&mut xs, &mut ys, &mut zs).map_err(|e| e.to_string())?;
    for (p, (x, y)) in pts.iter_mut().zip(xs.into_iter().zip(ys)) {
        *p = (x, y);
    }
    Ok(())
}

fn gt_of(info: &CubeInfo) -> [f64; 6] {
    info.geo_transform.unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, 1.0])
}

/// A place's pixel (col, row) as fractions.
fn place_xy(info: &CubeInfo, p: &Value) -> R<(f64, f64)> {
    let f = |k: &str| p.get(k).and_then(Value::as_f64);
    if let (Some(c), Some(r)) = (f("col"), f("row")) {
        return Ok((c, r));
    }
    let (x, y) = if let (Some(x), Some(y)) = (f("x"), f("y")) {
        (x, y)
    } else if let (Some(lon), Some(lat)) = (f("lon"), f("lat")) {
        let mut v = [(lon, lat)];
        lonlat(info, &mut v, false)?;
        v[0]
    } else {
        return Err(format!("a place is {{col, row}}, {{x, y}} or {{lon, lat}}: {p}"));
    };
    let g = gt_of(info);
    let det = g[1] * g[5] - g[2] * g[4];
    if det == 0.0 {
        return Err("the series has no georeference".into());
    }
    let (dx, dy) = (x - g[0], y - g[3]);
    Ok(((dx * g[5] - dy * g[2]) / det, (dy * g[1] - dx * g[4]) / det))
}

/// A place's pixel, inside the series.
fn place_pixel(info: &CubeInfo, p: &Value) -> R<(usize, usize)> {
    let (c, r) = place_xy(info, p)?;
    if c < 0.0 || r < 0.0 || c >= info.width as f64 || r >= info.height as f64 {
        return Err(format!("{p} is outside the series ({} x {} pixels)", info.width, info.height));
    }
    Ok((c as usize, r as usize))
}

/// The area a call names (window, bbox, bbox_lonlat), else the whole series: [col0, row0, col1, row1].
fn window_of(info: &CubeInfo, a: &Value) -> R<[usize; 4]> {
    let nums = |k: &str| -> Option<Vec<f64>> {
        a.get(k)?.as_array().map(|v| v.iter().filter_map(Value::as_f64).collect()).filter(|v: &Vec<f64>| v.len() == 4)
    };
    let (w, h) = (info.width as f64, info.height as f64);
    let clip = |c0: f64, r0: f64, c1: f64, r1: f64| -> R<[usize; 4]> {
        let (c0, c1) = (c0.min(c1).floor().clamp(0.0, w), c0.max(c1).ceil().clamp(0.0, w));
        let (r0, r1) = (r0.min(r1).floor().clamp(0.0, h), r0.max(r1).ceil().clamp(0.0, h));
        if c1 <= c0 || r1 <= r0 {
            return Err("the area is outside the series".into());
        }
        Ok([c0 as usize, r0 as usize, c1 as usize, r1 as usize])
    };
    if let Some(v) = nums("window") {
        return clip(v[0], v[1], v[2], v[3]);
    }
    let corners = if let Some(v) = nums("bbox") {
        Some([(v[0], v[1]), (v[2], v[3]), (v[0], v[3]), (v[2], v[1])])
    } else if let Some(v) = nums("bbox_lonlat") {
        let mut c = [(v[0], v[1]), (v[2], v[3]), (v[0], v[3]), (v[2], v[1])];
        lonlat(info, &mut c, false)?;
        Some(c)
    } else {
        None
    };
    match corners {
        Some(c) => {
            let px: Vec<(f64, f64)> =
                c.iter().map(|&(x, y)| place_xy(info, &json!({ "x": x, "y": y }))).collect::<R<_>>()?;
            let (c0, c1) = px.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
            let (r0, r1) = px.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
            clip(c0, r0, c1, r1)
        }
        None => Ok([0, 0, info.width, info.height]),
    }
}

/// The folder of a call's results: the one named, else Janus's results folder's mcp/<tool>_<time>.
fn out_dir(a: &Value, tool: &str) -> R<PathBuf> {
    let d = match a.get("output_dir").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => crate::settings::app_data_dir()
            .join("results")
            .join("mcp")
            .join(format!("{tool}_{}", crate::zeit::timestamp())),
    };
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

fn num(v: f32) -> Value {
    if v.is_finite() { json!((f64::from(v) * 1e6).round() / 1e6) } else { Value::Null }
}

fn files_of(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

// ---------------------------------------------------------------------------------------------------------------
// The tools
// ---------------------------------------------------------------------------------------------------------------

fn list_indices(a: &Value) -> Value {
    let filter = a["filter"].as_str().unwrap_or("").to_lowercase();
    let domain = a["domain"].as_str().unwrap_or("").to_lowercase();
    let list: Vec<Value> = janus_cube::calc::indices::all()
        .iter()
        .filter(|i| domain.is_empty() || i.domain.to_lowercase() == domain)
        .filter(|i| {
            filter.is_empty()
                || i.name.to_lowercase().contains(&filter)
                || i.long_name.to_lowercase().contains(&filter)
        })
        .map(|i| json!({ "name": i.name, "long_name": i.long_name, "domain": i.domain, "formula": i.formula, "bands": i.roles }))
        .collect();
    json!({ "count": list.len(), "indices": list })
}

impl Server {
    fn describe_series(&self, a: &Value) -> R<Value> {
        let info = self.series(&a["series"])?;
        let g = gt_of(&info);
        let (w, h) = (info.width as f64, info.height as f64);
        let extent = [g[0], g[3] + h * g[5], g[0] + w * g[1], g[3]];
        let mut corners = [(extent[0], extent[1]), (extent[2], extent[3])];
        let ll =
            lonlat(&info, &mut corners, true).ok().map(|_| [corners[0].0, corners[0].1, corners[1].0, corners[1].1]);
        let t = info.t();
        let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
        let shown = InputBand::shown(&info);
        Ok(json!({
            "name": info.description,
            "files": info.layers.len(),
            "first_file": info.first_path,
            "driver": info.driver,
            "width": info.width,
            "height": info.height,
            "crs": info.crs_name,
            "crs_authority": info.crs_authority,
            "geo_transform": info.geo_transform,
            "pixel_size": [g[1], -g[5]],
            "extent": extent,
            "extent_lonlat": ll,
            "data_type": info.data_type,
            "bands_per_date": info.bands_per_date,
            "band_names": info.band_names,
            "shown": shown.name,
            "quality_band": (info.sel.qa_band > 0).then(|| info.band_names.get(info.sel.qa_band as usize - 1).cloned()),
            "dates": t,
            "time_is_date": info.time_is_date,
            "first_date": labels.first(),
            "last_date": labels.last(),
            "date_labels": if t <= 500 { json!(labels) } else { json!(format!("{t} dates (first and last above)")) },
            "categorical": info.file_categorical,
            "classes": info.class_names,
        }))
    }

    fn pixel_series(&self, a: &Value) -> R<Value> {
        let info = self.series(&a["series"])?;
        let places = a["places"].as_array().ok_or("places: a list of places")?;
        if places.is_empty() || places.len() > 10_000 {
            return Err("from 1 to 10000 places".into());
        }
        let px: Vec<(usize, usize)> = places.iter().map(|p| place_pixel(&info, p)).collect::<R<_>>()?;
        let mut inputs = vec![InputBand::shown(&info)];
        for b in a["bands"].as_array().into_iter().flatten().filter_map(Value::as_i64) {
            if b >= 1 && b as usize <= info.bands_per_date.max(1) {
                inputs.push(InputBand::source(&info, b as i32));
            }
        }
        let s = janus_ml::data::read_points(&info, &inputs, &px);
        let (t, b) = (s.t, s.b);
        let out: Vec<Value> = px
            .iter()
            .enumerate()
            .map(|(i, &(c, r))| {
                let v = s.pixel(i);
                let mut o = json!({ "col": c, "row": r });
                for (k, inp) in inputs.iter().enumerate() {
                    o[&inp.name] = json!((0..t).map(|d| num(v[d * b + k])).collect::<Vec<_>>());
                }
                o
            })
            .collect();
        Ok(json!({
            "values": inputs.iter().map(|i| i.name.clone()).collect::<Vec<_>>(),
            "dates": info.layers.iter().map(|l| l.label.clone()).collect::<Vec<_>>(),
            "decimal_years": (0..t).map(|d| (info.decimal_year(d) * 1e4).round() / 1e4).collect::<Vec<_>>(),
            "places": out,
        }))
    }

    fn region_series(&self, a: &Value) -> R<Value> {
        let info = self.series(&a["series"])?;
        let win = window_of(&info, a)?;
        let max = a["max_pixels"].as_u64().unwrap_or(200_000).clamp(100, 5_000_000) as usize;
        let (w, h) = (win[2] - win[0], win[3] - win[1]);
        let stride = ((w * h) as f64 / max as f64).sqrt().ceil().max(1.0) as usize;
        let inputs = [InputBand::shown(&info)];
        let s = if stride == 1 {
            janus_ml::data::read_window(&info, &inputs, (win[0], win[1], w, h))
        } else {
            let px: Vec<(usize, usize)> = (win[1]..win[3])
                .step_by(stride)
                .flat_map(|r| (win[0]..win[2]).step_by(stride).map(move |c| (c, r)))
                .collect();
            janus_ml::data::read_points(&info, &inputs, &px)
        };
        let t = s.t;
        let rows: Vec<Value> = (0..t)
            .map(|d| {
                let mut v: Vec<f32> = (0..s.n).map(|i| s.values[i * t + d]).filter(|x| x.is_finite()).collect();
                v.sort_by(f32::total_cmp);
                let n = v.len();
                let q = |p: f64| if n == 0 { f32::NAN } else { v[((n - 1) as f64 * p).round() as usize] };
                let mean = if n == 0 { f64::NAN } else { v.iter().map(|&x| f64::from(x)).sum::<f64>() / n as f64 };
                let sd = if n < 2 {
                    f64::NAN
                } else {
                    (v.iter().map(|&x| (f64::from(x) - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
                };
                json!({
                    "date": info.layers[d].label,
                    "valid": n,
                    "valid_share": (n as f64 / s.n.max(1) as f64 * 1e4).round() / 1e4,
                    "mean": num(mean as f32), "std": num(sd as f32),
                    "min": num(q(0.0)), "p10": num(q(0.1)), "median": num(q(0.5)), "p90": num(q(0.9)), "max": num(q(1.0)),
                })
            })
            .collect();
        Ok(json!({
            "value": inputs[0].name,
            "window": win,
            "pixels_read": s.n,
            "sampled_every": stride,
            "dates": rows,
        }))
    }

    /// The calculator's layers (aliases given) and options.
    fn calc_layers(&self, a: &Value) -> R<(Vec<CalcLayer>, Options)> {
        let o = a["layers"].as_object().ok_or("layers: an object of series by alias")?;
        if o.is_empty() {
            return Err("name at least one layer".into());
        }
        let mut layers = vec![];
        for (alias, v) in o {
            let info = self.series(v)?;
            let name = match v {
                Value::String(p) => janus_cube::inputs::display_name(p),
                _ => alias.clone(),
            };
            layers.push(CalcLayer { name, alias: alias.clone(), info });
        }
        let opt = &a["options"];
        let index = |k: &str| -> R<Option<usize>> {
            match opt.get(k).and_then(Value::as_str) {
                None => Ok(None),
                Some(al) => layers.iter().position(|l| l.alias == al).map(Some).ok_or(format!("{k}: no layer {al:?}")),
            }
        };
        let mut opts =
            Options { dates_from: index("dates_from")?, grid_from: index("grid_from")?, ..Default::default() };
        if let Some(m) = opt.get("matching").and_then(Value::as_str) {
            opts.matching = match m.split_once(':') {
                Some(("nearest", d)) => DateMatch::Nearest(d.parse().map_err(|_| format!("matching: {m}"))?),
                _ => match m {
                    "nearest" => DateMatch::Nearest(8.0),
                    "same" | "same_day" | "exact" => DateMatch::Exact,
                    "month" => DateMatch::SameMonth,
                    "year" => DateMatch::SameYear,
                    _ => return Err(format!("matching: same, nearest[:days], month or year, not {m:?}")),
                },
            };
        }
        if let Some(r) = opt.get("resampling").and_then(Value::as_str) {
            opts.resampling = r.into();
        }
        if let Some(q) = opt.get("apply_qa").and_then(Value::as_bool) {
            opts.apply_qa = q;
        }
        Ok((layers, opts))
    }

    fn check_expression(&self, a: &Value) -> R<Value> {
        let text = a["text"].as_str().ok_or("text: the calculator's text")?;
        let (layers, opts) = self.calc_layers(a)?;
        let bands: Value = layers
            .iter()
            .map(|l| (l.alias.clone(), json!(l.info.band_names)))
            .collect::<serde_json::Map<_, _>>()
            .into();
        match plan::plan(text, &layers, &opts) {
            Ok(p) => {
                let dates = p.series.map(|_| p.dates(&layers)).unwrap_or_default();
                let g = &layers[p.grid].info;
                Ok(json!({
                    "ok": true,
                    "name": p.name,
                    "result": if p.series.is_some() { format!("a series of {} dates", dates.len()) } else { "an image".into() },
                    "first_date": dates.first().map(|d| &d.1),
                    "last_date": dates.last().map(|d| &d.1),
                    "grid": { "layer": layers[p.grid].alias, "width": g.width, "height": g.height, "crs": g.crs_name },
                    "reads": p.used.iter().map(|&l| layers[l].alias.clone()).collect::<Vec<_>>(),
                    "stages": p.stages.len(),
                    "bands": bands,
                }))
            }
            Err(e) => {
                let (line, col) = e.line_col(text);
                Ok(json!({ "ok": false, "error": e.message, "line": line, "column": col, "bands": bands }))
            }
        }
    }

    fn calculate(&self, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let text = a["text"].as_str().ok_or("text: the calculator's text")?;
        let (layers, opts) = self.calc_layers(a)?;
        let vrt = a["format"].as_str() == Some("vrt");
        let opts = Options { materialize: !vrt, ..opts };
        let p = plan::plan(text, &layers, &opts).map_err(|e| {
            let (l, col) = e.line_col(text);
            format!("line {l}, column {col}: {}", e.message)
        })?;
        let dir = out_dir(a, "calc")?;
        let split = if vrt { 1.0 } else { 0.05 };
        let vrt_dir = if vrt { dir.clone() } else { dir.join("vrt") };
        let w = p.write(text, &layers, &opts, &vrt_dir, &|f, m| c.progress(f * split, m), &c.stop)?;
        let files = if vrt {
            w.files
        } else {
            janus_cube::stac_vrt::download(&w.files, &dir, &|f, m| c.progress(split + (1.0 - split) * f, m), &c.stop)?
        };
        Ok(json!({
            "files": files,
            "folder": dir,
            "format": if vrt { "VRT (computed when read)" } else { "cloud-optimized GeoTIFF" },
            "reprojected_layers": w.warped,
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            "note": "the folder opens in Janus as a series",
        }))
    }
}

fn list_tools() -> Value {
    let tools: Vec<Value> = manifest()
        .tools
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "name": t.name,
                "category": t.category,
                "summary": t.description.split(". ").next().unwrap_or(""),
                "on": if t.input.as_deref() == Some("layers") { "maps (run_tool's maps)" } else { "a series" },
                "needs": { "time": t.requires.time, "min_dates": t.requires.min_dates, "bands": t.requires.bands },
            })
        })
        .collect();
    json!({ "count": tools.len(), "tools": tools })
}

fn describe_tool(a: &Value) -> R<Value> {
    let id = a["tool"].as_str().unwrap_or("");
    let t = manifest().tool(id).ok_or_else(|| format!("no tool {id:?} (list_tools)"))?;
    Ok(json!({
        "id": t.id,
        "name": t.name,
        "category": t.category,
        "description": t.description,
        "needs": { "time": t.requires.time, "min_dates": t.requires.min_dates, "min_per_year": t.requires.min_per_year, "bands": t.requires.bands },
        "params": t.params.iter().map(|p| json!({
            "id": p.id, "label": p.label, "type": p.kind, "default": p.default, "min": p.min, "max": p.max,
            "options": p.options, "labels": p.labels, "unit": p.unit, "help": p.help,
        })).collect::<Vec<_>>(),
        "outputs": t.outputs.iter().map(|o| json!({ "id": o.id, "name": o.name, "unit": o.unit })).collect::<Vec<_>>(),
    }))
}

/// Samples as the classification's sites: places with a class, or a vector file's features and its class field;
/// their classes in order of appearance.
fn sites_of(info: &Arc<CubeInfo>, a: &Value) -> R<(Vec<Site>, Vec<String>)> {
    let mut sites = vec![];
    let mut classes: Vec<String> = vec![];
    let mut class_of = |c: String| {
        if !classes.contains(&c) {
            classes.push(c.clone());
        }
        c
    };
    for (i, p) in a["samples"].as_array().into_iter().flatten().enumerate() {
        let class = match &p["class"] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return Err(format!("sample {}: no \"class\"", i + 1)),
        };
        let px = place_pixel(info, p)?;
        sites.push(Site {
            class: class_of(class),
            label: format!("Sample {}", i + 1),
            pixels: vec![px],
            mean: false,
            pin: None,
        });
    }
    if let Some(vector) = a["vector"].as_str() {
        let field = a["field"].as_str().ok_or("field: the vector's class field")?;
        let max = a["max_pixels"].as_u64().unwrap_or(50).max(1) as usize;
        for (k, (class, pixels)) in vector_samples(info, vector, field, max)?.into_iter().enumerate() {
            sites.push(Site {
                class: class_of(class),
                label: format!("Feature {}", k + 1),
                pixels,
                mean: false,
                pin: None,
            });
        }
    }
    if sites.is_empty() {
        return Err("give samples (places with a class) or a vector file and its class field".into());
    }
    Ok((sites, classes))
}

/// A feature's class and its pixels.
type Labelled = (String, Vec<(usize, usize)>);

/// A vector file's features as samples: each point's pixel, each polygon's pixels inside it (up to `max`, spread
/// evenly), with the class field's value.
fn vector_samples(info: &CubeInfo, path: &str, field: &str, max: usize) -> R<Vec<Labelled>> {
    use gdal::vector::LayerAccess;
    let ds = gdal::Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut layer = ds.layer(0).map_err(|e| format!("{path}: {e}"))?;
    let fi = layer.defn().field_index(field).map_err(|e| format!("{path}: field {field:?}: {e}"))?;
    let mut crs = SpatialRef::from_wkt(&info.crs_wkt).map_err(|e| e.to_string())?;
    crs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = match layer.spatial_ref() {
        Some(mut s) => {
            s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
            Some(CoordTransform::new(&s, &crs).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let g = gt_of(info);
    let mut out = vec![];
    for f in layer.features() {
        let Some(class) = f.field_as_string(fi).ok().flatten().filter(|s| !s.is_empty()) else { continue };
        let Some(geom) = f.geometry().filter(|g| !g.is_empty()) else { continue };
        let geom = match &ct {
            Some(ct) => geom.transform(ct).map_err(|e| e.to_string())?,
            None => geom.clone(),
        };
        let env = geom.envelope();
        let to_px = |x: f64, y: f64| ((x - g[0]) / g[1], (y - g[3]) / g[5]);
        let (c0, r0) = to_px(env.MinX, env.MaxY);
        let (c1, r1) = to_px(env.MaxX, env.MinY);
        let inside = |c: f64, r: f64| c >= 0.0 && r >= 0.0 && c < info.width as f64 && r < info.height as f64;
        let mut px = vec![];
        if geom.geometry_type() == gdal_sys::OGRwkbGeometryType::wkbPoint || (c1 - c0) < 1.0 && (r1 - r0) < 1.0 {
            let (c, r) = to_px((env.MinX + env.MaxX) / 2.0, (env.MinY + env.MaxY) / 2.0);
            if inside(c, r) {
                px.push((c as usize, r as usize));
            }
        } else {
            let (c0, c1) = (c0.min(c1).floor().max(0.0) as usize, c0.max(c1).ceil().min(info.width as f64) as usize);
            let (r0, r1) = (r0.min(r1).floor().max(0.0) as usize, r0.max(r1).ceil().min(info.height as f64) as usize);
            let cells = (c1.saturating_sub(c0)) * (r1.saturating_sub(r0));
            let step = ((cells as f64 / (max * 2) as f64).sqrt().floor() as usize).max(1);
            'rows: for r in (r0..r1).step_by(step) {
                for c in (c0..c1).step_by(step) {
                    let (x, y) = (g[0] + (c as f64 + 0.5) * g[1], g[3] + (r as f64 + 0.5) * g[5]);
                    let mut p = gdal::vector::Geometry::empty(gdal_sys::OGRwkbGeometryType::wkbPoint)
                        .map_err(|e| e.to_string())?;
                    p.add_point_2d((x, y));
                    if geom.contains(&p) {
                        px.push((c, r));
                        if px.len() >= max {
                            break 'rows;
                        }
                    }
                }
            }
        }
        if !px.is_empty() {
            out.push((class, px));
        }
    }
    Ok(out)
}

impl Server {
    fn run_tool(&self, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let id = a["tool"].as_str().unwrap_or("");
        let tool = manifest().tool(id).ok_or_else(|| format!("no tool {id:?} (list_tools)"))?;
        if tool.input.as_deref() == Some("layers") {
            return self.run_on_maps(tool, a, c);
        }
        let info = self.series(&a["series"])?;
        if let Some(why) = crate::zeit::applicability(tool, &info) {
            return Err(format!("{} cannot run on this series: {why}", tool.name));
        }
        let mut params: BTreeMap<String, Value> = a["params"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        if let Some(bad) = params.keys().find(|k| !tool.params.iter().any(|p| &p.id == *k)) {
            return Err(format!("{} has no parameter {bad:?} (describe_tool)", tool.name));
        }
        if id == "twdtw" && !params.contains_key("patterns") {
            params.insert("patterns".into(), twdtw_patterns(&info, a)?);
        }
        let mut roles = crate::zeit::guess_roles(&info);
        for (k, v) in a["roles"].as_object().into_iter().flatten() {
            roles.insert(k.clone(), v.as_i64().ok_or(format!("roles: {k} is a band number"))? as i32);
        }
        let dir = out_dir(a, id)?;
        let mut spec = crate::zeit::base_spec(&info, tool, &params, info.t().saturating_sub(1), self.threads);
        crate::zeit::series_inputs(&info, tool, &roles, &dir.join("series"), &mut spec)?;
        spec.window = Some(window_of(&info, a)?);
        spec.output_dir = dir.to_string_lossy().into_owned();
        let _ = std::fs::write(dir.join("job.json"), serde_json::to_string_pretty(&spec).unwrap_or_default());
        let r = janus_zeit::job::run_job(&spec, &|f, m| c.progress(f, m), &c.stop)?;
        Ok(json!({
            "tool": tool.name,
            "window": r.window,
            "outputs": r.outputs.iter().map(|o| json!({
                "name": o["name"].as_str().or(o["id"].as_str()),
                "path": o["path"],
                "unit": o["unit"],
                "classes": o.get("classes"),
            })).collect::<Vec<_>>(),
            "folder": dir,
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        }))
    }

    fn regularize(&self, a: &Value, c: &Call) -> R<Value> {
        use janus_zeit::regularize::{Composite, Fill, RegOptions, Step};
        let t0 = Instant::now();
        let info = self.series(&a["series"])?;
        let step = match &a["step"] {
            Value::Number(n) => Step::Days(n.as_u64().ok_or("step: a number of days")? as u32),
            Value::String(s) => match s.trim().to_lowercase().as_str() {
                "month" | "monthly" | "ms" => Step::Month,
                "year" | "yearly" | "ys" => Step::Year,
                d => Step::Days(d.trim_end_matches('d').parse().map_err(|_| format!("step: {s:?}"))?),
            },
            _ => return Err("step: \"16d\", \"month\", \"year\" or a number of days".into()),
        };
        let composite = match a["composite"].as_str().unwrap_or("median") {
            "median" => Composite::Median,
            "medoid" => Composite::Medoid,
            "clearest" => Composite::Clearest,
            o => return Err(format!("composite: median, medoid or clearest, not {o:?}")),
        };
        let fill = match a["fill"].as_str().unwrap_or("none") {
            "none" => Fill::None,
            "linear" => Fill::Linear,
            f => match f.split_once(':') {
                Some(("whittaker", l)) => Fill::Whittaker(l.parse().map_err(|_| format!("fill: {f}"))?),
                _ if f == "whittaker" => Fill::Whittaker(10.0),
                _ => return Err(format!("fill: none, linear or whittaker:LAMBDA, not {f:?}")),
            },
        };
        let dir = out_dir(a, "regularize")?;
        let opts = RegOptions {
            step,
            composite,
            fill,
            max_gap_days: a["max_gap_days"].as_f64().unwrap_or(0.0),
            all_bands: a["all_bands"].as_bool().unwrap_or(false),
            window: Some(window_of(&info, a)?),
            stem: janus_cube::stac_vrt::sanitize(&series_stem(&info)),
            threads: self.threads,
        };
        let r = janus_zeit::regularize::regularize(&info, &opts, &dir, &|f, m| c.progress(f, m), &c.stop)?;
        Ok(json!({
            "files": r.files,
            "periods": r.periods.len(),
            "empty_periods": r.empty,
            "folder": dir,
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            "note": "the folder opens in Janus (describe_series) as the regular series",
        }))
    }

    fn warp(&self, a: &Value, c: &Call) -> R<Value> {
        use janus_zeit::warp::{Format, WarpOptions, warp};
        let src = a["src"].as_str().ok_or("src: a raster")?;
        let format = match a["format"].as_str().unwrap_or("cog") {
            "cog" => Format::Cog,
            "gtiff" => Format::GTiff,
            "vrt" => Format::Vrt,
            f => return Err(format!("format: cog, gtiff or vrt, not {f:?}")),
        };
        let dst = match a["dst"].as_str() {
            Some(d) => PathBuf::from(d),
            None => out_dir(a, "warp")?.join(format!(
                "{}.{}",
                Path::new(src).file_stem().map_or("warped".into(), |s| s.to_string_lossy()),
                format.extension()
            )),
        };
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let t_srs = a["t_srs"].as_str().map(String::from);
        let extent = a["extent"]
            .as_array()
            .map(|v| v.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
            .filter(|v| v.len() == 4)
            .map(|v| {
                let srs = a["extent_srs"].as_str().map(String::from).or(t_srs.clone()).unwrap_or_default();
                ([v[0], v[1], v[2], v[3]], srs)
            });
        let opts = WarpOptions {
            t_srs,
            pixel: a["pixel"].as_f64().map(|p| (p, p)),
            extent,
            cutline: a["cutline"].as_str().map(|v| (v.to_string(), String::new())),
            resampling: a["resampling"].as_str().unwrap_or("near").into(),
            format,
            threads: self.threads,
            ..Default::default()
        };
        warp(src, &dst.to_string_lossy(), &opts, &|f| {
            c.progress(f, "warping");
            !c.stop.load(Ordering::Relaxed)
        })?;
        Ok(json!({ "file": dst }))
    }

    fn zonal(&self, a: &Value, c: &Call) -> R<Value> {
        use janus_zeit::zonal::{STATS, ZonalKind, ZonalOptions, zonal};
        let vector = a["vector"].as_str().ok_or("vector: a vector file")?;
        let maps: Vec<String> = a["maps"]
            .as_array()
            .map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if maps.is_empty() {
            return Err("maps: one raster or more".into());
        }
        let first = gdal::Dataset::open(&maps[0]).map_err(|e| format!("{}: {e}", maps[0]))?;
        let (w, h) = first.raster_size();
        let grid = janus_zeit::warp::Grid {
            wkt: first.projection(),
            gt: first.geo_transform().map_err(|e| format!("{}: {e}", maps[0]))?,
            w,
            h,
        };
        let sources = map_sources(&maps)?;
        let kind = match a["kind"].as_str().unwrap_or("continuous") {
            "continuous" => ZonalKind::Continuous,
            "classes" => ZonalKind::Classes,
            "years" => ZonalKind::Years,
            k => return Err(format!("kind: continuous, classes or years, not {k:?}")),
        };
        let opts = ZonalOptions {
            layer: a["layer"].as_str().unwrap_or("").into(),
            id_field: a["id_field"].as_str().unwrap_or("").into(),
            all_touched: a["all_touched"].as_bool().unwrap_or(false),
            kind,
            threads: self.threads,
            ..Default::default()
        };
        let t = zonal(vector, &grid, &sources, &opts, &|f, m| c.progress(f, m), &c.stop)?;
        let names = |v: i64| v.to_string();
        let dir = out_dir(a, "zonal")?;
        let csv = dir.join("zonal.csv");
        std::fs::write(&csv, t.long_csv(&names)).map_err(|e| format!("{}: {e}", csv.display()))?;
        // the answer: the first map's table (at most 200 zones)
        let rows: Vec<Value> = match kind {
            ZonalKind::Continuous => (0..t.ids.len().min(200))
                .map(|z| {
                    let s = &t.stats[0][z];
                    let mut o = json!({ "zone": t.ids[z], "pixels": t.pixels[z] });
                    for (k, name) in STATS.iter().enumerate() {
                        o[*name] = num(s[k] as f32);
                    }
                    o
                })
                .collect(),
            _ => t.counts[0]
                .iter()
                .take(2000)
                .map(|&(z, v, px, m2)| json!({ "zone": t.ids[z as usize], "value": v, "pixels": px, "hectares": (m2 / 1e4 * 1e4).round() / 1e4 }))
                .collect(),
        };
        Ok(json!({
            "zones": t.ids.len(),
            "maps": t.labels,
            "csv": csv,
            "first_map": rows,
            "note": t.note,
        }))
    }

    fn train_classifier(&self, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let info = self.series(&a["series"])?;
        let (sites, classes) = sites_of(&info, a)?;
        if classes.len() < 2 {
            return Err(format!("two classes at least (got {classes:?})"));
        }
        let mut inputs = vec![InputBand::shown(&info)];
        for b in a["bands"].as_array().into_iter().flatten().filter_map(Value::as_i64) {
            if b >= 1 && b as usize <= info.bands_per_date.max(1) {
                inputs.push(InputBand::source(&info, b as i32));
            }
        }
        let min_valid = a["min_valid"].as_f64().unwrap_or(50.0).clamp(1.0, 100.0);
        c.progress(0.01, "reading the samples");
        let samples = crate::ml::sample_series(&info, &inputs, &sites, &classes, min_valid);
        if samples.0.n < 10 {
            return Err(format!("{} samples with enough dates: ten at least", samples.0.n));
        }
        let per_class = samples.3.clone();
        let method = a["method"].as_str().unwrap_or("random_forest");
        let arch = match method {
            "random_forest" | "forest" | "rf" => janus_ml::Arch::RandomForest(ForestHyper {
                trees: a["trees"].as_u64().unwrap_or(500).clamp(1, 5000) as usize,
                ..Default::default()
            }),
            "tempcnn" => janus_ml::Arch::TempCnn(Default::default()),
            "ltae" | "l-tae" | "lighttae" => janus_ml::Arch::LTae(Default::default()),
            m => return Err(format!("method: random_forest, tempcnn or ltae, not {m:?}")),
        };
        let mut cfg = TrainConfig {
            validation_split: a["validation_share"].as_f64().unwrap_or(0.25).clamp(0.0, 0.9),
            seed: a["seed"].as_u64().unwrap_or(42),
            ..TrainConfig::for_arch(&arch)
        };
        if let Some(e) = a["epochs"].as_u64() {
            cfg.epochs = (e as usize).clamp(1, 2000);
        }
        let stem = janus_cube::stac_vrt::sanitize(&arch.name().to_lowercase());
        let model_path = match a["output"].as_str() {
            Some(p) => PathBuf::from(p),
            None => out_dir(a, &stem)?.join(format!("{stem}.janusmodel")),
        };
        if let Some(p) = model_path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        let log = crate::zeit::JobLog { path: model_path.with_extension("log"), t0: Instant::now() };
        let run = Arc::new(Mutex::new(crate::ml::TrainRun::default()));
        let header = match &arch {
            janus_ml::Arch::RandomForest(h) => crate::ml::forest_job(
                &info,
                inputs,
                &classes,
                h,
                &cfg,
                min_valid,
                samples,
                Calendar::of(&info),
                &model_path.to_string_lossy(),
                &log,
                &run,
                &|f, m| c.progress(f, m),
                &c.stop,
            )?,
            _ => crate::ml::train_job(
                &info,
                &inputs,
                &sites,
                &classes,
                &arch,
                &cfg,
                min_valid,
                a["gpu"].as_bool().unwrap_or(true),
                &model_path.to_string_lossy(),
                &log,
                &run,
                &|f, m| c.progress(f, m),
                &c.stop,
            )?,
        };
        let tr = &header.training;
        let v = &tr["validation"];
        let imp: Vec<f64> =
            tr["importance"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        let b = header.inputs.len().max(1);
        let mut by_date: Vec<(usize, f64)> = (0..header.calendar.len())
            .map(|t| (t, imp.get(t * b..(t + 1) * b).map_or(0.0, |s| s.iter().sum())))
            .collect();
        by_date.sort_by(|x, y| y.1.total_cmp(&x.1));
        Ok(json!({
            "model": model_path,
            "classes": classes.iter().zip(&per_class).map(|(c, n)| json!({ "class": c, "samples": n })).collect::<Vec<_>>(),
            "validation": {
                "overall_accuracy": v["overall_accuracy"],
                "samples": tr["samples"]["validation"],
                "held_out": tr["samples"]["split"],
                "confusion": v["confusion"],
                "producers_accuracy": v["producers_accuracy"],
                "users_accuracy": v["users_accuracy"],
            },
            "method": header.arch.name(),
            "out_of_bag_accuracy": tr["oob"]["overall_accuracy"],
            "best_epoch": tr.get("best_epoch"),
            "note": if v["overall_accuracy"].as_f64().is_some_and(f64::is_finite) {
                "validated on samples the model never saw (whole places for the forest)"
            } else {
                "too few places to hold some out: the out-of-bag accuracy is the guide (give more places per class)"
            },
            "most_important_dates": by_date.iter().take(8).map(|&(t, w)| json!({ "date": header.calendar.labels[t], "importance": (w * 1e4).round() / 1e4 })).collect::<Vec<_>>(),
            "area_of_applicability": header.applicability.is_some(),
            "log": log.path,
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        }))
    }

    fn classify(&self, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let path = a["model"].as_str().ok_or("model: a .janusmodel file")?;
        let info = self.series(&a["series"])?;
        let device = janus_ml::model::shared_device(false);
        let model = janus_ml::Model::load(path, &device.0)?;
        let dir = out_dir(a, "classify")?;
        let spec = janus_ml::apply::ApplySpec {
            window: window_of(&info, a)?,
            output_dir: dir.to_string_lossy().into_owned(),
            stem: janus_cube::stac_vrt::sanitize(&model.header.arch.name().to_lowercase()),
            aoa: a["aoa"].as_bool().unwrap_or(true) && model.header.applicability.is_some(),
            ..Default::default()
        };
        let r = janus_ml::apply::apply(&spec, &info, &model, &|f, m| c.progress(f, m), &c.stop)?;
        Ok(json!({
            "classes": model.header.classes.iter().enumerate().map(|(k, c)| json!({ "value": k + 1, "class": c.name })).collect::<Vec<_>>(),
            "outputs": r.outputs.iter().map(|o| json!({ "name": o["name"], "path": o["path"] })).collect::<Vec<_>>(),
            "window": r.window,
            "pixels_classified": r.classified,
            "folder": dir,
            "files": files_of(&dir),
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        }))
    }
}

/// A series' name for its files: its folder's (a file a date), else its file's.
fn series_stem(info: &CubeInfo) -> String {
    let p = Path::new(&info.first_path);
    let of = |q: Option<&std::ffi::OsStr>| q.map(|s| s.to_string_lossy().into_owned());
    (if info.layers.len() > 1 { of(p.parent().and_then(Path::file_name)) } else { of(p.file_stem()) })
        .unwrap_or_else(|| "series".into())
}

/// Opens files (series, maps, vector layers) in a window of Janus's of its own.
fn open_in_janus(a: &Value) -> R<Value> {
    let paths: Vec<String> = a["paths"]
        .as_array()
        .map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if paths.is_empty() {
        return Err("paths: the files or folders to open".into());
    }
    let want = a["window"].as_str().unwrap_or("auto");
    if want == "open" || want == "auto" && crate::inbox::watched() {
        if !crate::inbox::watched() {
            return Err("no window of Janus is open (window new opens one)".into());
        }
        crate::inbox::post(&json!({ "paths": paths, "expression": a["expression"], "places": a["places"] }))?;
        return Ok(json!({ "opened": paths, "in": "the window already open, as layers" }));
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(&exe);
    if let Some(e) = a["expression"].as_str() {
        cmd.args(["--expr", e]);
    }
    for p in a["places"].as_array().into_iter().flatten() {
        let (Some(c), Some(r)) = (p["col"].as_f64(), p["row"].as_f64()) else { continue };
        let class = p["class"].as_str().map_or(String::new(), |c| format!(",{c}"));
        cmd.args(["--pin", &format!("{},{}{class}", c as usize, r as usize)]);
    }
    cmd.args(&paths);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    Ok(json!({ "opened": paths, "in": "a new window" }))
}

/// TWDTW's patterns from samples: each class's mean series of the shown value at its places.
fn twdtw_patterns(info: &Arc<CubeInfo>, a: &Value) -> R<Value> {
    if a["samples"].as_array().is_none_or(Vec::is_empty) {
        return Err("TWDTW needs samples (places with a class) or params.patterns".into());
    }
    let (sites, classes) = sites_of(info, a)?;
    let shown = [InputBand::shown(info)];
    let pixels: Vec<(usize, usize)> = sites.iter().flat_map(|s| s.pixels.iter().copied()).collect();
    let raw = janus_ml::data::read_points(info, &shown, &pixels);
    let t = raw.t;
    // each pixel's class
    let of: Vec<&str> = sites.iter().flat_map(|s| std::iter::repeat_n(s.class.as_str(), s.pixels.len())).collect();
    let mut out = vec![];
    for cl in &classes {
        let idx: Vec<usize> = (0..of.len()).filter(|&i| of[i] == cl).collect();
        let values: Vec<Value> = (0..t)
            .map(|d| {
                let v: Vec<f64> =
                    idx.iter().map(|&i| f64::from(raw.values[i * t + d])).filter(|x| x.is_finite()).collect();
                if v.is_empty() { Value::Null } else { json!(v.iter().sum::<f64>() / v.len() as f64) }
            })
            .collect();
        out.push(json!({
            "name": cl,
            "from": format!("mean of {} samples", idx.len()),
            "years": (0..t).map(|d| info.decimal_year(d)).collect::<Vec<_>>(),
            "days": if info.time_is_date { json!((0..t).map(|d| info.unix_day(d)).collect::<Vec<_>>()) } else { Value::Null },
            "values": values,
        }));
    }
    Ok(Value::Array(out))
}

/// Rasters as maps to count: band 1 of each, or every band of one file (a band a date).
fn map_sources(maps: &[String]) -> R<Vec<janus_zeit::areas::AreaSource>> {
    use gdal::Metadata;
    use janus_zeit::areas::AreaSource;
    let first = gdal::Dataset::open(&maps[0]).map_err(|e| format!("{}: {e}", maps[0]))?;
    Ok(if maps.len() == 1 && first.raster_count() > 1 {
        (1..=first.raster_count())
            .map(|b| {
                let label = first
                    .rasterband(b)
                    .map(|r| r.description().unwrap_or_default())
                    .ok()
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| format!("band {b}"));
                AreaSource { path: maps[0].clone(), band: b, label }
            })
            .collect()
    } else {
        maps.iter()
            .map(|m| AreaSource { path: m.clone(), band: 1, label: janus_cube::inputs::display_name(m) })
            .collect()
    })
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// A file in the call's results folder unless `dst` names one.
fn dst_of(a: &Value, tool: &str, name: &str) -> R<PathBuf> {
    let p = match a["dst"].as_str() {
        Some(d) => PathBuf::from(d),
        None => out_dir(a, tool)?.join(name),
    };
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    Ok(p)
}

impl Server {
    /// A Zeit tool on maps (agreement...): the maps as its layers parameter, on the first's grid.
    fn run_on_maps(&self, tool: &janus_zeit::Tool, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let maps = strings(&a["maps"]);
        if maps.len() < 2 {
            return Err(format!("{} runs on two maps or more: give them as maps", tool.name));
        }
        let lp = tool.params.iter().find(|p| p.kind == "layers").ok_or("no maps parameter")?;
        let mut params: BTreeMap<String, Value> = a["params"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        params.insert(
            lp.id.clone(),
            json!(
                maps.iter()
                    .map(|m| json!({ "path": m, "name": janus_cube::inputs::display_name(m) }))
                    .collect::<Vec<_>>()
            ),
        );
        let dir = out_dir(a, &tool.id)?;
        let spec = janus_zeit::job::JobSpec {
            tool: tool.id.clone(),
            params,
            output_dir: dir.to_string_lossy().into_owned(),
            threads: Some(self.threads as i64),
            ..Default::default()
        };
        let r = janus_zeit::job::run_job(&spec, &|f, m| c.progress(f, m), &c.stop)?;
        Ok(json!({
            "tool": tool.name,
            "outputs": r.outputs.iter().map(|o| json!({ "name": o["name"].as_str().or(o["id"].as_str()), "path": o["path"] })).collect::<Vec<_>>(),
            "folder": dir,
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        }))
    }

    fn polygonize(&self, a: &Value, c: &Call) -> R<Value> {
        let src = a["src"].as_str().ok_or("src: a raster")?;
        let stem = Path::new(src).file_stem().map_or("polygons".into(), |s| s.to_string_lossy().into_owned());
        let dst = dst_of(a, "polygonize", &format!("{stem}.gpkg"))?;
        let opts = janus_zeit::polygonize::PolyOptions {
            band: a["band"].as_u64().unwrap_or(1).max(1) as usize,
            eight: a["eight"].as_bool().unwrap_or(false),
            min_area_ha: a["min_area_ha"].as_f64().unwrap_or(0.0),
            simplify_m: a["simplify_m"].as_f64().unwrap_or(0.0),
            years: a["years"].as_bool().unwrap_or(false),
            layer: janus_cube::stac_vrt::sanitize(&stem),
            ..Default::default()
        };
        let r = janus_zeit::polygonize::polygonize(src, &opts, &dst, &|f, m| c.progress(f, m), &c.stop)?;
        Ok(json!({
            "file": dst,
            "polygons": r.polygons,
            "area_ha": (r.area_ha * 100.0).round() / 100.0,
            "patches_dropped": r.dropped,
        }))
    }

    fn rasterize(&self, a: &Value, c: &Call) -> R<Value> {
        use janus_zeit::rasterize::{Burn, RasterizeOptions, rasterize};
        let vector = a["vector"].as_str().ok_or("vector: a vector file")?;
        // the grid: a raster's, else a series'
        let grid = match a["like"].as_str().map(gdal::Dataset::open) {
            Some(Ok(ds)) if ds.raster_count() > 0 => {
                let (w, h) = ds.raster_size();
                janus_zeit::warp::Grid {
                    wkt: ds.projection(),
                    gt: ds.geo_transform().map_err(|e| e.to_string())?,
                    w,
                    h,
                }
            }
            _ => {
                let info = self.series(&a["like"])?;
                janus_zeit::warp::Grid { wkt: info.crs_wkt.clone(), gt: gt_of(&info), w: info.width, h: info.height }
            }
        };
        let burn = match a["burn"].as_str().unwrap_or("one") {
            "one" => Burn::One,
            "index" => Burn::Index,
            f => Burn::Attribute(f.to_string()),
        };
        let stem = Path::new(vector).file_stem().map_or("rasterized".into(), |s| s.to_string_lossy().into_owned());
        let dst = dst_of(a, "rasterize", &format!("{stem}.tif"))?;
        let opts = RasterizeOptions {
            layer: a["layer"].as_str().unwrap_or("").into(),
            burn,
            all_touched: a["all_touched"].as_bool().unwrap_or(false),
            grid,
        };
        let r = rasterize(vector, &opts, &dst, &|f, m| c.progress(f, m), &c.stop)?;
        Ok(json!({
            "file": dst,
            "kind": r.kind,
            "burned": r.burned,
            "features": r.features,
            "classes": r.classes.iter().map(|(v, t)| json!({ "value": v, "class": t })).collect::<Vec<_>>(),
        }))
    }

    fn speckle(&self, a: &Value, c: &Call) -> R<Value> {
        use janus_zeit::speckle::{Filter, SpeckleOptions, speckle};
        let t0 = Instant::now();
        let info = self.series(&a["series"])?;
        let f = a["filter"].as_str().unwrap_or("quegan").to_lowercase();
        let filter = match f.split_once(':') {
            Some(("lee", l)) => Filter::Lee { looks: l.parse().map_err(|_| format!("filter: {f}"))? },
            _ if f == "lee" => Filter::Lee { looks: 4.4 },
            _ if f == "quegan" => Filter::Quegan,
            _ => return Err(format!("filter: quegan, lee or lee:LOOKS, not {f:?}")),
        };
        let dir = out_dir(a, "speckle")?;
        let opts = SpeckleOptions {
            filter,
            size: a["size"].as_u64().unwrap_or(7).clamp(3, 51) as usize,
            window: Some(window_of(&info, a)?),
            stem: janus_cube::stac_vrt::sanitize(&series_stem(&info)),
            threads: self.threads,
        };
        let files = speckle(&info, &opts, &dir, &|f, m| c.progress(f, m), &c.stop)?;
        Ok(json!({
            "files": files,
            "folder": dir,
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            "note": "the folder opens in Janus (describe_series) as the filtered series",
        }))
    }

    fn areas(&self, a: &Value, c: &Call) -> R<Value> {
        use janus_zeit::areas::{AreaOptions, areas};
        let maps = strings(&a["maps"]);
        if maps.is_empty() {
            return Err("maps: one raster or more".into());
        }
        let sources = map_sources(&maps)?;
        let window = a["window"]
            .as_array()
            .map(|v| v.iter().filter_map(Value::as_u64).map(|x| x as usize).collect::<Vec<_>>())
            .filter(|v| v.len() == 4)
            .map(|v| [v[0], v[1], v[2], v[3]]);
        let opts = AreaOptions { window, floor: a["floor"].as_bool().unwrap_or(false), threads: self.threads };
        let t = areas(&sources, &opts, &|f, m| c.progress(f, m), &c.stop)?;
        let dir = out_dir(a, "areas")?;
        let csv = dir.join("areas.csv");
        std::fs::write(&csv, t.csv(&|v| v.to_string())).map_err(|e| format!("{}: {e}", csv.display()))?;
        let maps: Vec<Value> = t
            .labels
            .iter()
            .enumerate()
            .take(200)
            .map(|(s, label)| {
                let total = t.total(s);
                let values: Vec<Value> = t
                    .values
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| t.pixels[s][*k] > 0)
                    .map(|(k, v)| {
                        json!({
                            "value": v,
                            "pixels": t.pixels[s][k],
                            "hectares": (t.m2[s][k] / 1e4 * 100.0).round() / 100.0,
                            "percent": if total > 0.0 { (1e4 * t.m2[s][k] / total).round() / 100.0 } else { 0.0 },
                        })
                    })
                    .collect();
                json!({ "map": label, "values": values })
            })
            .collect();
        Ok(json!({ "csv": csv, "maps": maps, "note": t.note }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A series of 3 dates of 4 x 3 pixels (value = date * 10 + col), UTM 20S.
    fn series(dir: &Path) -> String {
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut srs = SpatialRef::from_epsg(32720).unwrap();
        srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        for (k, day) in ["2020-01-15", "2020-02-15", "2020-03-15"].iter().enumerate() {
            let p = dir.join(format!("{day}.tif"));
            let mut d = drv.create_with_band_type::<f32, _>(&p, 4, 3, 1).unwrap();
            d.set_geo_transform(&[500000.0, 10.0, 0.0, 9000000.0, 0.0, -10.0]).unwrap();
            d.set_spatial_ref(&srs).unwrap();
            let v: Vec<f32> = (0..12).map(|i| (k * 10 + i % 4) as f32).collect();
            let mut b = d.rasterband(1).unwrap();
            b.write((0, 0), (4, 3), &mut gdal::raster::Buffer::new((4, 3), v)).unwrap();
        }
        dir.to_string_lossy().into_owned()
    }

    fn quiet() -> Call {
        Call {
            out: Arc::new(Mutex::new(std::io::stdout())),
            token: Value::Null,
            sent: Mutex::new((0, Instant::now())),
            stop: Arc::default(),
        }
    }

    #[test]
    fn speaks_the_protocol() {
        let s = Server { threads: 2, ..Default::default() };
        let init = s
            .answer(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }))
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(init["result"]["serverInfo"]["name"], "janus");
        // a notification is not answered; an unknown method is an error
        assert!(s.answer(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
        assert_eq!(s.answer(&json!({ "jsonrpc": "2.0", "id": 2, "method": "nope" })).unwrap()["error"]["code"], -32601);
        let tools = catalogue();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        for t in &tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            for r in t["inputSchema"]["required"].as_array().unwrap() {
                assert!(t["inputSchema"]["properties"].get(r.as_str().unwrap()).is_some(), "{}: {r}", t["name"]);
            }
        }
        // every tool answers (an error for missing arguments, not a panic)
        let c = quiet();
        for n in names {
            let _ = s.call(&json!({ "name": n, "arguments": {} }), &c);
        }
    }

    #[test]
    fn reads_and_computes_a_series() {
        // PROJ's data, as Janus finds it
        crate::configure_data();
        gdal::DriverManager::register_all();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("janus-mcp-test-{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = series(&dir);
        let s = Server { threads: 2, ..Default::default() };
        let c = quiet();
        let d = s.describe_series(&json!({ "series": path })).unwrap();
        assert_eq!((d["width"].as_u64(), d["height"].as_u64(), d["dates"].as_u64()), (Some(4), Some(3), Some(3)));
        assert_eq!(d["first_date"], "2020-01-15");
        // a pixel by its col and row, and by its coordinates (col 2, row 1's centre)
        let p = s
            .pixel_series(
                &json!({ "series": path, "places": [{ "col": 3, "row": 0 }, { "x": 500025.0, "y": 8999985.0 }] }),
            )
            .unwrap();
        let v = |i: usize| {
            p["places"][i].as_object().unwrap().iter().find(|(k, _)| *k != "col" && *k != "row").unwrap().1.clone()
        };
        assert_eq!(v(0), json!([3.0, 13.0, 23.0]));
        assert_eq!(v(1), json!([2.0, 12.0, 22.0]));
        assert!(s.pixel_series(&json!({ "series": path, "places": [{ "col": 9, "row": 0 }] })).is_err());
        let r = s.region_series(&json!({ "series": path, "window": [0, 0, 2, 3] })).unwrap();
        assert_eq!(r["dates"][1]["mean"], json!(10.5));
        assert_eq!(r["dates"][1]["valid"], 6);
        // the calculator: checked (an error's place), then written
        let bad = s.check_expression(&json!({ "text": "m = max(a", "layers": { "a": path } })).unwrap();
        assert_eq!((bad["ok"].as_bool(), bad["line"].as_u64()), (Some(false), Some(1)));
        let ok = s.check_expression(&json!({ "text": "m = max(a) - min(a)", "layers": { "a": path } })).unwrap();
        assert_eq!(ok["result"], "an image");
        let out = dir.join("calc");
        let w = s
            .calculate(&json!({ "text": "m = max(a) - min(a)", "layers": { "a": path }, "output_dir": out }), &c)
            .unwrap();
        let f = w["files"][0].as_str().unwrap();
        let ds = gdal::Dataset::open(f).unwrap();
        let px = ds.rasterband(1).unwrap().read_as::<f32>((0, 0), (4, 3), (4, 3), None).unwrap();
        assert!(px.data().iter().all(|&x| (x - 20.0).abs() < 1e-6), "{:?}", px.data());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
