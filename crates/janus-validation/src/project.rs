//! The project: a GeoPackage with the sample points, every interpreter's labels, the strata and the project's
//! settings; the strata raster beside it. The same tables and keys as the C++, so a project made by either opens in
//! the other (src/validation.cpp: createProject, loadProject, saveLabels, ...).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use gdal::spatial_ref::{AxisMappingStrategy, SpatialRef};
use gdal::vector::sql::Dialect;
use gdal::vector::{Feature, Geometry, LayerAccess, LayerOptions, OGRFieldType, OGRwkbGeometryType};
use gdal::{Dataset, DatasetOptions, DriverManager, GdalOpenFlags};
use serde_json::{Value, json};

use crate::NONE;
use crate::design::{Point, Rng};
use crate::labels::{Label, events_from, events_text, resolve};
use crate::reader::last_error;
use crate::rules::{Grid, MapRule, row_areas};
use crate::strata::{Strata, StrataSpec, Stratum};

/// A map assessed with the sample, and how the reference labels translate to its classes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssessedMap {
    pub rule: MapRule,
    /// reference label -> class name ("" = left out)
    pub mapping: BTreeMap<String, String>,
}

/// samples (points in the grid's CRS: sample_id, stratum, stratum_name, row, col, and the reference: label,
/// ref_date, confidence, notes, events, interpreters, status), labels (sample_id, interpreter, label, date,
/// confidence, notes, events, updated), strata and project (key, value) tables; the strata raster beside it.
#[derive(Clone, Debug)]
pub struct Project {
    /// absolute
    pub path: String,
    pub strata_path: String,
    /// copies of a project share it
    pub id: String,
    pub spec: StrataSpec,
    /// without row_counts
    pub strata: Strata,
    pub seed: u64,
    /// alloc, target_se, min_per_stratum, confidence
    pub design: Value,
    pub label_set: Vec<String>,
    pub points: Vec<Point>,
    /// interpreter -> per point
    pub labels: BTreeMap<String, Vec<Label>>,
    pub maps: Vec<AssessedMap>,
}

impl Default for Project {
    fn default() -> Self {
        Project {
            path: String::new(),
            strata_path: String::new(),
            id: String::new(),
            spec: StrataSpec::default(),
            strata: Strata::default(),
            seed: 42,
            design: json!({}),
            label_set: Vec::new(),
            points: Vec::new(),
            labels: BTreeMap::new(),
            maps: Vec::new(),
        }
    }
}

/// An interpreter's labels of some points (index, label) and the reference that follows for them on the samples
/// layer.
#[derive(Clone, Debug, Default)]
pub struct FinalRef {
    pub point: usize,
    /// empty: none
    pub reference: Label,
    pub interpreters: i32,
    pub conflict: bool,
}

pub fn new_project_id() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    // the address of a local and the process id stand for the C++'s random_device
    let local = 0u8;
    let mut r = Rng(t ^ ((&local as *const u8 as u64) << 16) ^ (u64::from(std::process::id()) << 40));
    format!("{:016x}{:016x}", r.next(), r.next())
}

fn sql_text(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A path relative to `base` when they share a root (C++'s lexically_relative), else None.
fn relative(path: &Path, base: &Path) -> Option<PathBuf> {
    let (a, b): (Vec<Component>, Vec<Component>) = (path.components().collect(), base.components().collect());
    if a.first() != b.first() {
        return None;
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut r = PathBuf::new();
    for _ in common..b.len() {
        r.push("..");
    }
    for c in &a[common..] {
        r.push(c.as_os_str());
    }
    (!r.as_os_str().is_empty()).then_some(r)
}

/// "." and ".." resolved without the file system (C++'s lexically_normal).
fn normal(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

fn open_gpkg(path: &str, update: bool) -> Result<Dataset, String> {
    let mut flags = GdalOpenFlags::GDAL_OF_VECTOR;
    flags |= if update { GdalOpenFlags::GDAL_OF_UPDATE } else { GdalOpenFlags::GDAL_OF_READONLY };
    Dataset::open_ex(path, DatasetOptions { open_flags: flags, ..Default::default() })
        .map_err(|e| format!("cannot open {path}: {e}"))
}

/// A transaction on the dataset (GDALDatasetStartTransaction), rolled back unless committed.
struct Txn<'a> {
    ds: &'a Dataset,
    open: bool,
}

impl<'a> Txn<'a> {
    fn start(ds: &'a Dataset) -> Txn<'a> {
        // SAFETY: a live dataset.
        unsafe { gdal_sys::GDALDatasetStartTransaction(ds.c_dataset(), 0) };
        Txn { ds, open: true }
    }

    fn commit(mut self, what: &str) -> Result<(), String> {
        self.open = false;
        // SAFETY: a live dataset.
        if unsafe { gdal_sys::GDALDatasetCommitTransaction(self.ds.c_dataset()) } != gdal_sys::OGRErr::OGRERR_NONE {
            return Err(last_error(what));
        }
        Ok(())
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        if self.open {
            // SAFETY: a live dataset.
            unsafe { gdal_sys::GDALDatasetRollbackTransaction(self.ds.c_dataset()) };
        }
    }
}

fn sql(ds: &Dataset, q: &str) {
    let _ = ds.execute_sql(q, None, Dialect::DEFAULT);
}

fn idx(f: &Feature, name: &str) -> Result<usize, String> {
    f.field_index(name).map_err(|e| format!("{name}: {e}"))
}

fn set_int(f: &mut Feature, name: &str, v: i32) -> Result<(), String> {
    let i = idx(f, name)?;
    f.set_field_integer(i, v).map_err(|e| e.to_string())
}

fn set_int64(f: &mut Feature, name: &str, v: i64) -> Result<(), String> {
    let i = idx(f, name)?;
    f.set_field_integer64(i, v).map_err(|e| e.to_string())
}

fn set_real(f: &mut Feature, name: &str, v: f64) -> Result<(), String> {
    let i = idx(f, name)?;
    f.set_field_double(i, v).map_err(|e| e.to_string())
}

fn set_text(f: &mut Feature, name: &str, v: &str) -> Result<(), String> {
    let i = idx(f, name)?;
    f.set_field_string(i, v).map_err(|e| e.to_string())
}

/// A field as text ("" when null), as OGR's GetFieldAsString.
fn get_text(f: &Feature, name: &str) -> String {
    f.field_index(name).ok().and_then(|i| f.field_as_string(i).ok().flatten()).unwrap_or_default()
}

fn get_int(f: &Feature, name: &str) -> i32 {
    f.field_index(name).ok().and_then(|i| f.field_as_integer(i).ok().flatten()).unwrap_or(0)
}

fn get_int64(f: &Feature, name: &str) -> i64 {
    f.field_index(name).ok().and_then(|i| f.field_as_integer64(i).ok().flatten()).unwrap_or(0)
}

fn get_real(f: &Feature, name: &str) -> f64 {
    f.field_index(name).ok().and_then(|i| f.field_as_double(i).ok().flatten()).unwrap_or(0.0)
}

fn write_strata_table(ds: &Dataset, p: &Project) -> Result<(), String> {
    let layer = ds.layer_by_name("strata").map_err(|_| "the project has no strata table".to_string())?;
    sql(ds, "DELETE FROM strata");
    for s in &p.strata.strata {
        let mut f = Feature::new(layer.defn()).map_err(|e| e.to_string())?;
        set_int(&mut f, "code", s.code)?;
        set_text(&mut f, "name", &s.name)?;
        if s.cls != NONE {
            set_int(&mut f, "class", s.cls)?;
        }
        set_int(&mut f, "change", i32::from(s.change))?;
        set_int(&mut f, "buffer", i32::from(s.buffer))?;
        set_int64(&mut f, "pixels", s.pixels)?;
        set_real(&mut f, "area", s.area)?;
        set_real(&mut f, "weight", s.weight)?;
        set_real(&mut f, "expected_ua", s.expected_ua)?;
        set_int(&mut f, "n", s.n)?;
        f.create(&layer).map_err(|_| last_error("cannot write the strata table"))?;
    }
    Ok(())
}

fn write_project_table(ds: &Dataset, p: &Project) -> Result<(), String> {
    let layer = ds.layer_by_name("project").map_err(|_| "the project has no project table".to_string())?;
    sql(ds, "DELETE FROM project");
    let maps: Vec<Value> = p.maps.iter().map(|m| json!({"rule": m.rule.to_json(), "mapping": m.mapping})).collect();
    let mapped: Vec<Value> = p
        .strata
        .mapped
        .iter()
        .map(|m| Value::Object(m.iter().map(|(c, a)| (c.to_string(), json!(a))).collect()))
        .collect();
    let base = Path::new(&p.path).parent().unwrap_or(Path::new(""));
    let rel = relative(Path::new(&p.strata_path), base)
        .map_or_else(|| p.strata_path.clone(), |r| r.to_string_lossy().into_owned());
    let kv = [
        ("format", "janus-validation 1".to_string()),
        ("id", p.id.clone()),
        ("spec", p.spec.to_json().to_string()),
        ("grid", p.strata.grid.to_json().to_string()),
        ("unit", p.strata.unit.clone()),
        ("seed", p.seed.to_string()),
        ("design", p.design.to_string()),
        ("label_set", json!(p.label_set).to_string()),
        ("maps", Value::Array(maps).to_string()),
        ("mapped", Value::Array(mapped).to_string()),
        ("strata_raster", rel),
    ];
    for (k, v) in kv {
        let mut f = Feature::new(layer.defn()).map_err(|e| e.to_string())?;
        set_text(&mut f, "key", k)?;
        set_text(&mut f, "value", &v)?;
        f.create(&layer).map_err(|_| last_error("cannot write the project table"))?;
    }
    Ok(())
}

fn write_points(ds: &Dataset, p: &Project, pts: &[Point]) -> Result<(), String> {
    let layer = ds.layer_by_name("samples").map_err(|_| "the project has no samples layer".to_string())?;
    let names: BTreeMap<i32, &str> = p.strata.strata.iter().map(|s| (s.code, s.name.as_str())).collect();
    for q in pts {
        let mut f = Feature::new(layer.defn()).map_err(|e| e.to_string())?;
        // SAFETY: a live feature.
        unsafe { gdal_sys::OGR_F_SetFID(f.c_feature(), i64::from(q.id)) };
        set_int(&mut f, "sample_id", q.id)?;
        set_int(&mut f, "stratum", q.stratum)?;
        set_text(&mut f, "stratum_name", names.get(&q.stratum).copied().unwrap_or(""))?;
        set_int(&mut f, "row", q.row)?;
        set_int(&mut f, "col", q.col)?;
        let mut g = Geometry::empty(OGRwkbGeometryType::wkbPoint).map_err(|e| e.to_string())?;
        g.set_point_2d(0, (q.x, q.y));
        f.set_geometry(g).map_err(|e| e.to_string())?;
        f.create(&layer).map_err(|_| last_error("cannot write the sample points"))?;
    }
    Ok(())
}

/// Writes the whole project (over an existing file).
pub fn create_project(p: &Project) -> Result<(), String> {
    let drv = DriverManager::get_driver_by_name("GPKG").map_err(|_| "GDAL has no GeoPackage driver".to_string())?;
    let _ = std::fs::remove_file(&p.path);
    {
        let mut ds = drv.create_vector_only(&p.path).map_err(|e| format!("cannot create {}: {e}", p.path))?;
        let mut srs = if p.strata.grid.wkt.is_empty() { None } else { SpatialRef::from_wkt(&p.strata.grid.wkt).ok() };
        if let Some(s) = &mut srs {
            s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        }
        use OGRFieldType::*;
        // name, points (else no geometry), fields
        type Table<'a> = (&'a str, bool, Vec<(&'a str, OGRFieldType::Type)>);
        let tables: [Table; 4] = [
            (
                "samples",
                true,
                vec![
                    ("sample_id", OFTInteger),
                    ("stratum", OFTInteger),
                    ("stratum_name", OFTString),
                    ("row", OFTInteger),
                    ("col", OFTInteger),
                    ("label", OFTString),
                    ("ref_date", OFTString),
                    ("confidence", OFTInteger),
                    ("notes", OFTString),
                    ("events", OFTString),
                    ("interpreters", OFTInteger),
                    ("status", OFTString),
                ],
            ),
            (
                "labels",
                false,
                vec![
                    ("sample_id", OFTInteger),
                    ("interpreter", OFTString),
                    ("label", OFTString),
                    ("date", OFTString),
                    ("confidence", OFTInteger),
                    ("notes", OFTString),
                    ("events", OFTString),
                    ("updated", OFTString),
                ],
            ),
            (
                "strata",
                false,
                vec![
                    ("code", OFTInteger),
                    ("name", OFTString),
                    ("class", OFTInteger),
                    ("change", OFTInteger),
                    ("buffer", OFTInteger),
                    ("pixels", OFTInteger64),
                    ("area", OFTReal),
                    ("weight", OFTReal),
                    ("expected_ua", OFTReal),
                    ("n", OFTInteger),
                ],
            ),
            ("project", false, vec![("key", OFTString), ("value", OFTString)]),
        ];
        for (name, points, fields) in &tables {
            let layer = ds
                .create_layer(LayerOptions {
                    name,
                    srs: if *points { srs.as_ref() } else { None },
                    ty: if *points { OGRwkbGeometryType::wkbPoint } else { OGRwkbGeometryType::wkbNone },
                    options: None,
                })
                .map_err(|_| last_error("cannot create the project's tables"))?;
            layer.create_defn_fields(fields).map_err(|_| last_error("cannot create the project's tables"))?;
        }
        let t = Txn::start(&ds);
        write_points(&ds, p, &p.points)?;
        write_strata_table(&ds, p)?;
        write_project_table(&ds, p)?;
        t.commit(&format!("cannot write {}", p.path))?;
        sql(&ds, "CREATE INDEX labels_point ON labels(sample_id, interpreter)");
        sql(&ds, "CREATE INDEX samples_id ON samples(sample_id)");
    }
    // labels already given (a project written again, e.g. merged)
    let mut finals = Vec::new();
    for i in 0..p.points.len() {
        let (r, conflict) = resolve(&p.labels, i);
        let f = FinalRef {
            point: i,
            reference: r.cloned().unwrap_or_default(),
            conflict,
            interpreters: p.labels.values().filter(|v| v.get(i).is_some_and(|l| !l.label.is_empty())).count() as i32,
        };
        if r.is_some() || conflict {
            finals.push(f);
        }
    }
    let mut first = true;
    for (who, v) in &p.labels {
        let rows: Vec<(usize, Label)> =
            v.iter().enumerate().filter(|(_, l)| !l.is_empty()).map(|(i, l)| (i, l.clone())).collect();
        if rows.is_empty() {
            continue;
        }
        save_labels(&p.path, who, &rows, if first { &finals } else { &[] })?;
        first = false;
    }
    Ok(())
}

/// The project and strata tables (maps, translations, label set, design).
pub fn save_project_meta(p: &Project) -> Result<(), String> {
    let ds = open_gpkg(&p.path, true)?;
    let t = Txn::start(&ds);
    write_strata_table(&ds, p)?;
    write_project_table(&ds, p)?;
    t.commit(&format!("cannot write {}", p.path))
}

/// New points (a larger sample), with the strata table.
pub fn append_points(p: &Project, pts: &[Point]) -> Result<(), String> {
    let ds = open_gpkg(&p.path, true)?;
    let t = Txn::start(&ds);
    write_points(&ds, p, pts)?;
    write_strata_table(&ds, p)?;
    t.commit(&format!("cannot write {}", p.path))
}

/// An interpreter's labels of some points (index, label) and the reference that follows for them (`finals`) on
/// the samples layer.
pub fn save_labels(path: &str, interpreter: &str, rows: &[(usize, Label)], finals: &[FinalRef]) -> Result<(), String> {
    let ds = open_gpkg(path, true)?;
    let layer = ds.layer_by_name("labels").map_err(|_| format!("{path}: no labels table"))?;
    let t = Txn::start(&ds);
    if !rows.is_empty() {
        let ids: Vec<String> = rows.iter().map(|(i, _)| (i + 1).to_string()).collect();
        sql(
            &ds,
            &format!(
                "DELETE FROM labels WHERE interpreter = {} AND sample_id IN ({})",
                sql_text(interpreter),
                ids.join(",")
            ),
        );
    }
    for (i, l) in rows {
        if l.is_empty() {
            continue;
        }
        let mut f = Feature::new(layer.defn()).map_err(|e| e.to_string())?;
        set_int(&mut f, "sample_id", *i as i32 + 1)?;
        set_text(&mut f, "interpreter", interpreter)?;
        set_text(&mut f, "label", &l.label)?;
        set_text(&mut f, "date", &l.date)?;
        set_int(&mut f, "confidence", l.confidence)?;
        set_text(&mut f, "notes", &l.notes)?;
        set_text(&mut f, "events", &events_text(&l.events))?;
        set_text(&mut f, "updated", &l.updated)?;
        f.create(&layer).map_err(|_| last_error("cannot write the labels"))?;
    }
    let or_null = |s: &str| if s.is_empty() { "NULL".to_string() } else { sql_text(s) };
    for f in finals {
        let r = &f.reference;
        let status = if f.conflict {
            "conflict"
        } else if r.label.is_empty() {
            ""
        } else {
            "labelled"
        };
        let q = format!(
            "UPDATE samples SET label = {}, ref_date = {}, confidence = {}, notes = {}, events = {}, interpreters = {}, status = {} WHERE sample_id = {}",
            or_null(&r.label),
            or_null(&r.date),
            if r.confidence != 0 { r.confidence.to_string() } else { "NULL".into() },
            or_null(&r.notes),
            or_null(&events_text(&r.events)),
            f.interpreters,
            or_null(status),
            f.point + 1
        );
        sql(&ds, &q);
    }
    t.commit(&format!("cannot write the labels to {path}"))
}

fn read_keys(ds: &Dataset) -> BTreeMap<String, String> {
    let mut kv = BTreeMap::new();
    if let Ok(mut l) = ds.layer_by_name("project") {
        for f in l.features() {
            kv.insert(get_text(&f, "key"), get_text(&f, "value"));
        }
    }
    kv
}

fn read_label_table(ds: &Dataset, points: usize, out: &mut BTreeMap<String, Vec<Label>>) -> bool {
    let Ok(mut l) = ds.layer_by_name("labels") else { return false };
    for f in l.features() {
        let id = get_int(&f, "sample_id");
        let who = get_text(&f, "interpreter");
        if id >= 1 && id as usize <= points && !who.is_empty() {
            let v = out.entry(who).or_default();
            v.resize(points, Label::default());
            v[id as usize - 1] = Label {
                label: get_text(&f, "label"),
                date: get_text(&f, "date"),
                confidence: get_int(&f, "confidence"),
                notes: get_text(&f, "notes"),
                events: events_from(&get_text(&f, "events")),
                updated: get_text(&f, "updated"),
            };
        }
    }
    true
}

pub fn load_project(path: &str) -> Result<Project, String> {
    let ds = open_gpkg(path, false)?;
    let kv = read_keys(&ds);
    let get = |k: &str| kv.get(k).cloned().unwrap_or_default();
    if !get("format").starts_with("janus-validation") {
        return Err(format!("{path} is not a validation project of Janus"));
    }
    let abs = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let mut p = Project { path: normal(&abs).to_string_lossy().into_owned(), ..Default::default() };
    let parse = |k: &str| -> Result<Value, String> {
        let s = get(k);
        if s.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&s).map_err(|e| format!("{path}: damaged project table ({e})"))
    };
    p.id = get("id");
    p.spec = StrataSpec::from_json(&parse("spec")?);
    p.strata.grid = Grid::from_json(&parse("grid")?);
    p.strata.unit = get("unit");
    p.seed = get("seed").trim().parse().unwrap_or(0);
    p.design = match parse("design")? {
        Value::Null => json!({}),
        d => d,
    };
    p.label_set =
        parse("label_set")?.as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
    for m in parse("maps")?.as_array().into_iter().flatten() {
        p.maps.push(AssessedMap {
            rule: MapRule::from_json(m.get("rule").unwrap_or(&json!({}))),
            mapping: m
                .get("mapping")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect(),
        });
    }
    for m in parse("mapped")?.as_array().into_iter().flatten() {
        let o = m
            .as_object()
            .into_iter()
            .flatten()
            .map(|(c, a)| (crate::rules::atoi(c), a.as_f64().unwrap_or(0.0)))
            .collect();
        p.strata.mapped.push(o);
    }
    let raster = PathBuf::from(get("strata_raster"));
    let raster =
        if raster.is_absolute() { raster } else { Path::new(&p.path).parent().unwrap_or(Path::new("")).join(raster) };
    p.strata_path = normal(&raster).to_string_lossy().into_owned();
    p.strata.row_area = row_areas(&p.strata.grid).0;
    if let Ok(mut l) = ds.layer_by_name("strata") {
        for f in l.features() {
            let cls = f.field_index("class").ok().and_then(|i| f.field_as_integer(i).ok().flatten()).unwrap_or(NONE);
            p.strata.strata.push(Stratum {
                code: get_int(&f, "code"),
                name: get_text(&f, "name"),
                cls,
                change: get_int(&f, "change") != 0,
                buffer: get_int(&f, "buffer") != 0,
                pixels: get_int64(&f, "pixels"),
                area: get_real(&f, "area"),
                weight: get_real(&f, "weight"),
                expected_ua: get_real(&f, "expected_ua"),
                n: get_int(&f, "n"),
            });
        }
    }
    let Ok(mut samples) = ds.layer_by_name("samples") else {
        return Err(format!("{path}: no samples layer or no strata"));
    };
    if p.strata.strata.is_empty() {
        return Err(format!("{path}: no samples layer or no strata"));
    }
    let mut pts = Vec::new();
    for f in samples.features() {
        let mut q = Point {
            id: get_int(&f, "sample_id"),
            stratum: get_int(&f, "stratum"),
            row: get_int(&f, "row"),
            col: get_int(&f, "col"),
            ..Default::default()
        };
        match f.geometry().filter(|g| g.geometry_type() == OGRwkbGeometryType::wkbPoint) {
            Some(g) => {
                let (x, y, _) = g.get_point(0);
                (q.x, q.y) = (x, y);
            }
            None => (q.x, q.y) = p.strata.grid.centre(i64::from(q.col), i64::from(q.row)),
        }
        pts.push(q);
    }
    pts.sort_by_key(|q| q.id);
    if pts.iter().enumerate().any(|(i, q)| q.id != i as i32 + 1) {
        return Err(format!(
            "{path}: the sample ids must run from 1 to the number of points (was the samples layer edited?)"
        ));
    }
    p.points = pts;
    read_label_table(&ds, p.points.len(), &mut p.labels);
    for v in p.labels.values_mut() {
        v.resize(p.points.len(), Label::default());
    }
    Ok(p)
}

/// A GeoPackage written by create_project (opened as a project, not as vector layers).
pub fn is_project(path: &str) -> bool {
    let is_gpkg = Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("gpkg"));
    if !is_gpkg {
        return false;
    }
    let Ok(ds) = open_gpkg(path, false) else { return false };
    if ds.layer_by_name("project").is_err() {
        return false;
    }
    read_keys(&ds).get("format").is_some_and(|f| f.starts_with("janus-validation"))
}

/// The labels of another copy of the project (to merge): every interpreter's, by point; and its project id.
pub fn read_labels(path: &str, points: usize) -> Result<(String, BTreeMap<String, Vec<Label>>), String> {
    let ds = open_gpkg(path, false)?;
    let id = read_keys(&ds).get("id").cloned().unwrap_or_default();
    let mut out = BTreeMap::new();
    if !read_label_table(&ds, points, &mut out) {
        return Err(format!("{path}: no labels table"));
    }
    Ok((id, out))
}
