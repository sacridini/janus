//! The strata of one map (its classes) or of several (how many call a pixel change), a buffer around change, a
//! limit; their raster, pixels and areas; the areas of a map's classes; a map's values at points
//! (src/validation.cpp: computeStrata, countStrata, mappedAreas, valuesAt).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;

use gdal::DriverManager;
use gdal::raster::{ColorEntry, ColorTable, PaletteInterpretation, RasterCreationOptions};
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_geo::plane::{Polygon, Ring};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::reader::{MapReader, RowAcc, Spans, band_rows, open_raster};
use crate::rules::{Grid, MapRule, grid_of, row_areas, same_crs};
use crate::{NONE, Progress};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StrataSpec {
    /// one: its classes are the strata; several: how many call a pixel change ("k of M")
    pub maps: Vec<MapRule>,
    /// > 0: pixels without change within this many pixels of a change (Chebyshev) form a stratum
    pub buffer: i32,
    /// only pixels whose centres are inside (the first map's pixels); empty: every pixel
    pub limit: Polygon,
}

impl StrataSpec {
    pub fn to_json(&self) -> Value {
        json!({
            "maps": self.maps.iter().map(MapRule::to_json).collect::<Vec<_>>(),
            "buffer": self.buffer,
            "limit": self.limit,
        })
    }

    pub fn from_json(j: &Value) -> StrataSpec {
        let mut s = StrataSpec {
            maps: j.get("maps").and_then(Value::as_array).into_iter().flatten().map(MapRule::from_json).collect(),
            buffer: j.get("buffer").and_then(Value::as_i64).unwrap_or(0) as i32,
            limit: Vec::new(),
        };
        for r in j.get("limit").and_then(Value::as_array).into_iter().flatten() {
            let ring: Ring = r.as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
            s.limit.push(ring);
        }
        s
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stratum {
    /// its value in the strata raster (1-255; 0 = outside every stratum)
    pub code: i32,
    pub name: String,
    /// one map: the class of the map it holds
    pub cls: i32,
    /// its pixels are change
    pub change: bool,
    /// the buffer stratum
    pub buffer: bool,
    pub pixels: i64,
    pub area: f64,
    /// share of the area
    pub weight: f64,
    /// design: the user's accuracy expected of it
    pub expected_ua: f64,
    /// design: points in it
    pub n: i32,
}

impl Default for Stratum {
    fn default() -> Self {
        Stratum {
            code: 0,
            name: String::new(),
            cls: NONE,
            change: false,
            buffer: false,
            pixels: 0,
            area: 0.0,
            weight: 0.0,
            expected_ua: 0.75,
            n: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Strata {
    pub grid: Grid,
    /// in display order
    pub strata: Vec<Stratum>,
    /// of the areas
    pub unit: String,
    /// area of a pixel on each row
    pub row_area: Vec<f64>,
    /// [stratum][row] pixels (empty unless counted)
    pub row_counts: Vec<Vec<i32>>,
    /// [map of the spec] class -> area inside the strata
    pub mapped: Vec<BTreeMap<i32, f64>>,
}

impl Default for Strata {
    fn default() -> Self {
        Strata {
            grid: Grid::default(),
            strata: Vec::new(),
            unit: "ha".into(),
            row_area: Vec::new(),
            row_counts: Vec::new(),
            mapped: Vec::new(),
        }
    }
}

impl Strata {
    pub fn total_area(&self) -> f64 {
        self.strata.iter().map(|s| s.area).sum()
    }

    /// The index of the stratum of `code`, if any.
    pub fn index_of_code(&self, code: i32) -> Option<usize> {
        self.strata.iter().position(|s| s.code == code)
    }
}

const CHANGE_COLOURS: [[i16; 3]; 8] = [
    [215, 48, 39],
    [244, 109, 67],
    [253, 174, 97],
    [165, 0, 38],
    [252, 78, 42],
    [227, 26, 28],
    [189, 0, 38],
    [240, 59, 32],
];
const OTHER_COLOURS: [[i16; 3]; 8] = [
    [26, 152, 80],
    [145, 207, 96],
    [69, 117, 180],
    [116, 173, 209],
    [102, 189, 99],
    [153, 112, 171],
    [128, 128, 128],
    [77, 146, 33],
];

fn cancelled(p: Option<&Progress>) -> bool {
    p.is_some_and(|p| p.cancel.load(Ordering::Relaxed))
}

fn threads_of(p: Option<&Progress>) -> usize {
    p.map_or(1, |p| p.threads)
}

/// One pass over the maps at full resolution, in bands of rows (other maps read on the first one's grid): the
/// raster of the strata written to `out` (GTiff, Byte, 0 = no stratum, with the strata's names), the pixels and
/// areas of each stratum per row, and the areas of every map's classes inside the strata.
pub fn compute_strata(spec: &StrataSpec, out: &str, p: Option<&Progress>) -> Result<Strata, String> {
    if spec.maps.is_empty() {
        return Err("no map to stratify".into());
    }
    let mut r = Strata::default();
    let m = spec.maps.len();
    let mut readers = Vec::with_capacity(m);
    for (k, map) in spec.maps.iter().enumerate() {
        let rd = MapReader::open(&map.path, map.band, (k > 0).then_some(&r.grid), threads_of(p))?;
        if k == 0 {
            r.grid = rd.own().clone();
        }
        readers.push(rd);
    }
    let (w, h) = (r.grid.w, r.grid.h);
    (r.row_area, r.unit) = row_areas(&r.grid);

    let drv = DriverManager::get_driver_by_name("GTiff").map_err(|_| "GDAL has no GTiff driver".to_string())?;
    let mut co = RasterCreationOptions::new();
    for (k, v) in [("TILED", "YES"), ("COMPRESS", "DEFLATE"), ("BIGTIFF", "IF_SAFER")] {
        co.set_name_value(k, v).map_err(|e| e.to_string())?;
    }
    co.set_name_value("NUM_THREADS", &threads_of(p).max(1).to_string()).map_err(|e| e.to_string())?;
    let mut o = drv
        .create_with_band_type_with_options::<u8, _>(out, w, h, 1, &co)
        .map_err(|e| format!("cannot write {out}: {e}"))?;
    // the raster removed unless it is finished
    struct Unlink<'a>(&'a str, bool);
    impl Drop for Unlink<'_> {
        fn drop(&mut self) {
            if !self.1 {
                let _ = std::fs::remove_file(self.0);
            }
        }
    }
    let mut keep = Unlink(out, false);
    if r.grid.has_gt {
        let _ = o.set_geo_transform(&r.grid.gt);
    }
    if !r.grid.wkt.is_empty() {
        let _ = o.set_projection(&r.grid.wkt);
    }
    let result = (|| -> Result<Strata, String> {
        let mut ob = o.rasterband(1).map_err(|e| e.to_string())?;
        let _ = ob.set_no_data_value(Some(0.0));
        // a palette from the start: its colours are set at the end, and GTiff cannot make a raster a palette
        // once it has written blocks
        let mut blank = ColorTable::new(PaletteInterpretation::Rgba);
        blank.set_color_entry(0, &ColorEntry::rgba(0, 0, 0, 0));
        ob.set_color_table(&blank);

        let buf = spec.buffer.max(0) as usize;
        let rows_per = band_rows(w, m, buf);
        let pool =
            rayon::ThreadPoolBuilder::new().num_threads(threads_of(p).max(1)).build().map_err(|e| e.to_string())?;
        let maps = &spec.maps;
        let mut vals: Vec<Vec<f32>> = vec![Vec::new(); m];
        let (mut key, mut chg): (Vec<i32>, Vec<u8>) = (Vec::new(), Vec::new());
        let (mut hd, mut near): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
        let mut tokens: Vec<i32> = Vec::new();
        let mut out_rows: Vec<u8> = Vec::new();
        let mut inside: Vec<bool> = Vec::new();
        let mut row_in = vec![true; w];
        let mut code_of_key: BTreeMap<i32, i32> = BTreeMap::new();
        let (mut buffer_code, mut next_code) = (0, 1);
        let mut counts: Vec<Vec<i32>> = vec![Vec::new(); 256];
        let mut mapped: Vec<BTreeMap<i32, f64>> = vec![BTreeMap::new(); m];
        let mut spans = Spans::new(&spec.limit);
        let limited = !spans.is_empty();
        const BUFFER: i32 = i32::MIN + 1; // the buffer's token (no class is that low)

        // Each band in four steps, the C++'s single pass made parallel with the same result: the classes (and the
        // buffer) of every pixel, rows in parallel; each row's tokens in the order they first appear, merged in row
        // order to give the codes in the order a pass along the rows meets them; the codes and each row's pixels of
        // every class, rows in parallel; the counts and areas added row by row, in order (the same sums).
        for y0 in (0..h).step_by(rows_per) {
            if cancelled(p) {
                return Err("cancelled".into());
            }
            let y1 = h.min(y0 + rows_per);
            let (r0, r1) = (y0.saturating_sub(buf), h.min(y1 + buf));
            let (rr, ny) = (r1 - r0, y1 - y0);
            for k in 0..m {
                vals[k].resize(w * rr, 0.0);
                readers[k]
                    .rows(r0, rr, &mut vals[k])
                    .map_err(|e| format!("{}: read failed: {e}", spec.maps[k].path))?;
            }
            key.resize(w * rr, NONE);
            chg.resize(w * rr, 0);
            let vals_ref = &vals;
            pool.install(|| {
                key.par_chunks_mut(w).zip(chg.par_chunks_mut(w)).enumerate().for_each(|(row, (kr, cr))| {
                    let base = row * w;
                    for x in 0..w {
                        if m == 1 {
                            let c = maps[0].class_of(f64::from(vals_ref[0][base + x]));
                            kr[x] = c;
                            cr[x] = u8::from(c != NONE && maps[0].is_change(c));
                            continue;
                        }
                        let mut n = 0;
                        let mut any = true;
                        for k in 0..m {
                            let c = maps[k].class_of(f64::from(vals_ref[k][base + x]));
                            if c == NONE {
                                any = false;
                                break;
                            }
                            n += i32::from(maps[k].is_change(c));
                        }
                        (kr[x], cr[x]) = if any { (n, u8::from(n > 0)) } else { (NONE, 0) };
                    }
                });
            });
            // the buffer: change dilated by `buf` pixels along the rows, then down the columns (the rows within
            // buf of each row)
            if buf > 0 {
                hd.resize(w * rr, 0);
                near.resize(w * ny, 0);
                let (chg, hd_ref) = (&chg, &mut hd);
                pool.install(|| {
                    hd_ref.par_chunks_mut(w).zip(chg.par_chunks(w)).for_each_init(
                        || vec![0i64; w + 1],
                        |pre, (hrow, c)| {
                            for x in 0..w {
                                pre[x + 1] = pre[x] + i64::from(c[x]);
                            }
                            for x in 0..w {
                                hrow[x] = u8::from(pre[w.min(x + buf + 1)] - pre[x.saturating_sub(buf)] > 0);
                            }
                        },
                    );
                });
                let hd = &hd;
                pool.install(|| {
                    near.par_chunks_mut(w).enumerate().for_each(|(j, nrow)| {
                        let y = y0 + j;
                        nrow.fill(0);
                        for yy in y.saturating_sub(buf).max(r0)..(y + buf + 1).min(r1) {
                            for (n, &v) in nrow.iter_mut().zip(&hd[(yy - r0) * w..(yy - r0 + 1) * w]) {
                                *n |= v;
                            }
                        }
                    });
                });
            }
            if limited {
                inside.resize(w * ny, false);
                for y in y0..y1 {
                    spans.row(y, w, &mut row_in);
                    inside[(y - y0) * w..(y - y0 + 1) * w].copy_from_slice(&row_in);
                }
            }
            // each pixel's token (a class, or the buffer), and each row's tokens in the order they first appear
            tokens.resize(w * ny, NONE);
            let (key, chg, near, inside) = (&key, &chg, &near, &inside);
            let firsts: Vec<Vec<i32>> = pool.install(|| {
                tokens
                    .par_chunks_mut(w)
                    .enumerate()
                    .map(|(j, tr)| {
                        let base = (y0 + j - r0) * w;
                        let (mut seen, mut last) = (Vec::new(), NONE);
                        for x in 0..w {
                            let kv = key[base + x];
                            let t = if kv == NONE || (limited && !inside[j * w + x]) {
                                NONE
                            } else if buf > 0 && chg[base + x] == 0 && near[j * w + x] != 0 {
                                BUFFER
                            } else {
                                kv
                            };
                            tr[x] = t;
                            // (more than 256 tokens is an error below: no need to keep them all)
                            if t != NONE && t != last && seen.len() <= 256 && !seen.contains(&t) {
                                seen.push(t);
                            }
                            last = t;
                        }
                        seen
                    })
                    .collect()
            });
            for t in firsts.into_iter().flatten() {
                if t == BUFFER {
                    if buffer_code == 0 {
                        buffer_code = next_code;
                        next_code += 1;
                    }
                } else if let std::collections::btree_map::Entry::Vacant(e) = code_of_key.entry(t) {
                    e.insert(next_code);
                    next_code += 1;
                }
                if next_code > 256 {
                    return Err("more than 255 strata: give the map's values as intervals".into());
                }
            }
            let mut lut: HashMap<i32, u8> = code_of_key.iter().map(|(&k, &c)| (k, c as u8)).collect();
            lut.insert(BUFFER, buffer_code as u8);
            // the codes, and each row's pixels of every code and of every map's classes
            out_rows.resize(w * ny, 0);
            let (tokens, lut, vals_ref) = (&tokens, &lut, &vals);
            type RowCounts = (Vec<(u8, i32)>, Vec<Vec<(i32, i64)>>);
            let rows: Vec<RowCounts> = pool.install(|| {
                out_rows
                    .par_chunks_mut(w)
                    .enumerate()
                    .map(|(j, orow)| {
                        let base = (y0 + j - r0) * w;
                        let tr = &tokens[j * w..(j + 1) * w];
                        let mut cnt = [0i32; 256];
                        let mut acc: Vec<RowAcc> = (0..m).map(|_| RowAcc::default()).collect();
                        let (mut lt, mut lc) = (NONE, 0u8);
                        for x in 0..w {
                            let t = tr[x];
                            if t == NONE {
                                orow[x] = 0;
                                continue;
                            }
                            if t != lt {
                                (lt, lc) = (t, lut[&t]);
                            }
                            orow[x] = lc;
                            cnt[lc as usize] += 1;
                            for k in 0..m {
                                acc[k].add(if m == 1 {
                                    key[base + x]
                                } else {
                                    maps[k].class_of(f64::from(vals_ref[k][base + x]))
                                });
                            }
                        }
                        let codes = (1..256).filter(|&c| cnt[c] > 0).map(|c| (c as u8, cnt[c])).collect();
                        (codes, acc.iter_mut().map(RowAcc::take).collect())
                    })
                    .collect()
            });
            for (j, (codes, classes)) in rows.into_iter().enumerate() {
                let y = y0 + j;
                for (c, n) in codes {
                    let cc = &mut counts[c as usize];
                    if cc.is_empty() {
                        cc.resize(h, 0);
                    }
                    cc[y] += n;
                }
                for (k, row) in classes.into_iter().enumerate() {
                    for (c, n) in row {
                        *mapped[k].entry(c).or_insert(0.0) += n as f64 * r.row_area[y];
                    }
                }
            }
            let mut buffer = gdal::raster::Buffer::new((w, ny), std::mem::take(&mut out_rows));
            ob.write((0, y0 as isize), (w, ny), &mut buffer).map_err(|e| format!("cannot write {out}: {e}"))?;
            out_rows = buffer.into_shape_and_vec().1;
            if let Some(p) = p {
                p.set_done(y1 as f64 / h as f64);
            }
        }
        // the strata in order: by class (or by how many maps call change), the buffer last
        for (&k, &code) in &code_of_key {
            let mut s = Stratum { code, ..Default::default() };
            if m == 1 {
                s.cls = k;
                s.name = spec.maps[0].class_name(k);
                s.change = spec.maps[0].is_change(k);
            } else {
                s.change = k > 0;
                s.name = if k == 0 {
                    "no change in any map".into()
                } else if k as usize == m {
                    format!("change in all {m} maps")
                } else {
                    format!("change in {k} of {m} maps")
                };
            }
            r.strata.push(s);
        }
        if buffer_code != 0 {
            r.strata.push(Stratum {
                code: buffer_code,
                buffer: true,
                name: format!("buffer ({buf} px)"),
                ..Default::default()
            });
        }
        let mut total = 0.0;
        for s in &mut r.strata {
            let c = &counts[s.code as usize];
            for y in 0..h {
                s.pixels += i64::from(c[y]);
                s.area += f64::from(c[y]) * r.row_area[y];
            }
            total += s.area;
            r.row_counts.push(c.clone());
        }
        for s in &mut r.strata {
            s.weight = if total > 0.0 { s.area / total } else { 0.0 };
        }
        r.mapped = mapped;
        if r.strata.is_empty() {
            return Err("no pixel of the map falls in a stratum (no data everywhere, or outside the limit)".into());
        }
        // names and colours for other programs (QGIS) and for the map
        let mut names = vec![String::new(); next_code as usize];
        let mut ct = ColorTable::new(PaletteInterpretation::Rgba);
        ct.set_color_entry(0, &ColorEntry::rgba(0, 0, 0, 0));
        let (mut nc, mut no) = (0, 0);
        for s in &r.strata {
            names[s.code as usize] = s.name.clone();
            let c = if s.buffer {
                [254, 224, 139]
            } else if s.change {
                nc += 1;
                CHANGE_COLOURS[(nc - 1) % CHANGE_COLOURS.len()]
            } else {
                no += 1;
                OTHER_COLOURS[(no - 1) % OTHER_COLOURS.len()]
            };
            ct.set_color_entry(s.code as u16, &ColorEntry::rgba(c[0], c[1], c[2], 255));
        }
        set_category_names(&ob, &names);
        // GTiff sets the photometric tag again, refused once blocks are written (it is a palette already)
        // SAFETY: GDAL's error handler stack, pushed and popped around the call.
        unsafe { gdal_sys::CPLPushErrorHandler(Some(gdal_sys::CPLQuietErrorHandler)) };
        ob.set_color_table(&ct);
        unsafe { gdal_sys::CPLPopErrorHandler() };
        Ok(std::mem::take(&mut r))
    })();
    drop(o);
    if result.is_ok() {
        keep.1 = true;
    }
    result
}

/// GDALSetRasterCategoryNames (the gdal crate has no wrapper).
fn set_category_names(b: &gdal::raster::RasterBand, names: &[String]) {
    let mut l = gdal::cpl::CslStringList::new();
    for n in names {
        let _ = l.add_string(n);
    }
    // SAFETY: a live band; GDAL copies the list.
    unsafe {
        gdal_sys::GDALSetRasterCategoryNames(b.c_rasterband(), l.as_ptr());
    }
}

/// The pixels of each stratum per row, read from the strata raster (for a sample drawn later).
pub fn count_strata(strata_path: &str, s: &mut Strata, p: Option<&Progress>) -> Result<(), String> {
    let rd = MapReader::open(strata_path, 1, None, threads_of(p))?;
    if rd.w != s.grid.w || rd.h != s.grid.h {
        return Err(format!("{strata_path}: not the size of the project's strata"));
    }
    let (w, h) = (rd.w, rd.h);
    let rows_per = band_rows(w, 1, 0);
    let mut idx = [usize::MAX; 256];
    for (i, t) in s.strata.iter().enumerate() {
        idx[(t.code & 255) as usize] = i;
    }
    s.row_counts = vec![vec![0; h]; s.strata.len()];
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads_of(p).max(1)).build().map_err(|e| e.to_string())?;
    let mut v = Vec::new();
    for y0 in (0..h).step_by(rows_per) {
        if cancelled(p) {
            return Err("cancelled".into());
        }
        let n = rows_per.min(h - y0);
        v.resize(w * n, 0.0);
        rd.rows(y0, n, &mut v).map_err(|e| format!("{strata_path}: read failed: {e}"))?;
        // each row's pixels of every code, rows in parallel
        let rows: Vec<[i32; 256]> = pool.install(|| {
            v.par_chunks(w)
                .map(|row| {
                    let mut cnt = [0i32; 256];
                    for &c in row {
                        if (1.0..=255.0).contains(&c) {
                            cnt[c as usize] += 1;
                        }
                    }
                    cnt
                })
                .collect()
        });
        for (r, cnt) in rows.iter().enumerate() {
            for c in 1..256 {
                if cnt[c] > 0 && idx[c] != usize::MAX {
                    s.row_counts[idx[c]][y0 + r] += cnt[c];
                }
            }
        }
        if let Some(p) = p {
            p.set_done((y0 + n) as f64 / h as f64);
        }
    }
    Ok(())
}

/// The area of each class of `map` over the pixels of the strata raster that are in a stratum (the study area),
/// the map read on the strata's grid.
pub fn mapped_areas(
    map: &MapRule,
    strata_path: &str,
    s: &Strata,
    p: Option<&Progress>,
) -> Result<BTreeMap<i32, f64>, String> {
    let st = MapReader::open(strata_path, 1, None, threads_of(p))?;
    let mr = MapReader::open(&map.path, map.band, Some(&s.grid), threads_of(p))?;
    let (w, h) = (st.w, st.h);
    let rows_per = band_rows(w, 2, 0);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads_of(p).max(1)).build().map_err(|e| e.to_string())?;
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut area = BTreeMap::new();
    for y0 in (0..h).step_by(rows_per) {
        if cancelled(p) {
            return Err("cancelled".into());
        }
        let n = rows_per.min(h - y0);
        a.resize(w * n, 0.0);
        b.resize(w * n, 0.0);
        st.rows(y0, n, &mut a)
            .and_then(|_| mr.rows(y0, n, &mut b))
            .map_err(|e| format!("{}: read failed: {e}", map.path))?;
        // each row's pixels of every class, rows in parallel, then the areas added row by row (the same sums)
        let rows: Vec<Vec<(i32, i64)>> = pool.install(|| {
            a.par_chunks(w)
                .zip(b.par_chunks(w))
                .map(|(ar, br)| {
                    let mut acc = RowAcc::default();
                    for x in 0..w {
                        if !(ar[x] >= 1.0) {
                            continue;
                        }
                        let c = map.class_of(f64::from(br[x]));
                        if c != NONE {
                            acc.add(c);
                        }
                    }
                    acc.take()
                })
                .collect()
        });
        for (r, row) in rows.into_iter().enumerate() {
            let ra = s.row_area.get(y0 + r).copied().unwrap_or(1.0);
            for (c, k) in row {
                *area.entry(c).or_insert(0.0) += k as f64 * ra;
            }
        }
        if let Some(p) = p {
            p.set_done((y0 + n) as f64 / h as f64);
        }
    }
    Ok(area)
}

/// The raw values of `map` at points (x, y in the CRS `wkt`): NaN off the map or no data.
pub fn values_at(map: &MapRule, wkt: &str, xy: &[[f64; 2]]) -> Result<Vec<f64>, String> {
    let ds = open_raster(&map.path, 1)?;
    if map.band < 1 || map.band as usize > ds.raster_count() {
        return Err(format!("{}: no band {}", map.path, map.band));
    }
    let g = grid_of(&ds);
    let mut inv = [0f64; 6];
    let mut gt = g.gt;
    // SAFETY: two arrays of 6 doubles.
    if unsafe { gdal_sys::GDALInvGeoTransform(gt.as_mut_ptr(), inv.as_mut_ptr()) } == 0 {
        return Err(format!("{}: its transform cannot be inverted", map.path));
    }
    let mut x: Vec<f64> = xy.iter().map(|p| p[0]).collect();
    let mut y: Vec<f64> = xy.iter().map(|p| p[1]).collect();
    let mut ok = vec![true; xy.len()];
    if !same_crs(wkt, &g.wkt) {
        let mut a = SpatialRef::from_wkt(wkt).map_err(|e| e.to_string())?;
        let mut b = SpatialRef::from_wkt(&g.wkt).map_err(|e| e.to_string())?;
        a.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        b.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let ct =
            CoordTransform::new(&a, &b).map_err(|_| format!("{}: no transformation from the points' CRS", map.path))?;
        // point by point: one that fails does not lose the others
        for i in 0..xy.len() {
            let (mut px, mut py) = ([x[i]], [y[i]]);
            ok[i] = ct.transform_coords(&mut px, &mut py, &mut []).is_ok();
            (x[i], y[i]) = (px[0], py[0]);
        }
    }
    let band = ds.rasterband(map.band as usize).map_err(|e| e.to_string())?;
    let nd = band.no_data_value().filter(|v| !v.is_nan());
    // in row order: the blocks stay in GDAL's cache
    let mut px: Vec<Option<(usize, usize)>> = vec![None; xy.len()];
    for i in 0..xy.len() {
        if !ok[i] {
            continue;
        }
        let c = inv[0] + x[i] * inv[1] + y[i] * inv[2];
        let r = inv[3] + x[i] * inv[4] + y[i] * inv[5];
        if c >= 0.0 && r >= 0.0 && c < g.w as f64 && r < g.h as f64 {
            px[i] = Some((c.floor() as usize, r.floor() as usize));
        }
    }
    let mut order: Vec<usize> = (0..xy.len()).collect();
    order.sort_by_key(|&i| px[i].map_or(-1, |p| p.1 as i64));
    let mut out = vec![f64::NAN; xy.len()];
    let mut v = [0f64];
    for i in order {
        let Some((c, r)) = px[i] else { continue };
        if band.read_into_slice::<f64>((c as isize, r as isize), (1, 1), (1, 1), &mut v, None).is_err() {
            continue;
        }
        out[i] = if nd.is_some_and(|nd| v[0] == nd) { f64::NAN } else { v[0] };
    }
    Ok(out)
}
