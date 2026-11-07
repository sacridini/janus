//! What the chains of tools share (Time series analysis, tsa.rs; Analysis-ready data, ard.rs): a step of a chain run
//! as the tool's own task would run it (its folder with job.json, job.log, provenance.json and rerun.py, the spec
//! built from a series, its roles and a window, `janus_zeit::job::run_job`) and the chain as one task of Tools >
//! Tasks. Neither computes anything: the tools do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use janus_cube::CubeInfo;
use janus_zeit::job::{JobResult, run_job};
use janus_zeit::manifest::Tool;
use serde_json::{Value, json};

use crate::provenance::{OutputRecord, Provenance, SCRIPT, SeriesRecord};
use crate::zeit::{Job, JobLog, JobState, JobStatus, base_spec, other_series, series_inputs};

/// A step of the chain: its tool and the parameters it runs with.
pub(crate) struct ChainStep {
    pub tool: &'static Tool,
    pub params: BTreeMap<String, Value>,
}

/// What the chain starts from: the series, its band roles and the window it runs on.
pub(crate) struct ChainInput {
    pub info: Arc<CubeInfo>,
    /// the layer's name and what it opened (for the provenance)
    pub name: String,
    pub inputs: Vec<String>,
    pub roles: BTreeMap<String, i32>,
    pub window: [usize; 4],
    pub scope: String,
    pub scope_index: usize,
    /// the date a map tool takes its year from
    pub map_t: usize,
    pub threads: usize,
}

/// What a chain did: the outputs of every step that ran and the steps left out, with why.
pub(crate) struct Chain {
    pub result: JobResult,
    pub skipped: Vec<(String, String)>,
}

/// A step to run: the tool with its parameters on a series (its band roles, and the window of it, None for all of it),
/// and what its provenance says of where it reads and of the chain.
pub(crate) struct StepRun<'a> {
    pub tool: &'static Tool,
    pub params: &'a BTreeMap<String, Value>,
    pub info: &'a Arc<CubeInfo>,
    pub roles: &'a BTreeMap<String, i32>,
    pub window: Option<[usize; 4]>,
    /// the date a map tool takes its year from
    pub map_t: usize,
    pub threads: usize,
    /// a second series the tool reads (ImproPhe's coarse one): its name and series
    pub other: Option<(&'a str, &'a CubeInfo)>,
    /// the provenance's scope and its index in the window's list
    pub scope: String,
    pub scope_index: usize,
    /// the series the step reads, as the provenance records it
    pub series: SeriesRecord,
    /// where it reads, for the provenance and the log ("the series", or the file of the step that wrote it)
    pub reads: String,
    /// the chain's name, its folder, the step's place in it (from 0) and the number of steps
    pub task: &'a str,
    pub root: &'a Path,
    pub k: usize,
    pub n: usize,
}

/// The step's tool through `run_job` into its subfolder of the chain's (`NN_tool`: job.json, job.log,
/// provenance.json, rerun.py), its provenance written as it starts and as it ends. `progress` is of the step alone.
/// The error of a step that failed names its tool; a cancelled one is "cancelled".
pub(crate) fn run_step(
    r: &StepRun,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let (tool, info, k, n) = (r.tool, r.info, r.k, r.n);
    let dir = r.root.join(format!("{:02}_{}", k + 1, tool.id));
    let work = dir.join("work");
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let mut spec = base_spec(info, tool, r.params, r.map_t, r.threads);
    series_inputs(info, tool, r.roles, &work.join("series"), &mut spec)?;
    if let Some((name, other)) = r.other {
        other_series(other, name, &work.join("other"), &mut spec)?;
    }
    spec.window = r.window;
    spec.output_dir = dir.to_string_lossy().into_owned();
    let win = r.window.unwrap_or([0, 0, info.width, info.height]);
    let spec_path = dir.join("job.json");
    let _ = std::fs::write(&spec_path, serde_json::to_string_pretty(&spec).unwrap_or_default());
    // its provenance as it starts: the series the step read, its Python
    let mut prov = Provenance::start("zeit", &tool.id, &tool.name);
    prov.set_params(r.params);
    prov.scope = r.scope.clone();
    prov.scope_index = r.scope_index;
    prov.window = Some(win);
    prov.threads = spec.threads;
    prov.series = Some(r.series.clone());
    prov.extra.insert(
        "chain".into(),
        json!({"task": r.task, "step": k + 1, "of": n, "folder": r.root.to_string_lossy(), "reads": r.reads}),
    );
    let to = dir.join("python").to_string_lossy().into_owned();
    match crate::python::run_code(info, tool, r.params, r.roles, r.other.map(|o| o.1), win, spec.threads, &to) {
        Ok(code) if std::fs::write(dir.join(SCRIPT), &code).is_ok() => prov.python = SCRIPT.into(),
        Ok(_) => prov.python_note = format!("{SCRIPT} could not be written"),
        Err(e) => prov.python_note = e,
    }
    prov.write(&dir);
    let log = JobLog { path: dir.join("job.log"), t0: Instant::now() };
    log.write(&format!(
        "=== {} step {} of {n}: {}\ntool: {}\nreads: {}\nwindow: {win:?}\nspec: {}\noutput: {}\n",
        crate::zeit::timestamp_text(),
        k + 1,
        tool.name,
        tool.id,
        r.reads,
        spec_path.display(),
        dir.display()
    ));
    let logged = Mutex::new((-1.0, -1.0));
    let step_progress = |f: f64, m: &str| {
        progress(f, m);
        let now = log.t0.elapsed().as_secs_f64();
        let mut l = logged.lock().unwrap();
        if l.0 < 0.0 || f >= 1.0 || f - l.0 >= 0.1 || now - l.1 >= 5.0 {
            log.line(&format!("{:3.0}%  {m}", f * 100.0));
            *l = (f, now);
        }
    };
    let t0 = Instant::now();
    let res = run_job(&spec, &step_progress, cancel);
    prov.seconds = t0.elapsed().as_secs_f64();
    let res = match res {
        Ok(res) => res,
        Err(e) => {
            let cancelled = e == "cancelled";
            log.line(if cancelled { "cancelled" } else { "failed" });
            prov.state = if cancelled { "cancelled" } else { "failed" }.into();
            prov.error = if cancelled { String::new() } else { e.clone() };
            prov.write(&dir);
            return Err(if cancelled { e } else { format!("{}: {e}", tool.name) });
        }
    };
    prov.outputs = res
        .outputs
        .iter()
        .map(|o| OutputRecord {
            name: o["name"].as_str().or(o["id"].as_str()).unwrap_or("").to_string(),
            path: o["path"].as_str().unwrap_or("").to_string(),
        })
        .collect();
    for o in &prov.outputs {
        log.line(&format!("output: {} -> {}", o.name, o.path));
    }
    log.line("done");
    prov.state = "done".into();
    prov.write(&dir);
    Ok(res)
}

/// A step's call (its `result1`, `result2`... as the tools' generators write them) with its results numbered after the
/// `first` ones before it in the chain's script, and how many it has. `step_name` is what the files of its results are
/// called; a step with several adds `_r1`, `_r2`... to it. The pairs (variable, file name) go to `names` (`save_code`).
pub(crate) fn number_results(
    code: &str,
    first: usize,
    step_name: &str,
    names: &mut Vec<(String, String)>,
) -> (String, usize) {
    // its results after those before it (highest first: result2 before result1)
    let n = (1..=9).rev().find(|m| code.contains(&format!("result{m}"))).unwrap_or(0);
    let mut code = code.to_string();
    // through a placeholder first: the new names may have two digits, and "result1" is a prefix of "result12"
    for m in (1..=n).rev() {
        code = code.replace(&format!("result{m}"), &format!("\u{1}{m}\u{1}"));
    }
    for m in (1..=n).rev() {
        code = code.replace(&format!("\u{1}{m}\u{1}"), &format!("result{}", first + m));
        names
            .push((format!("result{}", first + m), if n > 1 { format!("{step_name}_r{m}") } else { step_name.into() }));
    }
    let k0 = names.len() - n;
    names[k0..].reverse();
    (code, n)
}

/// The series a chain wrote (its name and file): what the Classification can read the dates of. The outputs a chain
/// keeps from the layers (`no_layer`) are not among them.
pub(crate) fn series_outputs(result: &JobResult) -> Vec<(String, String)> {
    result
        .outputs
        .iter()
        .filter(|o| o["series"].as_bool() == Some(true) && o["no_layer"].as_bool() != Some(true))
        .filter_map(|o| {
            let name = o["name"].as_str().or(o["id"].as_str())?.to_string();
            Some((name, o["path"].as_str()?.to_string()))
        })
        .collect()
}

/// What a chain's task is: how it is listed in Tools > Tasks and where it writes.
pub(crate) struct JobStart {
    pub title: String,
    /// the task's tool id in the list (not a tool of the manifest)
    pub tool: &'static str,
    /// the thread's name
    pub thread: &'static str,
    /// the series the task belongs to
    pub cube: u64,
    pub root: PathBuf,
    /// the log's first lines after its header
    pub log_head: String,
}

/// Starts the chain as one task (Tools > Tasks): `run` on a thread of its own with the chain's progress, logged
/// every 5 s or 10%, the skipped steps and outputs in the log, and the series it wrote (`series_outputs`) in `written`
/// when it ends well.
pub(crate) fn spawn_job(
    jobs: &mut Vec<Job>,
    start: JobStart,
    written: Arc<Mutex<Vec<(String, String)>>>,
    ctx: &egui::Context,
    run: impl FnOnce(&(dyn Fn(f64, &str) + Sync), &AtomicBool) -> Result<Chain, String> + Send + 'static,
) {
    let JobStart { title, tool, thread, cube, root, log_head } = start;
    let log = JobLog { path: root.join("job.log"), t0: Instant::now() };
    log.write(&format!("=== {} job: {title}\n{log_head}output: {}\n", crate::zeit::timestamp_text(), root.display()));
    let log_path = log.path.to_string_lossy().to_string();
    let status = Arc::new(Mutex::new(JobStatus {
        state: JobState::Running,
        progress: 0.0,
        message: "starting".into(),
        error: String::new(),
        result: None,
        seconds: 0.0,
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (st, cn, ctx) = (status.clone(), cancel.clone(), ctx.clone());
    std::thread::Builder::new()
        .name(thread.into())
        .spawn(move || {
            let t0 = Instant::now();
            // progress to the log every 5 s or 10%
            let logged = Mutex::new((-1.0, -1.0));
            let progress = |f: f64, m: &str| {
                let mut s = st.lock().unwrap();
                s.progress = f;
                s.message = m.to_string();
                drop(s);
                let now = t0.elapsed().as_secs_f64();
                let mut l = logged.lock().unwrap();
                if l.0 < 0.0 || f >= 1.0 || f - l.0 >= 0.1 || now - l.1 >= 5.0 {
                    log.line(&format!("{:3.0}%  {m}", f * 100.0));
                    *l = (f, now);
                }
                ctx.request_repaint();
            };
            let r = run(&progress, &cn);
            let mut s = st.lock().unwrap();
            s.seconds = t0.elapsed().as_secs_f64();
            match r {
                Ok(chain) => {
                    for (tool, why) in &chain.skipped {
                        log.line(&format!("skipped {tool}: {why}"));
                    }
                    for o in &chain.result.outputs {
                        let name = o["name"].as_str().or(o["id"].as_str()).unwrap_or("");
                        log.line(&format!("output: {name} -> {}", o["path"].as_str().unwrap_or("")));
                    }
                    log.line("done");
                    s.message = match chain.skipped.len() {
                        0 => String::new(),
                        _ => format!(
                            "Left out: {}.",
                            chain.skipped.iter().map(|(t, w)| format!("{t} ({w})")).collect::<Vec<_>>().join("; ")
                        ),
                    };
                    *written.lock().unwrap() = series_outputs(&chain.result);
                    s.state = JobState::Done;
                    s.progress = 1.0;
                    s.result = Some(chain.result);
                }
                Err(e) if e == "cancelled" => {
                    log.line("cancelled");
                    s.state = JobState::Cancelled;
                }
                Err(e) => {
                    log.line(&format!("error: {e}"));
                    log.line("failed");
                    s.state = JobState::Failed;
                    s.error = e;
                }
            }
            ctx.request_repaint();
        })
        .ok();
    jobs.push(Job {
        title,
        tool: tool.into(),
        cube,
        output_dir: root.to_string_lossy().to_string(),
        log_path,
        started: Instant::now(),
        status,
        cancel,
        loaded: false,
    });
}

/// Whether a cancel was asked of the chain (what a chain checks before each step).
pub(crate) fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

/// The index of the layer that opened exactly `path` (a series a chain wrote), among the layers' inputs.
pub(crate) fn opened_from(layers: &[&[String]], path: &str) -> Option<usize> {
    let key = crate::provenance::absolute(path);
    layers.iter().position(|inputs| inputs.len() == 1 && crate::provenance::absolute(&inputs[0]) == key)
}

impl crate::app::State {
    /// The layer that opened the series `path`, if one is open.
    pub(crate) fn series_layer(&self, path: &str) -> Option<usize> {
        let inputs: Vec<&[String]> = self.layers.iter().map(|l| l.inputs.as_slice()).collect();
        opened_from(&inputs, path)
    }

    /// Makes the layer that opened `path` the active one, if there is one.
    pub(crate) fn activate_series(&mut self, path: &str, ctx: &egui::Context) -> bool {
        let found = self.series_layer(path);
        if let Some(i) = found
            && i != self.active
        {
            self.set_active(i, ctx);
        }
        found.is_some()
    }

    /// The series `path` a chain wrote, active: its layer if it is open, else opened as the chains' other series are
    /// (it becomes active when it arrives: `pending_use`, taken by the Time series analysis window's frame).
    pub(crate) fn use_series(&mut self, path: &str, ctx: &egui::Context) {
        if !self.activate_series(path, ctx) {
            if !self.zeit.series_to_open.iter().any(|(q, _)| q == path) {
                self.zeit.series_to_open.push((path.to_string(), false));
            }
            self.tsa.pending_use = Some(path.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_series_is_found_among_the_layers_by_the_one_file_it_opened() {
        let a = vec!["C:/data/a.tif".to_string()];
        let b = vec!["C:/data/out/series".to_string()];
        let two = vec!["C:/data/out/series".to_string(), "C:/data/b.tif".to_string()];
        let layers: Vec<&[String]> = vec![&a, &two, &b];
        assert_eq!(opened_from(&layers, "C:/data/out/series"), Some(2));
        assert_eq!(opened_from(&layers, "C:/data/a.tif"), Some(0));
        assert_eq!(opened_from(&layers, "C:/data/c.tif"), None);
        assert_eq!(opened_from(&[], "C:/data/a.tif"), None);
    }
}
