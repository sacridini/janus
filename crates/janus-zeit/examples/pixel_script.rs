//! The Python of Copy Python for a pin or an ROI, from the command line (what the chart's "Python..." menu gives):
//!
//!     pixel_script OUT.py --open PATH... [--band N] [--nd N] [--expr TEXT] [--qa N --rule fmask|landsat_c2|scl|hls_fmask|nonzero]
//!         (--pin COL,ROW | --roi X0,Y0,X1,Y1 [--ring "x,y x,y ..."]) [--roles blue=1,green=2,...]
//!         [--tool ID[=JSON params | =@FILE.json]]... [--show PREFIX]
//!
//! `--show` writes the files' paths as PREFIX + the file's name (the website's guides), else absolute.

use std::collections::BTreeMap;

use janus_cube::{BandSelection, QaRule};
use janus_zeit::Params;
use janus_zeit::python::{Roi, Spec, Target, pixel_script};
use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).expect("OUT.py").clone();
    let mut open: Vec<String> = vec![];
    let mut sel = BandSelection::default();
    let mut target: Option<Target> = None;
    let mut roles: BTreeMap<String, i32> = BTreeMap::new();
    let mut tools: Vec<(String, Params)> = vec![];
    let mut show: Option<String> = None;
    let mut rings: Vec<Vec<f64>> = vec![];
    let mut roi: Option<[usize; 4]> = None;
    let mut explicit_roles = false;
    let mut i = 2;
    while i < args.len() {
        let a = args[i].as_str();
        let mut next = || {
            i += 1;
            args[i].clone()
        };
        match a {
            "--open" => {
                while i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    i += 1;
                    open.push(args[i].clone());
                }
            }
            "--band" => sel.band = next().parse().unwrap(),
            "--nd" => sel.nd_band = next().parse().unwrap(),
            "--qa" => sel.qa_band = next().parse().unwrap(),
            "--expr" => sel.expr = janus_cube::calc::ExprId::intern("Expression", &next()),
            "--rule" => {
                sel.qa_rule = match next().as_str() {
                    "fmask" => QaRule::Fmask,
                    "landsat_c2" => QaRule::LandsatC2,
                    "scl" => QaRule::Scl,
                    "hls_fmask" => QaRule::HlsFmask,
                    _ => QaRule::NonZero,
                }
            }
            "--pin" => {
                let v: Vec<i64> = next().split(',').map(|x| x.parse().unwrap()).collect();
                target = Some(Target::Pin { id: 1, col: v[0], row: v[1] });
            }
            "--roi" => {
                let v: Vec<usize> = next().split(',').map(|x| x.parse().unwrap()).collect();
                roi = Some([v[0], v[1], v[2], v[3]]);
            }
            "--ring" => {
                let v: Vec<f64> =
                    next().split([' ', ',']).filter(|x| !x.is_empty()).map(|x| x.parse().unwrap()).collect();
                rings.push(v);
            }
            "--roles" => {
                explicit_roles = true;
                for kv in next().split(',') {
                    let (k, v) = kv.split_once('=').unwrap();
                    roles.insert(k.to_string(), v.parse().unwrap());
                }
            }
            "--tool" => {
                let t = next();
                let (id, json) = t.split_once('=').map_or((t.as_str(), "{}"), |(a, b)| (a, b));
                let json = json.strip_prefix('@').map_or(json.to_string(), |f| std::fs::read_to_string(f).unwrap());
                let given: BTreeMap<String, Value> = serde_json::from_str(&json).unwrap();
                tools.push((id.to_string(), Params::with_defaults(id, &given).unwrap()));
            }
            "--show" => show = Some(next()),
            other => panic!("unknown argument {other}"),
        }
        i += 1;
    }
    let info = janus_cube::open_cube(&open, &sel).expect("open_cube");
    if let Some([x0, y0, x1, y1]) = roi {
        target = Some(Target::Roi(Roi { x0, y0, x1, y1, sampled: None, rings }));
    }
    if !explicit_roles {
        roles = janus_cube::roles::guess_band_roles(&info).into_iter().map(|(r, b)| (r.to_string(), b)).collect();
    }
    let path = |p: &str| match &show {
        Some(prefix) => format!("{prefix}{}", std::path::Path::new(p).file_name().unwrap().to_string_lossy()),
        None => std::path::absolute(p).map_or(p.to_string(), |a| a.to_string_lossy().into_owned()),
    };
    let name = std::path::Path::new(&open[0]).file_name().unwrap().to_string_lossy().into_owned();
    let spec = Spec {
        info: &info,
        version: env!("CARGO_PKG_VERSION"),
        name: &name,
        target: target.expect("--pin or --roi"),
        roles: &roles,
        tools: &tools,
        path: &path,
        other: None,
        chain: &[],
    };
    match pixel_script(&spec) {
        Ok(code) => std::fs::write(out, code).unwrap(),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
