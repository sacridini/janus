//! Web basemap (src/basemap.cpp): satellite imagery or a map drawn under the series, XYZ tiles in Web Mercator
//! (EPSG:3857) read through GDAL's WMS driver (TMS service: libcurl and a disk cache of its own, no new dependency)
//! in a pool of its own, kept on the GPU (LRU) and drawn on the map, the active layer's pixel grid, through a warp
//! grid from that grid to Web Mercator (reproject.rs). Only the zoom level that matches the screen and the tiles in
//! view are read; requests that left the screen are dropped before they are sent. Nothing here exists, or touches
//! the network, until a source is picked.

use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::{Dataset, DatasetOptions, GdalOpenFlags};
use janus_cache::JobPool;
use janus_cube::CubeInfo;

use crate::reproject::{Quiet, Reprojection, srs};

/// half the width of the Web Mercator world (m)
const E: f64 = 20037508.342789244;
/// grid around the active layer, × its longer side
const MARGIN: f64 = 1.5;
/// per view and level (a 4K screen needs ~160)
const MAX_TILES: f64 = 300.0;
/// a tile that failed (no connection) is asked for again after
const RETRY_MS: f64 = 15000.0;
/// ... or after a server error (a wrong URL, key)
const RETRY_HTTP_MS: f64 = 300000.0;
static OPENED: AtomicI32 = AtomicI32::new(0);

fn now_ms() -> f64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

fn tile_key(z: i32, x: i32, y: i32) -> u64 {
    ((z as u64) << 44) | ((x as u64) << 22) | y as u64
}

#[derive(Clone, Debug, PartialEq)]
pub struct BasemapSource {
    /// "esri", "eox", "osm" or "custom" (kept in the settings)
    pub id: String,
    /// as listed
    pub name: String,
    /// XYZ template: {z}, {x}, {y} ({-y}: rows from the bottom, {s}: "a")
    pub url: String,
    pub max_zoom: i32,
    /// pixels per side (256 or 512)
    pub tile_size: i32,
    /// drawn on the map and in the exports whenever the basemap is on
    pub attribution: String,
    /// licence / terms of use, for the interface
    pub terms: String,
    /// concurrent requests
    pub connections: usize,
    /// how long a tile stays in the disk cache
    pub cache_days: i32,
}

impl BasemapSource {
    /// The same tiles (a change of any of these makes a new Basemap).
    pub fn same_tiles(&self, o: &BasemapSource) -> bool {
        self.url == o.url
            && self.max_zoom == o.max_zoom
            && self.tile_size == o.tile_size
            && self.attribution == o.attribution
    }
}

/// Esri World Imagery, Sentinel-2 cloudless 2016 by EOX, OpenStreetMap.
pub fn presets() -> Vec<BasemapSource> {
    let p = |id: &str, name: &str, url: &str, max_zoom, attribution: &str, terms: &str, connections, cache_days| {
        BasemapSource {
            id: id.into(),
            name: name.into(),
            url: url.into(),
            max_zoom,
            tile_size: 256,
            attribution: attribution.into(),
            terms: terms.into(),
            connections,
            cache_days,
        }
    };
    vec![
        p(
            "esri",
            "Esri World Imagery",
            "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
            18,
            "Powered by Esri | Source: Esri, Vantor, Earthstar Geographics, and the GIS User Community",
            "Esri's terms of use apply (no key; the attribution must be shown)",
            4,
            30,
        ),
        p(
            "eox",
            "Sentinel-2 cloudless 2016 (EOX)",
            "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless_3857/default/g/{z}/{y}/{x}.jpg",
            15,
            "Sentinel-2 cloudless 2016 - https://cloudless.eox.at by EOX IT Services GmbH \
             (Contains modified Copernicus Sentinel data 2016)",
            "CC BY 4.0 (the 2016 mosaic; EOX's later years are non-commercial only)",
            4,
            90,
        ),
        p(
            "osm",
            "OpenStreetMap",
            "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
            19,
            "\u{a9} OpenStreetMap contributors",
            "ODbL; OSM's tile usage policy: light use only (2 connections, tiles cached)",
            2,
            7,
        ),
    ]
}

fn is_file_url(url: &str) -> bool {
    url.starts_with("file://")
}

/// Why a custom XYZ template cannot be used (None if it can).
pub fn url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() {
        return Some("type the tile URL");
    }
    if !url.starts_with("https://") && !url.starts_with("http://") && !is_file_url(url) {
        return Some("the URL must start with https://, http:// or file://");
    }
    if !url.contains("{z}") || !url.contains("{x}") || (!url.contains("{y}") && !url.contains("{-y}")) {
        return Some("the URL needs {z}, {x} and {y}");
    }
    None
}

/// TLS certificates for libcurl (basemap tiles, remote rasters): the user's setting if any, else the system's
/// bundle (Linux, macOS: the conda-built curl looks for one at its build prefix), else the one shipped in share/ssl.
/// Empty: curl's default (Windows: Schannel and the system store).
pub fn curl_ca_bundle() -> String {
    for k in ["CURL_CA_BUNDLE", "GDAL_CURL_CA_BUNDLE", "SSL_CERT_FILE"] {
        if gdal::config::get_config_option(k, "").is_ok_and(|v| !v.is_empty()) {
            return String::new();
        }
    }
    if !cfg!(windows) {
        // Debian/Ubuntu/Arch, Fedora/RHEL, openSUSE, older RHEL, macOS and Alpine
        for f in [
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/etc/pki/tls/cacert.pem",
            "/etc/ssl/cert.pem",
        ] {
            if Path::new(f).exists() {
                return f.into();
            }
        }
    }
    let shipped =
        std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("share").join("ssl").join("cacert.pem")));
    match shipped {
        Some(p) if p.exists() => p.to_string_lossy().into_owned(),
        _ => String::new(),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// GDAL WMS description of an XYZ source: the TMS service over the whole Web Mercator world, one overview per zoom
/// level, RGBA (JPEG tiles get an opaque alpha; tiles the server does not have, 204/404, are transparent).
fn wms_xml(s: &BasemapSource, cache_dir: &str) -> String {
    let from_bottom = s.url.contains("{-y}"); // TMS rows
    let url = s
        .url
        .replace("{-y}", "${y}")
        .replace("{z}", "${z}")
        .replace("{x}", "${x}")
        .replace("{y}", "${y}")
        .replace("{s}", "a");
    let ts = s.tile_size;
    let mut x = format!(
        "<GDAL_WMS><Service name=\"TMS\"><ServerUrl>{}</ServerUrl></Service><DataWindow>\
         <UpperLeftX>-20037508.342789244</UpperLeftX><UpperLeftY>20037508.342789244</UpperLeftY>\
         <LowerRightX>20037508.342789244</LowerRightX><LowerRightY>-20037508.342789244</LowerRightY>\
         <TileLevel>{}</TileLevel><TileCountX>1</TileCountX>\
         <TileCountY>1</TileCountY><YOrigin>{}</YOrigin></DataWindow>\
         <Projection>EPSG:3857</Projection><BlockSizeX>{ts}</BlockSizeX><BlockSizeY>{ts}</BlockSizeY>\
         <BandsCount>4</BandsCount>\
         <UserAgent>Janus/{} (+https://github.com/sacridini/janus)</UserAgent>\
         <Timeout>20</Timeout><ZeroBlockHttpCodes>204,404</ZeroBlockHttpCodes>",
        xml_escape(&url),
        s.max_zoom,
        if from_bottom { "bottom" } else { "top" },
        env!("CARGO_PKG_VERSION"),
    );
    if !is_file_url(&s.url) {
        // local tiles need no copy
        x += &format!(
            "<Cache><Path>{}</Path><Expires>{}</Expires><MaxSize>536870912</MaxSize></Cache>",
            xml_escape(cache_dir),
            s.cache_days * 86400
        );
    }
    x + "</GDAL_WMS>"
}

struct TileRead {
    key: u64,
    rgba: Vec<u8>,
    ms: f64,
}

#[derive(Default)]
struct State {
    /// key -> last frame it was in view
    wanted: HashMap<u64, u64>,
    inflight: HashSet<u64>,
    /// key -> when it may be asked for again (ms)
    failed: HashMap<u64, f64>,
    results: Vec<TileRead>,
    /// the last server error
    error: String,
}

/// What the reader threads share with the Basemap.
struct Shared {
    xml: String,
    ca_bundle: String,
    max_zoom: i32,
    tile_size: i32,
    wake: Arc<dyn Fn() + Send + Sync>,
    m: Mutex<State>,
    frame: AtomicU64,
    offline: AtomicBool,
    /// a dataset per concurrent request: GDAL datasets are not thread-safe
    idle: Mutex<Vec<Dataset>>,
}

impl Shared {
    fn acquire(&self) -> Option<Dataset> {
        if let Some(d) = self.idle.lock().unwrap().pop() {
            return Some(d);
        }
        let opts = DatasetOptions {
            open_flags: GdalOpenFlags::GDAL_OF_RASTER | GdalOpenFlags::GDAL_OF_READONLY,
            allowed_drivers: Some(&["WMS"]),
            ..Default::default()
        };
        let d = Dataset::open_ex(&self.xml, opts).ok()?;
        OPENED.fetch_add(1, Ordering::Relaxed);
        Some(d)
    }

    fn release(&self, d: Dataset) {
        self.idle.lock().unwrap().push(d);
    }
}

/// Tile (x, y) of zoom z as RGBA rows: one block of the overview of that level (GDAL fetches all four bands of it
/// in one request). Err((http status of a failed request: 0 the server was not reached, -1 other), message).
fn read_tile(
    ds: &Dataset,
    max_zoom: i32,
    ts: i32,
    z: i32,
    x: i32,
    y: i32,
    rgba: &mut [u8],
) -> Result<(), (i32, String)> {
    use gdal_sys::*;
    let h = ds.c_dataset();
    let mut bands = [std::ptr::null_mut(); 4];
    for (b, band) in bands.iter_mut().enumerate() {
        let mut bh = unsafe { GDALGetRasterBand(h, b as i32 + 1) };
        if !bh.is_null() && z < max_zoom {
            bh = unsafe { GDALGetOverview(bh, max_zoom - z - 1) };
        }
        if bh.is_null() || unsafe { GDALGetRasterBandXSize(bh) } != (ts << z) {
            return Err((-1, "unexpected tile pyramid".into()));
        }
        *band = bh;
    }
    unsafe { CPLErrorReset() };
    let mut ok = true;
    for (b, &band) in bands.iter().enumerate() {
        if !ok {
            break;
        }
        ok = unsafe {
            GDALRasterIO(
                band,
                GDALRWFlag::GF_Read,
                x * ts,
                y * ts,
                ts,
                ts,
                rgba.as_mut_ptr().add(b).cast(),
                ts,
                ts,
                GDALDataType::GDT_Byte,
                4,
                ts * 4,
            )
        } == CPLErr::CE_None;
    }
    for &band in &bands {
        unsafe { GDALFlushRasterCache(band) }; // on the GPU now, not in GDAL's block cache
    }
    if ok {
        return Ok(());
    }
    let msg = unsafe { CStr::from_ptr(CPLGetLastErrorMsg()) }.to_string_lossy().into_owned();
    let mut http = msg.find("HTTP status code: ").map_or(-1, |p| {
        let rest = &msg[p + 18..];
        rest[..rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len())].parse().unwrap_or(0)
    });
    if http == 0 && (msg.contains("certificate") || msg.contains("SSL")) {
        http = -1; // reached, but TLS failed: not "offline"
    }
    let error = if http > 0 {
        format!("HTTP {http}")
    } else if http == 0 {
        "no connection".into()
    } else if msg.contains("certificate") {
        "TLS certificates (see CURL_CA_BUNDLE)".into()
    } else {
        "the tile could not be read".into()
    };
    Err((http, error))
}

/// Where a tile is drawn: `quad`, the map-space box of its part inside the frame; `warp_quad` that box in the warp
/// grid's uv and `warp_src` the tile in the frame's uv (janus_gpu::Warp).
#[derive(Clone, Copy, Debug, Default)]
pub struct Placement {
    pub quad: [f64; 4],
    pub warp_quad: [f32; 4],
    pub warp_src: [f32; 4],
}

struct Tile<T> {
    tex: T,
    last_used: u64,
    /// map_version the placement was computed for
    placed_for: u64,
    placement: Option<Placement>,
}

pub struct Basemap<T> {
    src: BasemapSource,
    sh: Arc<Shared>,
    /// map space -> the Web Mercator frame below, and its grid on the GPU
    map: Option<(Reprojection, T)>,
    /// CubeInfo id of the active layer it was made for
    map_for: u64,
    map_version: u64,
    /// the frame's geotransform: left x, top y, pixel size (m)
    frame: [f64; 3],
    frame_w: usize,
    frame_h: usize,
    /// when the map was placed (ms)
    placed_at: f64,
    first_tile_ms: Option<f64>,
    frame_no: u64,
    gpu: HashMap<u64, Tile<T>>,
    gpu_bytes: usize,
    max_gpu_bytes: usize,
    load_ms_sum: f64,
    loads: u32,
    /// last: dropped first (its jobs use the shared state)
    pool: JobPool,
}

impl<T> Drop for Basemap<T> {
    fn drop(&mut self) {
        self.sh.m.lock().unwrap().wanted.clear(); // queued requests drop themselves
    }
}

impl<T> Basemap<T> {
    /// `cache_dir`: GDAL's tile cache (a folder per source inside it); `wake` is called from the reader threads
    /// when a tile is ready.
    pub fn new(src: &BasemapSource, cache_dir: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let mut src = src.clone();
        src.max_zoom = src.max_zoom.clamp(0, if src.tile_size > 256 { 21 } else { 22 }); // pixel sizes stay in an int
        let _ = std::fs::create_dir_all(cache_dir);
        let sh = Arc::new(Shared {
            xml: wms_xml(&src, &cache_dir.to_string_lossy()),
            ca_bundle: curl_ca_bundle(),
            max_zoom: src.max_zoom,
            tile_size: src.tile_size,
            wake,
            m: Mutex::new(State::default()),
            frame: AtomicU64::new(0),
            offline: AtomicBool::new(false),
            idle: Mutex::new(Vec::new()),
        });
        let pool = JobPool::new(src.connections.max(1), "basemap", None);
        Basemap {
            src,
            sh,
            map: None,
            map_for: 0,
            map_version: 0,
            frame: [0.0, 0.0, 1.0],
            frame_w: 1,
            frame_h: 1,
            placed_at: 0.0,
            first_tile_ms: None,
            frame_no: 0,
            gpu: HashMap::new(),
            gpu_bytes: 0,
            max_gpu_bytes: 128 << 20,
            load_ms_sum: 0.0,
            loads: 0,
            pool,
        }
    }

    pub fn source(&self) -> &BasemapSource {
        &self.src
    }

    /// GDAL WMS datasets opened by this process (tests: none while off).
    pub fn datasets_opened() -> i32 {
        OPENED.load(Ordering::Relaxed)
    }

    /// The last request did not reach the server.
    pub fn offline(&self) -> bool {
        self.sh.offline.load(Ordering::Relaxed)
    }

    /// The last server error (e.g. "HTTP 403"), "" if none.
    pub fn error(&self) -> String {
        self.sh.m.lock().unwrap().error.clone()
    }

    pub fn inflight(&self) -> usize {
        self.sh.m.lock().unwrap().inflight.len()
    }

    pub fn gpu_tiles(&self) -> usize {
        self.gpu.len()
    }

    pub fn gpu_bytes(&self) -> usize {
        self.gpu_bytes
    }

    pub fn loads(&self) -> u32 {
        self.loads
    }

    pub fn avg_load_ms(&self) -> f64 {
        if self.loads > 0 { self.load_ms_sum / f64::from(self.loads) } else { 0.0 }
    }

    /// Since the map was placed (None: no tile yet).
    pub fn first_tile_ms(&self) -> Option<f64> {
        self.first_tile_ms
    }

    pub fn has_map(&self) -> bool {
        self.map.is_some()
    }

    pub fn map(&self) -> Option<&Reprojection> {
        self.map.as_ref().map(|m| &m.0)
    }

    /// The warp grid on the GPU.
    pub fn grid(&self) -> Option<&T> {
        self.map.as_ref().map(|m| &m.1)
    }

    /// The GPU's resources (tiles, the warp grid) dropped; the map is placed again when asked.
    pub fn release_gpu(&mut self) {
        self.gpu.clear();
        self.gpu_bytes = 0;
        self.map = None;
        self.map_for = 0;
    }

    /// Places Web Mercator on the map: the warp grid from the active layer's pixels to the tiles (kept while the
    /// layer stays active). Err(why) if the layer has no georeferencing or CRS. `make_grid` puts the grid on the
    /// GPU (w, h, the uv pairs).
    ///
    /// The tiles' frame: a Web Mercator rectangle around everything the map can show (the layer and MARGIN × its
    /// longer side around it), as a raster about as fine as the layer; Reprojection makes the grid from the
    /// layer's pixels to it, as for a layer in another CRS. Its uv stay small numbers (float in the shader) even
    /// for the finest tiles.
    pub fn set_map(&mut self, a: &CubeInfo, make_grid: impl FnOnce(usize, usize, &[f32]) -> T) -> Result<(), String> {
        if self.map.is_some() && self.map_for == a.id {
            return Ok(());
        }
        self.map = None;
        self.map_for = 0;
        self.map_version += 1;
        let Some(g) = a.geo_transform else {
            return Err("the active layer has no georeferencing".into());
        };
        if a.crs_wkt.is_empty() {
            return Err("the active layer has no CRS".into());
        }
        let (Some(sa), Ok(mut sm)) = (srs(&a.crs_wkt), SpatialRef::from_epsg(3857)) else {
            return Err("the active layer's CRS is not understood".into());
        };
        sm.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let ct = {
            let _q = Quiet::new();
            CoordTransform::new(&sa, &sm).map_err(|_| "no transformation to Web Mercator".to_string())?
        };
        let m = MARGIN * a.width.max(a.height) as f64;
        let around = [-m, -m, a.width as f64 + m, a.height as f64 + m];
        const N: usize = 33;
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for j in 0..N {
            for i in 0..N {
                let px = around[0] + (around[2] - around[0]) * i as f64 / (N - 1) as f64;
                let py = around[1] + (around[3] - around[1]) * j as f64 / (N - 1) as f64;
                xs.push(g[0] + px * g[1] + py * g[2]);
                ys.push(g[3] + px * g[4] + py * g[5]);
            }
        }
        let mut ok = vec![0i32; xs.len()];
        {
            let _q = Quiet::new();
            unsafe {
                gdal_sys::OCTTransformEx(
                    ct.to_c_hct(),
                    xs.len() as i32,
                    xs.as_mut_ptr(),
                    ys.as_mut_ptr(),
                    std::ptr::null_mut(),
                    ok.as_mut_ptr(),
                );
            }
        }
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for k in 0..xs.len() {
            if ok[k] == 0 || !xs[k].is_finite() || !ys[k].is_finite() {
                continue;
            }
            let (x, y) = (xs[k].clamp(-E, E), ys[k].clamp(-E, E));
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
        if !(b[2] > b[0] && b[3] > b[1]) {
            return Err("the layer is outside Web Mercator".into());
        }
        // About one frame pixel per map pixel: the grid's error (< 0.05 of the frame's pixels) stays far below the
        // screen's pixels.
        let res = ((b[2] - b[0]) * (b[3] - b[1]) / ((around[2] - around[0]) * (around[3] - around[1]))).sqrt();
        self.frame_w = 1.max(((b[2] - b[0]) / res).ceil() as usize);
        self.frame_h = 1.max(((b[3] - b[1]) / res).ceil() as usize);
        let wkt = sm.to_wkt().unwrap_or_default();
        let f = CubeInfo {
            width: self.frame_w,
            height: self.frame_h,
            geo_transform: Some([b[0], res, 0.0, b[3], 0.0, -res]),
            crs_wkt: wkt,
            crs_authority: "EPSG:3857".into(),
            ..Default::default()
        };
        self.frame = [b[0], b[3], res];
        let r = Reprojection::new(a, &f, MARGIN)?;
        if !r.has_domain() {
            return Err("the layer is outside Web Mercator".into());
        }
        let tex = make_grid(r.grid_w, r.grid_h, &r.grid);
        self.map = Some((r, tex));
        self.map_for = a.id;
        self.placed_at = now_ms();
        self.first_tile_ms = None;
        Ok(())
    }

    /// Once per frame (whatever the number of map panels calling update()): requests not renewed for 2 frames are
    /// dropped before being sent.
    pub fn tick(&mut self) {
        self.frame_no += 1;
        self.sh.frame.store(self.frame_no, Ordering::Relaxed);
        if self.frame_no.is_multiple_of(120) {
            // prune old requests and failures
            let now = now_ms();
            let f = self.frame_no;
            let mut st = self.sh.m.lock().unwrap();
            st.wanted.retain(|_, &mut w| w + 120 >= f);
            st.failed.retain(|_, &mut t| now <= t);
        }
    }

    /// Zoom level and Web Mercator box (x0, y0, x1, y1; y up) of a view.
    fn view_tiles(&self, v: &[f64; 4], px_per_map_px: f64) -> Option<(i32, [f64; 4])> {
        let (r, _) = self.map.as_ref()?;
        if px_per_map_px.is_nan() || px_per_map_px <= 0.0 {
            return None;
        }
        let (x0, y0) = (v[0].max(r.domain[0]), v[1].max(r.domain[1]));
        let (x1, y1) = (v[2].min(r.domain[2]), v[3].min(r.domain[3]));
        if !(x1 > x0 && y1 > y0) {
            return None;
        }
        let p = r.polygon(x0, y0, x1, y1, 8);
        if p.len() < 6 {
            return None;
        }
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for q in p.as_chunks::<2>().0 {
            let (x, y) = (self.frame[0] + q[0] * self.frame[2], self.frame[1] - q[1] * self.frame[2]);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
        let b = b.map(|v| v.clamp(-E, E));
        // metres of Web Mercator per target pixel at the view's centre; the level whose pixels are about as fine
        // (up to 1.23× enlarged)
        let k = r.layer_px_per_map_px((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        if k.is_nan() || k <= 0.0 {
            return None;
        }
        let metres = k * self.frame[2] / px_per_map_px;
        let mut z = (((2.0 * E / (f64::from(self.src.tile_size) * metres)).log2() - 0.3).ceil() as i32)
            .clamp(0, self.src.max_zoom);
        while z > 0 {
            let r = tile_range(z, &b);
            if f64::from(r[2] - r[0] + 1) * f64::from(r[3] - r[1] + 1) <= MAX_TILES {
                break;
            }
            z -= 1;
        }
        Some((z, b))
    }

    /// A map panel's view: `view` = map-space rectangle (x0, y0, x1, y1, in the active layer's pixels),
    /// `px_per_map_px` = target pixels per map pixel. Requests the tiles it lacks at the matching zoom level (and a
    /// few of a coarser one first, a quick preview); returns that level.
    pub fn update(&mut self, view: &[f64; 4], px_per_map_px: f64) -> Option<i32> {
        let (z, b) = self.view_tiles(view, px_per_map_px)?;
        if z >= 3 {
            self.request(z - 3, &b, 0); // a coarse preview first: 1/64 of the tiles
        }
        self.request(z, &b, 1);
        Some(z)
    }

    fn request(&mut self, z: i32, b: &[f64; 4], priority: i32) {
        let r = tile_range(z, b);
        let (cx, cy, now) = (f64::from(r[0] + r[2]) * 0.5, f64::from(r[1] + r[3]) * 0.5, now_ms());
        let mut todo = Vec::new();
        let mut st = self.sh.m.lock().unwrap();
        for y in r[1]..=r[3] {
            for x in r[0]..=r[2] {
                let key = tile_key(z, x, y);
                if let Some(t) = self.gpu.get_mut(&key) {
                    t.last_used = self.frame_no;
                    continue;
                }
                st.wanted.insert(key, self.frame_no);
                if st.failed.get(&key).is_some_and(|&t| now < t) || st.inflight.contains(&key) {
                    continue;
                }
                todo.push(((f64::from(x) - cx).hypot(f64::from(y) - cy), key, x, y));
            }
        }
        // the pool runs the newest first: the tiles nearest the centre go last
        todo.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, key, x, y) in todo {
            st.inflight.insert(key);
            let sh = self.sh.clone();
            self.pool.submit(priority, move || {
                {
                    let mut st = sh.m.lock().unwrap();
                    if st.wanted.get(&key).is_none_or(|&w| w + 2 < sh.frame.load(Ordering::Relaxed)) {
                        st.inflight.remove(&key); // left the screen before being sent
                        return;
                    }
                }
                // per thread: a dead connection fails fast; certificates for curl
                let _ = gdal::config::set_thread_local_config_option("GDAL_HTTP_CONNECTTIMEOUT", "5");
                if !sh.ca_bundle.is_empty() {
                    let _ = gdal::config::set_thread_local_config_option("CURL_CA_BUNDLE", &sh.ca_bundle);
                }
                let t0 = now_ms();
                let ts = sh.tile_size;
                let mut rgba = vec![0u8; (ts * ts * 4) as usize];
                let res = match sh.acquire() {
                    Some(ds) => {
                        let r = read_tile(&ds, sh.max_zoom, ts, z, x, y, &mut rgba);
                        sh.release(ds);
                        r
                    }
                    None => Err((-1, "the tile source could not be opened".into())),
                };
                let ms = now_ms() - t0;
                {
                    let mut st = sh.m.lock().unwrap();
                    match res {
                        Ok(()) => {
                            st.results.push(TileRead { key, rgba, ms });
                            sh.offline.store(false, Ordering::Relaxed);
                            st.error.clear();
                        }
                        Err((http, error)) => {
                            st.inflight.remove(&key);
                            st.failed.insert(key, now_ms() + if http == 0 { RETRY_MS } else { RETRY_HTTP_MS });
                            if http == 0 {
                                sh.offline.store(true, Ordering::Relaxed);
                            } else {
                                st.error = error;
                            }
                        }
                    }
                }
                (sh.wake)();
            });
        }
    }

    /// Puts the tiles that were read on the GPU (up to `max`); `make` makes a texture of RGBA bytes (side × side).
    /// Returns whether any came.
    pub fn upload_ready(&mut self, max: usize, mut make: impl FnMut(&[u8], u32) -> T) -> bool {
        let batch: Vec<TileRead> = {
            let mut st = self.sh.m.lock().unwrap();
            let n = max.min(st.results.len());
            let at = st.results.len() - n;
            let batch: Vec<TileRead> = st.results.drain(at..).rev().collect();
            for r in &batch {
                st.inflight.remove(&r.key);
            }
            batch
        };
        let ts = self.src.tile_size as usize;
        for r in &batch {
            if self.gpu.contains_key(&r.key) {
                continue;
            }
            let tex = make(&r.rgba, ts as u32);
            self.gpu.insert(r.key, Tile { tex, last_used: self.frame_no, placed_for: 0, placement: None });
            self.gpu_bytes += ts * ts * 4;
            self.load_ms_sum += r.ms;
            self.loads += 1;
            if self.first_tile_ms.is_none() {
                self.first_tile_ms = Some(now_ms() - self.placed_at);
            }
        }
        if self.gpu_bytes > self.max_gpu_bytes {
            self.evict();
        }
        !batch.is_empty()
    }

    fn evict(&mut self) {
        let mut order: Vec<(u64, u64)> = self.gpu.iter().map(|(&k, t)| (t.last_used, k)).collect();
        order.sort_unstable();
        let one = (self.src.tile_size * self.src.tile_size * 4) as usize;
        for (used, key) in order {
            if self.gpu_bytes <= self.max_gpu_bytes * 9 / 10 || used >= self.frame_no {
                break;
            }
            self.gpu.remove(&key);
            self.gpu_bytes -= one;
        }
    }

    /// Where tile (z, x, y) is drawn: the map-space box of its part inside the frame (outside it the transformation
    /// may fail), with a margin for the curvature of its edges; the shader discards what falls outside the tile.
    fn place(&self, z: i32, x: i32, y: i32) -> Option<Placement> {
        let (r, _) = self.map.as_ref()?;
        let s = 2.0 * E / f64::from(1u32 << z);
        let res = self.frame[2];
        let (gx0, gy0) = (-E + f64::from(x) * s, E - f64::from(y) * s); // top left
        let (lx0, ly0, side) = ((gx0 - self.frame[0]) / res, (self.frame[1] - gy0) / res, s / res);
        let (cx0, cy0) = (lx0.max(0.0), ly0.max(0.0));
        let (cx1, cy1) = ((lx0 + side).min(self.frame_w as f64), (ly0 + side).min(self.frame_h as f64));
        if !(cx1 > cx0 && cy1 > cy0) {
            return None;
        }
        let b = r.footprint(cx0, cy0, cx1, cy1)?;
        let (mx, my) = ((b[2] - b[0]) * 0.02 + 1.0, (b[3] - b[1]) * 0.02 + 1.0);
        let d = r.domain;
        let q = [(b[0] - mx).max(d[0]), (b[1] - my).max(d[1]), (b[2] + mx).min(d[2]), (b[3] + my).min(d[3])];
        if q[2] <= q[0] || q[3] <= q[1] {
            return None;
        }
        let (dw, dh) = (d[2] - d[0], d[3] - d[1]);
        let (fw, fh) = (self.frame_w as f64, self.frame_h as f64);
        Some(Placement {
            quad: q,
            warp_quad: [
                ((q[0] - d[0]) / dw) as f32,
                ((q[1] - d[1]) / dh) as f32,
                ((q[2] - q[0]) / dw) as f32,
                ((q[3] - q[1]) / dh) as f32,
            ],
            warp_src: [(lx0 / fw) as f32, (ly0 / fh) as f32, (side / fw) as f32, (side / fh) as f32],
        })
    }

    /// The cached tiles covering the view, coarser levels first (they fill what the wanted level still lacks):
    /// f(tile, the warp grid, where).
    pub fn for_each_visible(&mut self, view: &[f64; 4], px_per_map_px: f64, mut f: impl FnMut(&T, &T, &Placement)) {
        let Some((z, b)) = self.view_tiles(view, px_per_map_px) else { return };
        // coarse to fine; the next finer level (zooming out) before the wanted one
        let mut levels: Vec<i32> = ((z - 8).max(0)..z).collect();
        if z < self.src.max_zoom {
            levels.push(z + 1);
        }
        levels.push(z);
        for l in levels {
            let r = tile_range(l, &b);
            if f64::from(r[2] - r[0] + 1) * f64::from(r[3] - r[1] + 1) > 4.0 * MAX_TILES {
                continue;
            }
            for y in r[1]..=r[3] {
                for x in r[0]..=r[2] {
                    let key = tile_key(l, x, y);
                    if !self.gpu.contains_key(&key) {
                        continue;
                    }
                    let stale = self.gpu[&key].placed_for != self.map_version;
                    let placement = if stale { Some(self.place(l, x, y)) } else { None };
                    let t = self.gpu.get_mut(&key).unwrap();
                    t.last_used = self.frame_no;
                    if let Some(p) = placement {
                        t.placement = p;
                        t.placed_for = self.map_version;
                    }
                    if let (Some(p), Some((_, grid))) = (&t.placement, &self.map) {
                        f(&t.tex, grid, p);
                    }
                }
            }
        }
    }
}

fn tile_range(z: i32, b: &[f64; 4]) -> [i32; 4] {
    let n = 1i32 << z;
    let s = 2.0 * E / f64::from(n);
    [
        (((b[0] + E) / s).floor() as i32).clamp(0, n - 1),
        (((E - b[3]) / s).floor() as i32).clamp(0, n - 1),
        (((b[2] + E) / s - 1e-9).floor() as i32).clamp(0, n - 1),
        (((E - b[1]) / s - 1e-9).floor() as i32).clamp(0, n - 1),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(url_problem(""), Some("type the tile URL"));
        assert!(url_problem("ftp://x/{z}/{x}/{y}").is_some());
        assert!(url_problem("https://x/{z}/{x}.png").is_some());
        assert_eq!(url_problem("https://x/{z}/{x}/{-y}.png"), None);
        let xml = wms_xml(&presets()[2], "C:/cache");
        assert!(xml.contains("<ServerUrl>https://tile.openstreetmap.org/${z}/${x}/${y}.png</ServerUrl>"));
        assert!(xml.contains("<YOrigin>top</YOrigin>") && xml.contains("<TileLevel>19</TileLevel>"));
    }

    #[test]
    fn ranges() {
        // the whole world at zoom 0 and 2
        let w = [-E, -E, E, E];
        assert_eq!(tile_range(0, &w), [0, 0, 0, 0]);
        assert_eq!(tile_range(2, &w), [0, 0, 3, 3]);
        // the north-east quarter at zoom 1: x 1, row 0
        assert_eq!(tile_range(1, &[1.0, 1.0, E, E]), [1, 0, 1, 0]);
        assert_eq!(tile_key(3, 5, 7), (3 << 44) | (5 << 22) | 7);
    }
}
