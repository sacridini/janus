//! The arguments a pin's or an ROI's script leaves out are those with Zeit's default value: `ZEIT_DEFAULTS` is checked
//! against the signatures of the installed Zeit (with `JANUS_PYTHON` naming a Python that has zeit-cdts), and the
//! registry's defaults are compared with them, the differences listed.

use std::collections::BTreeMap;

use janus_zeit::python::{ZEIT_DEFAULTS, tool_call};
use janus_zeit::{Params, PixelInput};

/// The calls of every tool with its default parameters (the ones that can be written without endmembers/patterns).
fn calls() -> Vec<(String, janus_zeit::python::ToolCall)> {
    let n = 60;
    let input = PixelInput {
        years: (0..n).map(|i| 2015.0 + i as f64 / 12.0).collect(),
        days: Some((0..n).map(|i| 16436 + 30 * i as i64).collect()),
        values: vec![0.3; n],
        bands: ["blue", "green", "red", "nir", "swir1", "swir2", "thermal"]
            .iter()
            .map(|r| (r.to_string(), vec![]))
            .collect(),
        ..Default::default()
    };
    janus_zeit::manifest()
        .tools
        .iter()
        .filter_map(|t| {
            let p = Params::with_defaults(&t.id, &BTreeMap::new())?;
            Some((t.id.clone(), tool_call(&t.id, &p, &input)?))
        })
        .collect()
}

#[test]
fn a_compact_call_omits_exactly_the_arguments_with_zeits_default() {
    let all = calls();
    assert!(all.len() > 20);
    let mut omitted = 0;
    for (id, call) in &all {
        let (code, _) = call.compact("");
        for (func, name, value) in call.arguments() {
            if name == "nodata" || name == "n_jobs" {
                assert!(!code.contains(&format!("{name}=")), "{id}: {name} is plumbing\n{code}");
                continue;
            }
            let is_default = ZEIT_DEFAULTS.iter().any(|&(f, p, v)| f == func && p == name && v == value);
            let at = format!(" {name}={value}");
            let shown = code
                .match_indices(&at)
                .any(|(i, _)| matches!(code[i + at.len()..].chars().next(), None | Some(',' | ')' | '\n')));
            // an argument is shown unless its value is Zeit's default
            assert_eq!(shown, !is_default, "{id}: {func} {name}={value}\n{code}");
            omitted += usize::from(is_default);
        }
    }
    assert!(omitted > 0, "some defaults equal the registry's");
}

#[test]
fn the_defaults_table_is_zeits() {
    let Ok(python) = std::env::var("JANUS_PYTHON") else { return };
    let dir = std::env::temp_dir().join(format!("janus-zeit-defaults-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let table: Vec<[&str; 3]> = ZEIT_DEFAULTS.iter().map(|&(f, p, v)| [f, p, v]).collect();
    let used: Vec<[String; 3]> = calls().iter().flat_map(|(_, c)| c.arguments()).map(|(f, p, v)| [f, p, v]).collect();
    std::fs::write(dir.join("in.json"), serde_json::json!({"table": table, "used": used}).to_string()).unwrap();
    std::fs::write(
        dir.join("check.py"),
        r#"
import inspect, json, sys
import zeit
data = json.load(open(sys.argv[1]))
def default(func, name):
    p = inspect.signature(getattr(zeit, func.split(".", 1)[1])).parameters.get(name)
    return None if p is None or p.default is inspect.Parameter.empty else repr(p.default)
wrong = [f"{f} {n}: table {v}, Zeit {default(f, n)}" for f, n, v in data["table"] if default(f, n) != v]
# the registry's value of a parameter against Zeit's default
differ = sorted({f"{f} {n}: registry {v}, Zeit {default(f, n)}" for f, n, v in data["used"]
                 if n not in ("nodata", "n_jobs") and default(f, n) is not None and default(f, n) != v})
print("RESULT " + json.dumps({"wrong": wrong, "differ": differ}))
"#,
    )
    .unwrap();
    let out = std::process::Command::new(python)
        .arg("-I")
        .arg(dir.join("check.py"))
        .arg(dir.join("in.json"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("RESULT "))
        .unwrap_or_else(|| panic!("{stdout}\n{}", String::from_utf8_lossy(&out.stderr)));
    let r: serde_json::Value = serde_json::from_str(line).unwrap();
    println!("registry defaults that are not Zeit's (kept explicit in the scripts):");
    for d in r["differ"].as_array().unwrap() {
        println!("  {}", d.as_str().unwrap());
    }
    assert!(r["wrong"].as_array().unwrap().is_empty(), "the table is not Zeit's: {}", r["wrong"]);
}
