//! "Copy Python" of the Level 1C to surface reflectance tool: Zeit's `zeit.level2` on each scene of the input folder, with the
//! tool's parameters (only those that are not Zeit's defaults), and `save` writing the products as the tool does.

use std::fmt::Write as _;

use super::{Arg, arg_text, is_default, kw, py_str, wrap_call};
use crate::level2_tool::{product_name, scenes};
use crate::pixel::Params;

/// A Python string literal with forward slashes (which Python reads on every system).
fn dq(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "/").replace('"', "\\\""))
}

/// The arguments of `zeit.level2` after the scene: the tool's parameters that are not Zeit's defaults.
fn arguments(p: &Params, path: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut args: Vec<Arg> = vec![];
    let file = |id: &str| Some(p.str(id).trim()).filter(|s| !s.is_empty()).map(|s| dq(&path(s)));
    if let Some(dem) = file("dem") {
        args.push(kw("dem", dem));
    }
    if matches!(p.str("merge"), "regression" | "improphe") {
        args.push(kw("merge", py_str(p.str("merge")).replace('\'', "\"")));
    }
    for (id, name) in [
        ("brdf", "brdf"),
        ("topography", "topography"),
        ("aod", "aod"),
        ("adjacency", "adjacency"),
        ("multiple_scattering", "multiple_scattering"),
        ("erase_cloud", "erase_cloud"),
        ("fmask", "fmask"),
        ("fmask_compat", "fmask_compat"),
    ] {
        args.push(kw(name, if p.bool(id) { "True" } else { "False" }));
    }
    if let Some(g) = file("gswo") {
        args.push(kw("gswo", g));
    }
    args.iter().filter(|a| !is_default("zeit.level2", a)).map(arg_text).collect()
}

/// The script of a run: each scene of `input` (a .SAFE folder, or a folder of them) corrected and saved under
/// `out_dir/<product>`. `path` makes a path absolute as the script writes it. Fails as the run does when the folder holds
/// no scene.
pub fn level2_script(
    version: &str,
    p: &Params,
    path: &dyn Fn(&str) -> String,
    out_dir: &str,
) -> Result<String, String> {
    let scenes = scenes(p.str("input"))?;
    let args = arguments(p, path);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = crate::numeric::civil(now.div_euclid(86400));
    let mut out = format!(
        "\"\"\"Janus {version} ({y:04}-{m:02}-{d:02}): the run of Level 1C to surface reflectance (FORCE) on {}, reproduced \
         outside Janus with Zeit.\nNeeds: pip install \"zeit-cdts>=3.6.0\"\nA scene takes about 22 GiB of memory (29 with the merge); they \
         run one at a time.\n\"\"\"\nimport zeit\n\n",
        scenes.iter().map(|s| product_name(s)).collect::<Vec<_>>().join(", ").replace('"', "'"),
    );
    let out_dir = path(out_dir);
    for (k, s) in scenes.iter().enumerate() {
        let mut call: Vec<String> = vec![dq(&path(&s.to_string_lossy()))];
        call.extend(args.iter().cloned());
        let _ = writeln!(out, "{}", wrap_call("l2", "zeit.level2", &call));
        let _ = writeln!(
            out,
            "l2.zeit.save({})",
            dq(&format!("{}/{}", out_dir.trim_end_matches(['/', '\\']), product_name(s)))
        );
        if k + 1 < scenes.len() {
            let _ = writeln!(out, "del l2  # the scene's memory before the next one\n");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_script_calls_zeit_level2_for_each_scene_with_the_parameters_that_are_not_defaults() {
        let dir = std::env::temp_dir().join(format!("janus_l2_script_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for n in ["S2B_MSIL1C_b.SAFE", "S2A_MSIL1C_a.SAFE"] {
            std::fs::create_dir_all(dir.join(n)).unwrap();
        }
        let given = [
            ("input", json!(dir.to_string_lossy())),
            ("dem", json!("D:\\data\\dem.tif")),
            ("merge", json!("improphe")),
            ("brdf", json!(false)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let p = Params::with_defaults("level2", &given).unwrap();
        let text = level2_script("1.2.3", &p, &|s| s.to_string(), "E:\\out").unwrap();
        let body = &text[text.find("import zeit").unwrap()..];
        assert!(body.contains("l2 = zeit.level2("), "{body}");
        assert!(body.contains("dem=\"D:/data/dem.tif\""), "{body}");
        assert!(body.contains("merge=\"improphe\""), "{body}");
        assert!(body.contains("brdf=False"), "{body}");
        assert!(!body.contains("topography") && !body.contains("fmask") && !body.contains("aod"), "{body}");
        assert!(body.contains("l2.zeit.save(\"E:/out/S2A_MSIL1C_a\")"), "{body}");
        assert!(body.contains("l2.zeit.save(\"E:/out/S2B_MSIL1C_b\")"), "{body}");
        assert!(body.find("S2A_MSIL1C_a").unwrap() < body.find("S2B_MSIL1C_b").unwrap(), "{body}");
        assert_eq!(body.matches("del l2").count(), 1, "{body}");
        assert!(text.contains("zeit-cdts>=3.6.0"));
    }
}
