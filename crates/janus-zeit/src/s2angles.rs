//! Sentinel-2's sun and view angles of every pixel from a tile's `MTD_TL.xml`, as Zeit's `zeit.sentinel2_angles`
//! gives them (a port of its `_s2angles.py`, our own code): ESA gives each tile's angles as 23 x 23 grids with a
//! 5 km step, the sun's zenith and azimuth and the view zenith and azimuth of every band and detector (a detector's
//! grid is NaN outside its swath), starting at the tile's upper left corner and running east and south. The
//! detectors of the band whose view angles every band takes (HLS's choice, B06) are fused cell by cell (the mean of
//! the zeniths, the circular mean of the azimuths), then each angle is interpolated bilinearly to the pixel
//! centres over the valid corners of the cell (Roy et al. 2017, RSE 199:25-38, Sec. 3.2), the azimuths through
//! their sine and cosine. The pixels may be in another CRS than the tile's, a date may be several tiles.
//!
//! The XML is read with quick-xml (MIT, no unsafe code, no GDAL needed to test it) into a small tree; GDAL's own
//! parser would be walked through raw pointers. Namespaces are ignored (the elements are found by their local name).

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// The `bandId` ESA numbers the bands with in `Viewing_Incidence_Angles_Grids`, from a band's name (`B06`, `b8a`,
/// Earth Search's `rededge2`, an integer id as text); None if it is not a Sentinel-2 band.
pub fn band_id(name: &str) -> Option<i32> {
    const IDS: [&str; 13] = ["B01", "B02", "B03", "B04", "B05", "B06", "B07", "B08", "B8A", "B09", "B10", "B11", "B12"];
    const ALIASES: [(&str, &str); 13] = [
        ("coastal", "B01"),
        ("blue", "B02"),
        ("green", "B03"),
        ("red", "B04"),
        ("rededge1", "B05"),
        ("rededge2", "B06"),
        ("rededge3", "B07"),
        ("nir", "B08"),
        ("nir08", "B8A"),
        ("nir09", "B09"),
        ("cirrus", "B10"),
        ("swir16", "B11"),
        ("swir22", "B12"),
    ];
    let name = name.trim();
    if let Ok(n) = name.parse::<i32>() {
        return (0..=12).contains(&n).then_some(n);
    }
    let low = name.to_ascii_lowercase();
    let key = ALIASES.iter().find(|(a, _)| *a == low).map_or(name, |(_, b)| b).to_ascii_uppercase();
    let key = match key.as_bytes() {
        [b'B', d] if d.is_ascii_digit() => format!("B0{}", *d as char),
        _ => key,
    };
    IDS.iter().position(|b| *b == key).map(|i| i as i32)
}

// --- reading the XML -----------------------------------------------------------------------------------------------

/// An element of the XML: its local name, attributes (local names), text and children.
#[derive(Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    kids: Vec<Node>,
}

impl Node {
    fn of(e: &BytesStart) -> Node {
        let local = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        let attrs = e.attributes().flatten().map(|a| (local(a.key.local_name().as_ref()), local(&a.value))).collect();
        Node { name: local(e.local_name().as_ref()), attrs, ..Default::default() }
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The first descendant (this one included) called `name`, in document order.
    fn first(&self, name: &str) -> Option<&Node> {
        if self.name == name {
            return Some(self);
        }
        self.kids.iter().find_map(|k| k.first(name))
    }

    /// Every descendant called `name`, in document order, not looking inside the matches.
    fn all<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        for k in &self.kids {
            if k.name == name {
                out.push(k);
            } else {
                k.all(name, out);
            }
        }
    }
}

/// The XML as a tree (a holder whose only child is the root).
fn tree(xml: &str) -> Result<Node, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack = vec![Node::default()];
    loop {
        match reader.read_event().map_err(|e| format!("not XML: {e}"))? {
            Event::Start(e) => stack.push(Node::of(&e)),
            Event::Empty(e) => stack.last_mut().expect("the holder").kids.push(Node::of(&e)),
            Event::End(_) => {
                let n = stack.pop().ok_or("not XML: an end tag without its start")?;
                stack.last_mut().ok_or("not XML: an end tag without its start")?.kids.push(n);
            }
            Event::Text(t) => {
                let t = t.decode().map_err(|e| format!("not XML: {e}"))?;
                stack.last_mut().expect("the holder").text.push_str(&t);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err("not XML: an element is not closed".into());
    }
    Ok(stack.pop().expect("the holder"))
}

/// A `Zenith` or `Azimuth` element: its values (rows x columns, row by row), the shape and the column and row
/// steps in metres.
struct Grid {
    rows: usize,
    cols: usize,
    step: (f64, f64),
    values: Vec<f64>,
}

fn number(n: &Node, what: &str, name: &str) -> Result<f64, String> {
    n.text.trim().parse::<f64>().map_err(|_| format!("MTD_TL.xml {name}: {what} is not a number: {:?}", n.text))
}

fn grid(element: &Node, what: &str, name: &str) -> Result<Grid, String> {
    let find = |tag: &str| element.first(tag).ok_or_else(|| format!("MTD_TL.xml: no {tag} in {what} of {name}"));
    let step = (number(find("COL_STEP")?, "COL_STEP", name)?, number(find("ROW_STEP")?, "ROW_STEP", name)?);
    let mut lines = vec![];
    element.all("VALUES", &mut lines);
    let rows: Vec<Vec<f64>> = lines
        .iter()
        .map(|l| {
            // Python's float() takes `NaN` in any case; Rust's parse takes it as `NaN` or `nan`
            l.text
                .split_whitespace()
                .map(|v| {
                    v.parse::<f64>().map_err(|_| format!("MTD_TL.xml {name}: {v:?} in the {what} grid is not a number"))
                })
                .collect()
        })
        .collect::<Result<_, _>>()?;
    if rows.is_empty() || rows.iter().any(|r| r.len() != rows[0].len()) {
        return Err(format!("MTD_TL.xml: the {what} grid has no rows or ragged ones"));
    }
    Ok(Grid { rows: rows.len(), cols: rows[0].len(), step, values: rows.concat() })
}

/// What the XML says of one tile: its geocoding and the angle grids (the sun's, and each band's detectors').
#[derive(Debug)]
pub struct Granule {
    pub name: String,
    /// the tile's CRS as the file names it ("EPSG:32723")
    pub crs: String,
    pub ulx: f64,
    pub uly: f64,
    /// the columns' and rows' steps of the angle grids, metres
    pub step: (f64, f64),
    rows: usize,
    cols: usize,
    /// zenith and azimuth of the sun
    sun: [Vec<f64>; 2],
    /// band id -> the (zenith, azimuth) of each detector
    view: BTreeMap<i32, Vec<[Vec<f64>; 2]>>,
    /// the sensing time, seconds since 1970-01-01 UTC
    pub sensing: Option<i64>,
}

/// Seconds since 1970-01-01 UTC of an ISO 8601 time ("2023-07-23T13:26:48.719742Z", "+00:00" or none for UTC).
fn parse_time(text: &str) -> Option<i64> {
    let t = text.trim();
    let (date, rest) = t.split_once(['T', ' '])?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // the zone: Z, +hh:mm, -hh:mm
    let (clock, offset) = match rest.find(['Z', 'z', '+', '-']) {
        Some(i) => {
            let sign = match rest.as_bytes()[i] {
                b'+' => -1,
                b'-' => 1,
                _ => 0,
            };
            let zone = &rest[i + 1..];
            let mut z = zone.split(':');
            let h: i64 = z.next().filter(|s| !s.is_empty()).map_or(Some(0), |s| s.parse().ok())?;
            let mi: i64 = z.next().map_or(Some(0), |s| s.parse().ok())?;
            (&rest[..i], sign * (h * 3600 + mi * 60))
        }
        None => (rest, 0),
    };
    let mut c = clock.split(':');
    let h: i64 = c.next()?.parse().ok()?;
    let mi: i64 = c.next()?.parse().ok()?;
    let s: f64 = c.next().map_or(Some(0.0), |s| s.parse().ok())?;
    // days from civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + s.floor() as i64 + offset)
}

/// The first element called `tag` in `from`, or why the file is not a tile's metadata.
fn need<'a>(from: &'a Node, tag: &str, name: &str) -> Result<&'a Node, String> {
    from.first(tag).ok_or_else(|| format!("MTD_TL.xml: no {tag} in {name}: is it a Sentinel-2 tile metadata file?"))
}

/// Parses an `MTD_TL.xml` (`name` is what messages call it).
pub fn parse(xml: &str, name: &str) -> Result<Granule, String> {
    let root = tree(xml).map_err(|e| format!("MTD_TL.xml {name}: {e}"))?;
    let geo = need(&root, "Tile_Geocoding", name)?;
    let crs = need(geo, "HORIZONTAL_CS_CODE", name)?.text.trim().to_string();
    let mut positions = vec![];
    geo.all("Geoposition", &mut positions);
    let pos = positions
        .iter()
        .find(|p| p.attr("resolution") == Some("10"))
        .or(positions.first())
        .ok_or_else(|| format!("MTD_TL.xml {name}: no Geoposition in Tile_Geocoding"))?;
    let ulx = number(need(pos, "ULX", name)?, "ULX", name)?;
    let uly = number(need(pos, "ULY", name)?, "ULY", name)?;
    let angles = need(&root, "Tile_Angles", name)?;
    let sun_el = need(angles, "Sun_Angles_Grid", name)?;
    let zen = grid(need(sun_el, "Zenith", name)?, "sun zenith", name)?;
    let azi = grid(need(sun_el, "Azimuth", name)?, "sun azimuth", name)?;
    if (zen.rows, zen.cols) != (azi.rows, azi.cols) || zen.step != azi.step {
        return Err(format!("MTD_TL.xml {name}: the sun's zenith and azimuth grids differ"));
    }
    let mut view: BTreeMap<i32, Vec<[Vec<f64>; 2]>> = BTreeMap::new();
    for g in angles.kids.iter().filter(|k| k.name == "Viewing_Incidence_Angles_Grids") {
        let z = grid(need(g, "Zenith", name)?, "view zenith", name)?;
        let a = grid(need(g, "Azimuth", name)?, "view azimuth", name)?;
        if (z.rows, z.cols) != (zen.rows, zen.cols) || (a.rows, a.cols) != (zen.rows, zen.cols) || z.step != zen.step {
            return Err(format!(
                "MTD_TL.xml {name}: a view grid is not the sun grid's shape and step ({} x {}, {} x {} m)",
                zen.rows, zen.cols, zen.step.0, zen.step.1
            ));
        }
        let id: i32 = g
            .attr("bandId")
            .and_then(|b| b.trim().parse().ok())
            .ok_or_else(|| format!("MTD_TL.xml {name}: a Viewing_Incidence_Angles_Grids without a bandId"))?;
        view.entry(id).or_default().push([z.values, a.values]);
    }
    if view.is_empty() {
        return Err(format!("MTD_TL.xml {name}: no Viewing_Incidence_Angles_Grids"));
    }
    // the first SENSING_TIME, understood or not
    let sensing = root.first("SENSING_TIME").and_then(|n| parse_time(&n.text));
    Ok(Granule {
        name: name.to_string(),
        crs,
        ulx,
        uly,
        step: zen.step,
        rows: zen.rows,
        cols: zen.cols,
        sun: [zen.values, azi.values],
        view,
        sensing,
    })
}

// --- fusing the detectors ---------------------------------------------------------------------------------------

/// A field of the grid as it is interpolated: zenith, and the sine and cosine of the azimuth (zero where the cell
/// has no detector), and the validity of each cell (1 or 0).
#[derive(Debug)]
struct Fused {
    f: Vec<[f32; 3]>,
    v: Vec<f32>,
}

/// The `(zenith, azimuth)` grids of the detectors of a band as one field: a cell covered by several detectors is
/// the mean of their zeniths and the circular mean of their azimuths (normalized to a unit vector); a cell none
/// covers is invalid (`_fields`).
fn fuse(detectors: &[[Vec<f64>; 2]], cells: usize) -> Fused {
    let mut f = vec![[0f32; 3]; cells];
    let mut v = vec![0f32; cells];
    for i in 0..cells {
        let (mut n, mut zen, mut s, mut c) = (0usize, 0.0f64, 0.0f64, 0.0f64);
        for d in detectors {
            let (z, a) = (d[0][i], d[1][i].to_radians());
            if z.is_finite() && a.is_finite() {
                n += 1;
                zen += z;
                s += a.sin();
                c += a.cos();
            }
        }
        if n == 0 {
            continue;
        }
        let nf = n as f64;
        let (s, c) = (s / nf, c / nf);
        let norm = s.hypot(c);
        if norm > 1e-9 {
            f[i] = [(zen / nf) as f32, (s / norm) as f32, (c / norm) as f32];
            v[i] = 1.0;
        }
    }
    Fused { f, v }
}

/// A tile reduced to what one call needs: its geocoding and the fused grids of the sun and of the view band.
#[derive(Debug)]
pub struct Prepared {
    pub name: String,
    pub crs: String,
    pub ulx: f64,
    pub uly: f64,
    pub step: (f64, f64),
    pub sensing: Option<i64>,
    rows: usize,
    cols: usize,
    sun: Fused,
    view: Fused,
}

impl Granule {
    /// The tile with the view angles of the band `band` (an ESA `bandId`, see `band_id`) fused.
    pub fn prepare(&self, band: i32) -> Result<Prepared, String> {
        let detectors = self
            .view
            .get(&band)
            .ok_or_else(|| format!("MTD_TL.xml {}: no view angles for band id {band}", self.name))?;
        let cells = self.rows * self.cols;
        Ok(Prepared {
            name: self.name.clone(),
            crs: self.crs.clone(),
            ulx: self.ulx,
            uly: self.uly,
            step: self.step,
            sensing: self.sensing,
            rows: self.rows,
            cols: self.cols,
            sun: fuse(std::slice::from_ref(&self.sun), cells),
            view: fuse(detectors, cells),
        })
    }
}

/// An `MTD_TL.xml`'s text as the tile with the view angles of `band` (`parse` and `prepare`).
pub fn read(xml: &str, name: &str, band: i32) -> Result<Prepared, String> {
    parse(xml, name)?.prepare(band)
}

impl Prepared {
    /// Whether the tile was sensed within a day of the date `day` (days since 1970-01-01), as Zeit asks (the dates
    /// of the cube and the sources were mismatched otherwise); a tile without a sensing time passes.
    pub fn check_date(&self, day: i64) -> Result<(), String> {
        let Some(sensing) = self.sensing else { return Ok(()) };
        if (sensing - day * 86400).abs() > 86400 {
            let (y, m, d) = crate::numeric::civil(sensing.div_euclid(86400));
            let (y2, m2, d2) = crate::numeric::civil(day);
            return Err(format!(
                "the tile {} was sensed on {y:04}-{m:02}-{d:02}, the date is {y2:04}-{m2:02}-{d2:02}: its metadata is of \
                 another date",
                self.name
            ));
        }
        Ok(())
    }
}

// --- interpolating ----------------------------------------------------------------------------------------------

/// The pixel grid the angles are computed for: its CRS (WKT) and geotransform (north up; the pixel centres are
/// `x = gt[0] + (col + 0.5) gt[1]`, `y = gt[3] + (row + 0.5) gt[5]`, as the pixel centres of a cube are).
#[derive(Debug)]
pub struct Like {
    wkt: String,
    gt: [f64; 6],
    /// whether the tile CRSs seen are the grid's (ignoring the axis order), by the tile's CRS text
    same: Mutex<HashMap<String, bool>>,
}

fn srs_of(definition: &str) -> Result<SpatialRef, String> {
    // WKT, "EPSG:32723" or a PROJ string alike
    let mut srs = SpatialRef::from_definition(definition).map_err(|e| format!("the CRS {definition:?}: {e}"))?;
    srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    Ok(srs)
}

impl Like {
    pub fn new(crs_wkt: &str, geotransform: [f64; 6]) -> Result<Like, String> {
        if crs_wkt.trim().is_empty() {
            return Err("the grid has no CRS: the pixel centres cannot be placed on the tile".into());
        }
        if geotransform[2] != 0.0 || geotransform[4] != 0.0 {
            return Err("the grid is rotated: Sentinel-2 angles are computed for north-up grids".into());
        }
        srs_of(crs_wkt)?;
        Ok(Like { wkt: crs_wkt.to_string(), gt: geotransform, same: Mutex::default() })
    }

    /// Whether `crs` (a tile's) is the grid's CRS, ignoring the axis order of geographic ones.
    fn is_same(&self, crs: &str) -> Result<bool, String> {
        if let Some(&s) = self.same.lock().map_err(|e| e.to_string())?.get(crs) {
            return Ok(s);
        }
        let (a, b) = (srs_of(&self.wkt)?, srs_of(crs)?);
        let option = std::ffi::CString::new("CRITERION=EQUIVALENT_EXCEPT_AXIS_ORDER_GEOGCRS").expect("no NUL");
        let options = [option.as_ptr(), std::ptr::null()];
        // SAFETY: both handles live through the call, the options are a NULL-terminated list of C strings.
        let same = unsafe { gdal_sys::OSRIsSameEx(a.to_c_hsrs(), b.to_c_hsrs(), options.as_ptr()) } != 0;
        self.same.lock().map_err(|e| e.to_string())?.insert(crs.to_string(), same);
        Ok(same)
    }
}

/// The cell and the weight of its next node of a position (in nodes) along an axis of `n` nodes; None outside the
/// grid (`_axis`). `single`: the position was held as a float32 (the general path of Zeit's), whose rounding
/// decides the edge.
fn axis(pos: f64, n: usize, single: bool) -> Option<(usize, f32)> {
    let pos = if single { f64::from(pos as f32) } else { pos };
    if !(pos >= 0.0 && pos <= (n - 1) as f64) {
        return None;
    }
    let cell = (pos.floor().max(0.0) as usize).min(n - 2);
    Some((cell, (pos - cell as f64).clamp(0.0, 1.0) as f32))
}

/// The fused field interpolated at a position given by its axes, over the valid corners (zenith, sine, cosine);
/// NaN where no corner is valid.
fn corners(fz: &Fused, cols: usize, r: (usize, f32), c: (usize, f32)) -> [f32; 3] {
    let (mut num, mut den) = ([0f32; 3], 0f32);
    for dy in 0..2 {
        for dx in 0..2 {
            let w = (if dy == 1 { r.1 } else { 1.0 - r.1 }) * (if dx == 1 { c.1 } else { 1.0 - c.1 });
            let i = (r.0 + dy) * cols + c.0 + dx;
            den += w * fz.v[i];
            for (n, f) in num.iter_mut().zip(&fz.f[i]) {
                *n += w * f;
            }
        }
    }
    if den <= 0.0 { [f32::NAN; 3] } else { [num[0] / den, num[1] / den, num[2] / den] }
}

/// Zenith and azimuth (degrees, 0-360) of an interpolated (zenith, sine, cosine) (`_angles_of`).
fn zenith_azimuth(f: [f32; 3]) -> (f32, f32) {
    // numpy's `degrees` of a float32 multiplies by 180f / PIf in single (57.295776), not by the nearest float32 to the
    // factor (57.29578, what `to_degrees` takes): one unit in the last place apart
    const DEGREES: f32 = 180.0 / std::f32::consts::PI;
    let az = f[1].atan2(f[2]) * DEGREES;
    (f[0], if az < 0.0 { az + 360.0 } else { az })
}

/// The angles of a grid in the tile's CRS, from the cell and weight of each of its rows and columns (`axis`; None outside the
/// tile's grid): SZA, SAA, VZA, VAA, each rows x columns, row by row, NaN outside and where no detector covers. It is
/// Zeit's `_interp_separable` in its order of float32 operations (along x, then along y, with no division by the weights
/// when every corner of the grid is valid), which is not that of `corners` in the last bits.
fn separable(p: &Prepared, rows: &[Option<(usize, f32)>], cols: &[Option<(usize, f32)>]) -> [Vec<f32>; 4] {
    let (h, w) = (rows.len(), cols.len());
    let mut out: [Vec<f32>; 4] = std::array::from_fn(|_| vec![f32::NAN; h * w]);
    for (k, layer) in [&p.sun, &p.view].into_iter().enumerate() {
        let whole = layer.v.iter().all(|&v| v > 0.0);
        // along x: every row of the tile's grid at every output column
        let mut fx = vec![[f32::NAN; 3]; p.rows * w];
        let mut vx = vec![0f32; p.rows * w];
        for r in 0..p.rows {
            for (j, c) in cols.iter().enumerate() {
                let Some((cell, wx)) = *c else { continue };
                let (i0, i1) = (r * p.cols + cell, r * p.cols + cell + 1);
                let a = 1.0 - wx;
                for q in 0..3 {
                    fx[r * w + j][q] = layer.f[i0][q] * a + layer.f[i1][q] * wx;
                }
                vx[r * w + j] = layer.v[i0] * a + layer.v[i1] * wx;
            }
        }
        // then along y
        for (i, r) in rows.iter().enumerate() {
            let Some((cell, wy)) = *r else { continue };
            for (j, c) in cols.iter().enumerate() {
                if c.is_none() {
                    continue;
                }
                let (a, b) = (cell * w + j, (cell + 1) * w + j);
                let mut f = [0f32; 3];
                for q in 0..3 {
                    f[q] = fx[a][q] + wy * (fx[b][q] - fx[a][q]);
                }
                if !whole {
                    let den = vx[a] + wy * (vx[b] - vx[a]);
                    f = if den <= 0.0 { [f32::NAN; 3] } else { [f[0] / den, f[1] / den, f[2] / den] };
                }
                let (z, az) = zenith_azimuth(f);
                out[2 * k][i * w + j] = z;
                out[2 * k + 1][i * w + j] = az;
            }
        }
    }
    out
}

/// The angles of a window `[x0, y0, w, h]` (in pixels of `like`) from one tile: SZA, SAA, VZA, VAA, each w x h row
/// by row, NaN outside the tile's grid or where no detector covers.
fn tile_block(p: &Prepared, like: &Like, window: [usize; 4]) -> Result<[Vec<f32>; 4], String> {
    let [x0, y0, w, h] = window;
    let n = w * h;
    let mut out: [Vec<f32>; 4] = std::array::from_fn(|_| vec![f32::NAN; n]);
    let (dx, dy) = p.step;
    let xs = |col: usize| like.gt[0] + ((x0 + col) as f64 + 0.5) * like.gt[1];
    let ys = |row: usize| like.gt[3] + ((y0 + row) as f64 + 0.5) * like.gt[5];
    let mut put = |i: usize, r: (usize, f32), c: (usize, f32)| {
        for (k, layer) in [&p.sun, &p.view].into_iter().enumerate() {
            let (z, a) = zenith_azimuth(corners(layer, p.cols, r, c));
            out[2 * k][i] = z;
            out[2 * k + 1][i] = a;
        }
    };
    if like.is_same(&p.crs)? {
        let cols: Vec<_> = (0..w).map(|c| axis((xs(c) - p.ulx) / dx, p.cols, false)).collect();
        let rows: Vec<_> = (0..h).map(|r| axis((p.uly - ys(r)) / dy, p.rows, false)).collect();
        return Ok(separable(p, &rows, &cols));
    }
    // the pixel centres projected to the tile's CRS (traditional GIS axis order: x east or longitude)
    let (from, to) = (srs_of(&like.wkt)?, srs_of(&p.crs)?);
    let tr = CoordTransform::new(&from, &to).map_err(|e| format!("the CRS of the tile {}: {e}", p.name))?;
    let mut tx: Vec<f64> = (0..n).map(|i| xs(i % w)).collect();
    let mut ty: Vec<f64> = (0..n).map(|i| ys(i / w)).collect();
    let mut ok = vec![0i32; n];
    // SAFETY: the arrays are n long each and live through the call; `tr` owns the transformation.
    unsafe {
        gdal_sys::OCTTransformEx(
            tr.to_c_hct(),
            n as i32,
            tx.as_mut_ptr(),
            ty.as_mut_ptr(),
            std::ptr::null_mut(),
            ok.as_mut_ptr(),
        );
    }
    for i in 0..n {
        if ok[i] == 0 {
            continue;
        }
        let (col, row) = ((tx[i] - p.ulx) / dx, (p.uly - ty[i]) / dy);
        if let (Some(r), Some(c)) = (axis(row, p.rows, true), axis(col, p.cols, true)) {
            put(i, r, c);
        }
    }
    Ok(out)
}

/// The angles of a date (SZA, SAA, VZA, VAA, degrees) over the window `[x0, y0, w, h]` of `like`, row by row, NaN
/// where the date's tiles have none; with several tiles the first fills the grid and the next ones the pixels
/// where the first has no view zenith (all four angles taken from the tile that has it, as Zeit does).
pub fn angles(tiles: &[&Prepared], like: &Like, window: [usize; 4]) -> Result<[Vec<f32>; 4], String> {
    let n = window[2] * window[3];
    let Some((first, more)) = tiles.split_first() else { return Ok(std::array::from_fn(|_| vec![f32::NAN; n])) };
    let mut out = tile_block(first, like, window)?;
    for p in more {
        if !out[2].iter().any(|v| v.is_nan()) {
            break;
        }
        let next = tile_block(p, like, window)?;
        for i in 0..n {
            if out[2][i].is_nan() {
                for k in 0..4 {
                    out[k][i] = next[k][i];
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_names() {
        for (name, id) in [
            ("B06", 5),
            ("b06", 5),
            ("B6", 5),
            ("B8A", 8),
            ("b8a", 8),
            ("rededge2", 5),
            ("nir", 7),
            ("nir08", 8),
            ("swir22", 12),
            ("B12", 12),
            ("0", 0),
            ("12", 12),
        ] {
            assert_eq!(band_id(name), Some(id), "{name}");
        }
        for name in ["B13", "13", "-1", "each", "", "B00x", "thermal"] {
            assert_eq!(band_id(name), None, "{name}");
        }
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-02T00:00:00Z"), Some(86400));
        assert_eq!(parse_time("2023-07-23T13:26:48.719742Z"), Some(1690118808));
        assert_eq!(parse_time("2023-07-23T15:26:48+02:00"), Some(1690118808));
        assert_eq!(parse_time("2023-07-23T13:26:48"), Some(1690118808));
        assert_eq!(parse_time("garbage"), None);
    }
}
