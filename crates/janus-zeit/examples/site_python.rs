//! The code of the website's "In Python" tabs (site/data/python.json), made by `janus_zeit::python::pixel_script`, the
//! same function as Copy Python of the chart's menu:
//!
//!     site_python CASES.json [OUT.json]
//!
//! (the JSON goes to OUT.json, in UTF-8 without a BOM, if one is given, else to the standard output)
//!
//! CASES.json (tools/python_snippets.json) lists cases `{guide, tool, params, input, steps}`; the output is
//! `{guide: {tool, code, steps, pixel}}`. Each case opens its series with `janus_cube::open_cube` (the quality band
//! found by its name), takes the tool's parameters (its defaults with `params` over them) and writes the script of
//! a pin.
//!
//! `input`: `open` (the files or folder to open; `{NAME}` is the environment variable NAME), `show` (the folder the
//! script writes before each file's name, "" for the current one), `band`/`nd_band` (the Display panel's), `pixel`
//! [column, row] or `lonlat` [x, y] of the pin (the pixel that holds the point, as a click on the map gives),
//! `name` (the layer's name, by default the opened file's or folder's name).

use std::collections::BTreeMap;
use std::path::Path;

use janus_cube::BandSelection;
use janus_zeit::Params;
use janus_zeit::python::{Spec, Target, pixel_script};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    guide: String,
    tool: String,
    #[serde(default)]
    params: BTreeMap<String, Value>,
    #[serde(default)]
    steps: Vec<String>,
    input: Input,
}

#[derive(Deserialize)]
struct Input {
    open: Vec<String>,
    #[serde(default)]
    show: String,
    #[serde(default = "one")]
    band: i32,
    #[serde(default)]
    nd_band: i32,
    pixel: Option<[i64; 2]>,
    lonlat: Option<[f64; 2]>,
    name: Option<String>,
}

fn one<T: From<u8>>() -> T {
    T::from(1)
}

/// `{NAME}` in a path: the environment variable NAME (where the guide's data is on this machine).
fn expand(path: &str) -> String {
    let mut out = path.to_string();
    while let (Some(a), Some(b)) = (out.find('{'), out.find('}')) {
        let name = out[a + 1..b].to_string();
        let value = std::env::var(&name).unwrap_or_else(|_| panic!("set the environment variable {name}"));
        out.replace_range(a..=b, &value);
    }
    out
}

fn run(case: &Case) -> Result<Value, String> {
    let i = &case.input;
    let sel = BandSelection { band: i.band, nd_band: i.nd_band, ..Default::default() };
    let open: Vec<String> = i.open.iter().map(|p| expand(p)).collect();
    let info = janus_cube::open_cube(&open, &sel)?;
    let (col, row) = match (i.pixel, i.lonlat) {
        (Some([c, r]), _) => (c, r),
        (None, Some([x, y])) => {
            let (px, py) = info.geo_to_pixel(x, y).ok_or("the series has no geotransform")?;
            (px.floor() as i64, py.floor() as i64)
        }
        _ => return Err("the pin needs `pixel` or `lonlat`".into()),
    };
    let roles: BTreeMap<String, i32> =
        janus_cube::roles::guess_band_roles(&info).into_iter().map(|(r, b)| (r.to_string(), b)).collect();
    let tool = (case.tool.clone(), Params::with_defaults(&case.tool, &case.params).ok_or("unknown tool")?);
    let first = &open[0];
    let name = i
        .name
        .clone()
        .unwrap_or_else(|| Path::new(first).file_name().map_or(first.clone(), |n| n.to_string_lossy().into_owned()));
    let show = &i.show;
    let path = |p: &str| format!("{show}{}", Path::new(p).file_name().unwrap().to_string_lossy());
    let tools = [tool];
    let spec = Spec {
        info: &info,
        version: env!("CARGO_PKG_VERSION"),
        name: &name,
        target: Target::Pin { id: 1, col, row },
        roles: &roles,
        tools: &tools,
        path: &path,
        other: None,
        chain: &[],
    };
    let code = pixel_script(&spec)?;
    eprintln!("{}: {} dates, pixel {col},{row}", case.guide, info.t());
    Ok(json!({"tool": case.tool, "code": code, "steps": case.steps, "pixel": [col, row]}))
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("site_python CASES.json [OUT.json]");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(&path).expect("the cases file");
    let cases: Vec<Case> = serde_json::from_str(&text).expect("the cases are not JSON of the expected shape");
    let mut out = serde_json::Map::new();
    let mut failed = false;
    for case in &cases {
        match run(case) {
            Ok(v) => {
                out.insert(case.guide.clone(), v);
            }
            Err(e) => {
                eprintln!("{}: {e}", case.guide);
                failed = true;
            }
        }
    }
    let json = serde_json::to_string_pretty(&Value::Object(out)).expect("JSON");
    match std::env::args().nth(2) {
        Some(file) => std::fs::write(file, json + "\n").expect("the output file"),
        None => println!("{json}"),
    }
    if failed {
        std::process::exit(1);
    }
}
