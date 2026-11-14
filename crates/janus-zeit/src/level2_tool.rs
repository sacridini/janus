//! The Level 1C to surface reflectance tool (`level2`): Zeit's `zeit.level2` (FORCE's force-l2ps, [`crate::level2_run`])
//! on one Sentinel-2 `.SAFE` folder, or on every `.SAFE` folder of a folder, one scene at a time. It reads no series: its
//! input is the `input` parameter (a folder), its output folder holds a subfolder for each scene, named after the product,
//! with the seven GeoTIFFs of `ds.zeit.save` and `attrs.json` (the dataset's attributes).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::job::{Estimate, JobResult, JobSpec};
use crate::level2::Safe;
use crate::level2_run::{self, Attr, Level2Options, Level2Output, Merge};
use crate::manifest::Tool;
use crate::pixel::Params;

/// Peak memory of a run on a whole tile (measured on 23LKC: 13.0 GiB without the merge, 17.5 with it).
pub const GIB_PER_SCENE: f64 = 13.0;
pub const GIB_PER_SCENE_MERGED: f64 = 17.5;

/// Seconds a scene takes on a whole tile with 8 threads (measured on 23LKC).
const SECONDS_PER_SCENE: f64 = 150.0;

/// Whether the folder is a `.SAFE` product itself.
fn is_safe(path: &Path) -> bool {
    let named = path.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with(".safe"));
    named || path.join("MTD_MSIL1C.xml").is_file()
}

/// The scenes of the input folder: the folder itself if it is a `.SAFE`, else the `.SAFE` folders in it, sorted by name.
pub fn scenes(input: &str) -> Result<Vec<PathBuf>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("ValueError: choose the folder of a Sentinel-2 L1C product (.SAFE), or of several".into());
    }
    let path = PathBuf::from(input);
    if !path.is_dir() {
        return Err(format!("ValueError: {input} is not a folder"));
    }
    if is_safe(&path) {
        return Ok(vec![path]);
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(&path)
        .map_err(|e| format!("{input}: {e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && is_safe(p))
        .collect();
    found.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    if found.is_empty() {
        return Err(format!("ValueError: no .SAFE folder in {input}"));
    }
    Ok(found)
}

/// The product's name: the `.SAFE` folder's without its extension.
pub fn product_name(safe: &Path) -> String {
    let name = safe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.len().checked_sub(5).filter(|&k| name.is_char_boundary(k) && name[k..].eq_ignore_ascii_case(".safe")) {
        Some(k) => name[..k].to_string(),
        None => name,
    }
}

fn path_param(p: &Params, id: &str) -> Option<PathBuf> {
    Some(p.str(id).trim()).filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// The options of `zeit.level2` for the tool's parameters, checked as Zeit checks them (a DEM when the topography is on,
/// the files there, a merge Zeit has). `threads` caps the work (Zeit's `n_jobs`).
pub fn options(p: &Params, threads: usize) -> Result<Level2Options, String> {
    let merge = match p.str("merge") {
        "" | "none" => None,
        m => Merge::parse(Some(m)).map_err(|e| format!("ValueError: {e}"))?,
    };
    let dem = path_param(p, "dem");
    if p.bool("topography") && dem.is_none() {
        return Err(
            "ValueError: the topographic correction needs a digital elevation model (a raster in metres): choose \
                    one in 'Digital elevation model', or switch the topographic correction off"
                .into(),
        );
    }
    let gswo = path_param(p, "gswo");
    for (what, f) in [("digital elevation model", &dem), ("water occurrence", &gswo)] {
        if let Some(f) = f.as_ref().filter(|f| !f.is_file()) {
            return Err(format!("ValueError: the {what} {} is not a file", f.display()));
        }
    }
    Ok(Level2Options {
        dem,
        brdf: p.bool("brdf"),
        topography: p.bool("topography"),
        aod: p.bool("aod"),
        adjacency: p.bool("adjacency"),
        multiple_scattering: p.bool("multiple_scattering"),
        erase_cloud: p.bool("erase_cloud"),
        fmask: p.bool("fmask"),
        gswo,
        fmask_compat: p.bool("fmask_compat"),
        n_jobs: threads.clamp(1, 8),
        merge,
        ..Default::default()
    })
}

/// The threads of a run: the app's setting, else the machine's (never above 8, Zeit's own cap).
fn threads(spec: &JobSpec) -> usize {
    match spec.threads {
        Some(n) if n > 0 => n as usize,
        _ => std::thread::available_parallelism().map_or(1, |n| n.get()),
    }
    .clamp(1, 8)
}

fn attr_json(a: &Attr) -> Value {
    match a {
        Attr::Bool(b) => json!(b),
        Attr::Int(i) => json!(i),
        Attr::Float(f) => json!(f), // NaN is null
        Attr::Str(s) => json!(s),
        Attr::List(l) => Value::Array(l.iter().map(attr_json).collect()),
    }
}

/// The dataset's attributes as a JSON object in the order of `ds.attrs`.
pub fn attrs_json(out: &Level2Output) -> String {
    let rows: Vec<String> =
        out.attrs.pairs().iter().map(|(k, a)| format!("  {}: {}", Value::String(k.clone()), attr_json(a))).collect();
    format!("{{\n{}\n}}\n", rows.join(",\n"))
}

/// Writes a scene's products and `attrs.json` to `folder`.
pub fn save(out: &Level2Output, folder: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = out.save(folder)?;
    let f = folder.join("attrs.json");
    std::fs::write(&f, attrs_json(out)).map_err(|e| format!("{}: {e}", f.display()))?;
    files.push(f);
    Ok(files)
}

/// The memory a scene needs, in GiB.
pub fn gib(p: &Params) -> f64 {
    if matches!(p.str("merge"), "regression" | "improphe") { GIB_PER_SCENE_MERGED } else { GIB_PER_SCENE }
}

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let scenes = scenes(p.str("input"))?;
    let opts = options(p, threads(spec))?;
    // every product opens before the first runs: a wrong folder is said at once, not after hours
    let products: Vec<(String, Safe)> = scenes
        .iter()
        .map(|s| Safe::open(s).map(|safe| (product_name(s), safe)).map_err(|e| format!("ValueError: {e}")))
        .collect::<Result<_, _>>()?;
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let n = products.len();
    let mut outputs: Vec<Value> = vec![];
    let mut window = [0, 0, 0, 0];
    for (k, (name, safe)) in products.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let label = if n > 1 { format!("scene {} of {n}: {name}", k + 1) } else { name.clone() };
        progress(k as f64 / n as f64, &format!("{label}: correcting (a scene runs to its end)"));
        let out = level2_run::level2(safe, &opts).map_err(|e| format!("{name}: {e}"))?;
        progress((k as f64 + 0.9) / n as f64, &format!("{label}: writing"));
        let folder = dir.join(name);
        let files = save(&out, &folder).map_err(|e| format!("{name}: {e}"))?;
        window = [0, 0, out.ncols, out.nrows];
        for (id, file) in [("boa", &files[0]), ("qai", &files[1])] {
            let o = tool.outputs.iter().find(|o| o.id == id);
            let mut d = serde_json::to_value(o).unwrap_or_default();
            d["name"] = json!(format!("{name} {}", o.map_or(id, |o| o.name.as_str())));
            d["path"] = json!(file.to_string_lossy());
            outputs.push(d);
        }
        progress((k + 1) as f64 / n as f64, &format!("{label}: done"));
    }
    Ok(JobResult { outputs, window })
}

pub(crate) fn estimate(spec: &JobSpec, p: &Params) -> Result<Estimate, String> {
    let n = scenes(p.str("input")).map_or(1, |s| s.len()) as f64;
    // a run is by scene, not by window (a folder tool has none): the whole cost is fixed, whatever the window
    let threads = threads(spec) as f64;
    let merge = if gib(p) > GIB_PER_SCENE { 1.3 } else { 1.0 };
    let sec_fixed = SECONDS_PER_SCENE * 8.0 / threads * merge * n;
    Ok(Estimate { sec_per_px: 0.0, sec_per_chunk: 0.0, chunk_cells: 4_000_000.0, sec_fixed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(given: &[(&str, Value)]) -> Params {
        let given = given.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        Params::with_defaults("level2", &given).unwrap()
    }

    #[test]
    fn product_names_lose_their_extension() {
        assert_eq!(product_name(Path::new("a/S2B_MSIL1C_20230728_T23LKC.SAFE")), "S2B_MSIL1C_20230728_T23LKC");
        assert_eq!(product_name(Path::new("x/scene.safe")), "scene");
        assert_eq!(product_name(Path::new("x/scene")), "scene");
    }

    #[test]
    fn a_folder_of_safes_gives_them_sorted_and_a_safe_itself() {
        let dir = std::env::temp_dir().join(format!("janus_l2_scenes_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for n in ["b.SAFE", "a.SAFE", "other"] {
            std::fs::create_dir_all(dir.join(n)).unwrap();
        }
        let found = scenes(dir.to_str().unwrap()).unwrap();
        let names: Vec<String> = found.iter().map(|p| product_name(p)).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(scenes(dir.join("b.SAFE").to_str().unwrap()).unwrap().len(), 1);
        assert!(scenes(dir.join("other").to_str().unwrap()).unwrap_err().contains("no .SAFE"));
        assert!(scenes("").unwrap_err().contains("choose the folder"));
        assert!(scenes(dir.join("nothing").to_str().unwrap()).unwrap_err().contains("not a folder"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_topography_needs_a_dem_and_the_options_follow_the_parameters() {
        let e = options(&params(&[]), 8).unwrap_err();
        assert!(e.contains("digital elevation model"), "{e}");
        let o =
            options(&params(&[("topography", json!(false)), ("merge", json!("improphe")), ("brdf", json!(false))]), 20)
                .unwrap();
        assert_eq!((o.merge, o.brdf, o.topography, o.n_jobs), (Some(Merge::Improphe), false, false, 8));
        assert!(o.dem.is_none() && o.aod && o.adjacency && o.multiple_scattering && o.fmask && !o.erase_cloud);
        let missing = params(&[("dem", json!("no/such/dem.tif"))]);
        assert!(options(&missing, 4).unwrap_err().contains("is not a file"));
        assert!(options(&params(&[("topography", json!(false)), ("merge", json!("starfm"))]), 4).is_err());
    }

    #[test]
    fn the_estimate_is_fixed_by_scene_and_needs_no_window() {
        let dir = std::env::temp_dir().join(format!("janus_l2_est_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for n in ["a.SAFE", "b.SAFE"] {
            std::fs::create_dir_all(dir.join(n)).unwrap();
        }
        let input = json!(dir.to_str().unwrap());
        let spec = |threads: u32, merge: &str| JobSpec {
            tool: "level2".into(),
            threads: Some(threads as _),
            params: [("input".to_string(), input.clone()), ("merge".to_string(), json!(merge))].into_iter().collect(),
            ..Default::default()
        };
        let est = |threads: u32, merge: &str| {
            let s = spec(threads, merge);
            let p = Params::with_defaults("level2", &s.params).unwrap();
            estimate(&s, &p).unwrap().seconds(0, 0)
        };
        assert!((est(8, "none") - 300.0).abs() < 1e-6);
        assert!((est(8, "improphe") - 390.0).abs() < 1e-6);
        assert!((est(4, "none") - 600.0).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
