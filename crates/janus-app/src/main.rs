//! Janus in Rust: raster time series and data cubes, explored and analysed (RUST_MIGRATION.md). Phase 1, the
//! skeleton: a series opened with GDAL, its overview (shared with the C++'s cache), the map and its statistics on
//! the GPU, the dock of panels, the time series chart; the startup measured as the C++ measures it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;
mod animation;
mod app;
mod areas_ui;
mod atlas;
mod atlas_math;
mod attributes;
mod bands_time;
mod basemap;
mod calc_live;
mod calculator;
mod canvas;
mod canvas_layer;
mod chart;
mod chips;
mod classes;
mod composite;
mod contents;
mod cycles;
mod display;
mod emb_download;
mod embeddings;
mod embeddings_test;
mod export_map;
mod exports;
mod fields_ui;
mod files;
mod gif;
mod golden;
mod groups;
mod histogram;
mod inbox;
mod labels;
mod layers;
mod map;
mod mcp;
mod measure;
mod measure_vectors;
mod methods;
mod ml;
mod mosaic;
mod navigation;
mod open;
mod order;
mod palette;
mod panels;
mod phase;
mod platform;
mod polygonize_ui;
mod project;
mod provenance;
mod python;
mod rasterize_ui;
mod regularize_ui;
mod roi;
mod sample_check;
mod sankey;
mod season;
mod selftest_zeit;
mod series;
mod series_ui;
mod session;
mod settings;
mod shots;
mod shots_ml;
mod shots_validation;
mod similarity;
mod speckle_ui;
mod stac;
mod status;
mod style;
mod theme;
mod tour;
mod trajectory;
mod transect;
mod trend_test;
mod ui_test;
mod undo;
mod update;
mod validation;
mod validation_tabs;
mod values;
mod vector_gpu;
mod vectors;
mod views;
mod warp_ui;
mod wavelet;
mod zeit;
mod zonal_ui;

use std::sync::Arc;
use std::time::Instant;

use eframe::egui_wgpu::{SurfaceConfig, WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "Janus - raster time series analysis

Usage:
  jn [options] [input ...]
  jn mcp [--threads N]  a server of the Model Context Protocol on the standard input and output: an
                      assistant (Claude Desktop, Claude Code...) describes series, reads their values,
                      runs the raster calculator, Zeit's tools, regularize, warp, zonal statistics, trains
                      and applies a Random Forest; results as files (docs/usage.md, MCP server)

Inputs:
  folder\\             every raster in the folder, 1 file = 1 date
  series.tif          1 multiband file, 1 band = 1 date
  a.tif b.tif ...     several files, 1 file = 1 date
  \"ndvi_*.tif\"        wildcard pattern (* and ?)
  https://.../cog.tif cloud-optimized GeoTIFFs on the web, read in place (only
  s3://, gs://, az:// the blocks shown are downloaded); several URLs = a series
  clim.nc, data.zarr  NetCDF / Zarr / HDF: a variable's time steps (CF dates);
                      with several variables the window asks which one, or
                      name it: NETCDF:\"clim.nc\":t2m, ZARR:\"data.zarr\":/t2m
  plots.gpkg ...      vector files (GeoJSON, Shapefile, GeoPackage, KML, CSV
                      of points) open as vector layers over the series
  project.gpkg        a validation project, with its sample and labels
  Dates are read from the file name or the band description
  (YYYY-MM-DD, YYYYMMDD, YYYY_MM, YYYY, A2001001...).

Options:
  --band N            band used when each file is one date (default: 1)
  --budget MB         memory for the cube overview, this run only
                      (default: Settings, 1024)
  --threads N         processing threads, this run only (default: Settings);
                      with --selftest-zeit, Zeit's threads
  -h, --help          show this help
  --version           show the version

Developer options:
  --describe IN       print how IN opens (dates, size, CRS, variables of a NetCDF / Zarr, the
                      series of the centre pixel) and exit
  --measure-startup   print startup timings and exit after the first frame
  --screenshot PNG    save the window once the series is ready, print its timings and exit
  --detach-map        the map in a window of its own (with --screenshot, both windows saved)
  --settings          the Settings window open
  --interface modern|classic[,THEME]  the interface of this run (and its theme, e.g. classic,Janus or
                      modern,Light)
  --profile-tour IN   a fixed tour of IN (overview, cursor series, dates, modes, zoom and pan at
                      full resolution, an ROI), timed stage by stage
  --tour-json FILE    with --profile-tour: the timings as JSON
  --measure-cache on|off IN  time exact series and an ROI from the files, build the overview (and the
                      full-resolution cache if on), time the reads again from the cache, check them
                      against the files; without a window
  --measure-vectors IN VEC...  a series and vector files over it: reading, placing, drawing, picking and
                      extracting the series at every feature, timed
  --measure-validation IN  the strata of a map (a buffer of 1 px), its areas, a sample of 1000 points, the
                      map at them and the estimators, timed
  The state of the window once the series is open (for screenshots):
  --basemap ID|URL    a basemap under the series: esri, eox, osm or an XYZ URL
  --season A-B        a season (days of the year A to B, e.g. 152-243)
  --layer IN          IN added as a layer (repeatable)
  --map-view          a map panel
  --swipe             the swipe
  --lens              the swipe as a lens (a circle at the cursor)
  --minimap           the minimap in the map's corner
  --goto PLACE        the map centred on a place (as Go to, Ctrl+G, takes it)
  --pin X,Y[,CLASS]   a pin at source pixel (X, Y), of CLASS for the classifiers (repeatable)
  --view X,Y,S        the map centred on source pixel (X, Y), S screen pixels per source pixel (with
                      --screenshot: once its full-resolution tiles are drawn)
  --opacity A         the active layer's opacity (0 to 1)
  --blend MODE        the active layer's blend mode (normal, multiply, screen, add, darken, lighten)
  --date N            the date (a stack's band) shown, from 1
  --roi X0,Y0,X1,Y1   an ROI rectangle (X0,Y0,X1,Y1,X2,Y2,...: a polygon)
  --transect X0,Y0,X1,Y1,...  a transect (a line, or a polyline)
  --mode NAME[,OPT]    what the map shows: value|anomaly|diff|composite|similarity|mean|std|slope|min|max|
                      amplitude|r2|dropdate|dropmag|rgb|phase (similarity,shape|pin|roi: the curves' shape, against the
                      first pin or the ROI's mean rather than the cursor; phase,even: no brightness by the mean;
                      highlight=LO:HI: those values highlighted, the others dimmed)
  --analysis seasonal|cycles|atlas|dates|bands|classes|scatter|trend|areas|zonal  the Analysis panel at that tab
                      (seasonal,M: its years from month M; cycles,trend: the trend kept; bands,common|zscore;
                      atlas,shapes,groups,density|value|mean|amplitude|trend: the curves' shapes compared,
                      three groups found, the points' colour;
                      classes,transitions: the matrix of two dates;
                      classes,flows[,changes][,zoom=N][,small=PCT][,window]: the flows between dates
                      (small=0: no Other; window: in a window of its own); trend,run: the trend test run,
                      before --screenshot)
  --chart K=V,...     the chart's options: style (0 lines ... 5 bars), values (0 values, 1 anomaly, 2 z-score,
                      3 seasonal anomaly, 4 CUSUM), trend (0-2), spread (the ROI's, 0 mean ... 4 density),
                      stripes (0/1), layers (1: every visible layer's series), classes (a categorical layer:
                      0 steps + markers, 1 steps, 2 markers, 3 class blocks)
  --zeit TOOL[,TOOL]  the Zeit tools fitted on the chart (e.g. landtrendr), their panels open
  --zeit-run          and run on the raster (the ROI's window if --roi) once the series is loaded
  --zeit-param TOOL.ID=JSON|@FILE  a parameter of a --zeit tool (repeatable; @FILE: its JSON in FILE)
  --embedding-view VIEW  a layer of embeddings drawn as pca, similarity (to pin 1), change, direction,
                      direction-like (pin 1's change) or classes (a class from each --pin)
  --stac CATALOG;COLLECTION;W,S,E,N;START;END[;CLOUD]  the Search STAC catalog window filled in (CATALOG:
                      earth-search, planetary-computer, bdc or an API's URL) and its search run
  --stac-open         and the series found opened as a layer (its bands at first, every date)
  --expr TEXT|INDEX   the inputs shown as an expression of their bands, e.g. \"(N - R) / (N + R)\", or an
                      index of the Awesome Spectral Indices by name (EVI, NDWI, NBR...)
  --calc TEXT         Tools > Raster calculator open with TEXT (statements apart by ;)
  --calc-run          and run once the layers are open: its result added as a layer
  --palette TEXT      the command palette open with TEXT typed (screenshots)
  --window NAME,...   tool windows open once the layers are: regularize, speckle, warp, build, save,
                      polygonize (the active series), rasterize, zonal (the first vector layer), areas
                      (Analysis > Areas), classification (a Random Forest on the pins, their samples
                      checked), classification-forest (its settings, the maps of the series' runs beside it, a
                      --zeit-run's waited for), classification-twdtw (TWDTW on the pins, estimated), fields (Field delineation on the
                      whole image), fields-train (the model fine-tuned 2 epochs on the first vector layer's polygons in
                      the ROI); NAME-run also runs it, the screenshot waiting for its result
  --validation-tab N  a validation project (a .gpkg input) on tab N of its panel (0 Design, 1 Accuracy,
                      2 Maps, 3 Interpreters, 4 Report)
  --shots TOUR IN OUT  a website guide's screenshots (PNGs in OUT) in a window of 1680 x 1050: validation
                      (IN from tools/make_tutorial_data.py), landtrendr, ccdc, coded (tools/make_guide_data.py),
                      bfast, phenology (an EVI series)
  Self-tests (tools/make_selftest_data.py makes their inputs):
  --selftest-zeit IN...  every Zeit tool on the series without a window: a pixel fit, its Python calls, the
                      estimate and a raster run of 256 x 256 px (LandTrendr also 512 x 512), the outputs read
                      back (timings)
  --selftest-validation  the estimators against Olofsson et al. (2014), the design, strata, sample and project on
                      synthetic maps and, with a Python with Zeit (JANUS_PYTHON, else `python`), the estimates
                      against zeit.accuracy
  --selftest-embeddings  the embedding kernels, eigenvectors and PCA against known answers, timed
  --selftest-ui A B [C [D [E [F [G]]]]]  the interface driven frame by frame (A, then B as a layer;
                      C: several bands per date; D, E: categorical, with and without a colour table;
                      F: B in another CRS; G: spectra; then the basemap with local tiles, a Zeit run,
                      a polygonal ROI, vector layers, the interpretation, the exports, an animation
                      and a validation project on D)
  --selftest-reproject B F A  F (B reprojected) drawn on B's grid where PROJ puts it; the cursor, a pin and
                      an ROI on B, A and F
  --selftest-multidim H/clim.nc  a NetCDF of two variables: the picker, its levels, in place
  --selftest-composite C  the RGB composite of the test series C (five bands per date) against its files
  --selftest-embeddings-ui DIR [OUT]  a series of embeddings: its views, their GeoTIFFs checked (in OUT); DIR
                      another kind of series: AlphaEarth 2023-2024 of its visible area (or --roi) downloaded
                      first and added on top
  --dump-golden IN... OUT  the maps of the display modes and a sample of the statistics in OUT
                      (tools/compare_golden.py compares two such folders)
";

/// Windows: a GUI program has no console; one started from a terminal writes to it.
fn attach_console() {
    #[cfg(windows)]
    unsafe {
        unsafe extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        AttachConsole(u32::MAX); // ATTACH_PARENT_PROCESS
    }
}

/// PROJ and GDAL data (src/main.cpp, configureBundledData): those shipped with the program (share/ next to it,
/// beside its lib/, or in the macOS bundle's Resources), else those next to the GDAL libraries on the PATH (a
/// development environment), unless the environment names them. The GDAL drivers built as plugins (NetCDF, HDF5)
/// likewise: the bundled ones, else the development environment's. TLS certificates for GDAL's curl
/// (src/basemap.cpp, curlCaBundle).
fn configure_data() {
    let exe = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf()));
    let mut shares: Vec<std::path::PathBuf> = exe
        .iter()
        .flat_map(|d| [d.join("share"), d.join("..").join("share"), d.join("..").join("Resources").join("share")])
        .collect();
    // the folder of the GDAL loaded (Windows), then those of the GDALs on the PATH (a conda environment's bin/,
    // an OSGeo installation)
    let lib = if cfg!(windows) { "gdal.dll" } else { "libgdal.so" };
    let gdal_dirs: Vec<std::path::PathBuf> = loaded_gdal_dir()
        .into_iter()
        .chain(
            std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).filter(|d| d.join(lib).exists()).collect::<Vec<_>>())
                .unwrap_or_default(),
        )
        .collect();
    // next to the libraries: the program's folder (Windows), lib/ (Linux), the bundle's Frameworks (macOS); else
    // the environment's lib/gdalplugins (conda's activation names it, a PATH alone does not)
    let plugins = exe
        .iter()
        .flat_map(|d| {
            [
                d.join("gdalplugins"),
                d.join("lib").join("gdalplugins"),
                d.join("..").join("Frameworks").join("gdalplugins"),
            ]
        })
        .chain(gdal_dirs.iter().flat_map(|d| [d.join("..").join("lib").join("gdalplugins"), d.join("gdalplugins")]))
        .find(|p| p.is_dir());
    if std::env::var_os("GDAL_DRIVER_PATH").is_none()
        && let Some(p) = plugins
    {
        let _ = gdal::config::set_config_option("GDAL_DRIVER_PATH", &p.to_string_lossy());
    }
    // the conda-built curl looks for its certificates at its build prefix: the system's bundle, else the one
    // shipped in share/ssl (Windows: Schannel and the system's store)
    if !cfg!(windows)
        && ["CURL_CA_BUNDLE", "GDAL_CURL_CA_BUNDLE", "SSL_CERT_FILE"].iter().all(|k| std::env::var_os(k).is_none())
    {
        let system = [
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/etc/pki/tls/cacert.pem",
            "/etc/ssl/cert.pem",
        ]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists());
        let shipped = shares.iter().map(|s| s.join("ssl").join("cacert.pem")).find(|p| p.exists());
        if let Some(ca) = system.or(shipped) {
            let _ = gdal::config::set_config_option("CURL_CA_BUNDLE", &ca.to_string_lossy());
        }
    }
    shares.extend(gdal_dirs.iter().map(|d| d.join("..").join("share")));
    // PROJ's database: the package's, a conda environment's (share/proj), an OSGeo installation's (projlib next to
    // gdal.dll); the environment's PROJ_DATA / PROJ_LIB first when set. Checked: a PROJ without its database, or
    // with another version's, knows no EPSG code and every CRS fails ("not understood")
    let mut projs: Vec<std::path::PathBuf> = shares
        .iter()
        .map(|s| s.join("proj"))
        .chain(gdal_dirs.iter().flat_map(|d| [d.join("projlib"), d.join("share").join("proj")]))
        .filter(|p| p.join("proj.db").exists())
        .collect();
    if let Some(env) = ["PROJ_DATA", "PROJ_LIB"].iter().find_map(std::env::var_os) {
        projs.retain(|p| p.as_os_str() != env);
        projs.insert(0, env.into());
    }
    let proj_ok = || {
        let _quiet = janus_geo::reproject::Quiet::new();
        gdal::spatial_ref::SpatialRef::from_epsg(3857).is_ok()
    };
    for (k, proj) in projs.iter().enumerate() {
        if k > 0 || std::env::var_os("PROJ_DATA").is_none() && std::env::var_os("PROJ_LIB").is_none() {
            let _ = gdal::config::set_config_option("PROJ_DATA", &proj.to_string_lossy());
            let c = std::ffi::CString::new(proj.to_string_lossy().as_bytes()).unwrap_or_default();
            let paths = [c.as_ptr(), std::ptr::null()];
            unsafe { gdal_sys::OSRSetPROJSearchPaths(paths.as_ptr()) };
        }
        if proj_ok() {
            break;
        }
    }
    let gdal_data = shares
        .iter()
        .map(|s| s.join("gdal"))
        .chain(gdal_dirs.iter().map(|d| d.join("gdal-data")))
        .find(|p| p.join("gdalvrt.xsd").exists());
    if std::env::var_os("GDAL_DATA").is_none()
        && let Some(gdal) = gdal_data
    {
        let _ = gdal::config::set_config_option("GDAL_DATA", &gdal.to_string_lossy());
    }
}

/// The folder of the gdal.dll this process loaded (it may not be the one it was built with: the first on the
/// PATH, e.g. an OSGeo installation's).
#[cfg(windows)]
fn loaded_gdal_dir() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
        fn GetModuleFileNameW(module: *mut std::ffi::c_void, out: *mut u16, size: u32) -> u32;
    }
    let name: Vec<u16> = "gdal.dll".encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated name; the buffer's size given.
    unsafe {
        let m = GetModuleHandleW(name.as_ptr());
        if m.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 32768];
        let n = GetModuleFileNameW(m, buf.as_mut_ptr(), buf.len() as u32) as usize;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..n.min(buf.len())]));
        path.parent().map(|p| p.to_path_buf())
    }
}

#[cfg(not(windows))]
fn loaded_gdal_dir() -> Option<std::path::PathBuf> {
    None
}

/// The window's icon (the taskbar, Alt+Tab; the program's own on Windows and in the macOS bundle).
fn window_icon() -> Arc<egui::IconData> {
    let png = include_bytes!("../../../resources/janus.png");
    Arc::new(image::load_from_memory(png).map_or_else(
        |_| egui::IconData::default(),
        |i| {
            let i = i.to_rgba8();
            egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() }
        },
    ))
}

/// How many files this process may open: on Unix its soft limit, raised first to the hard one (at most 8192;
/// macOS starts programs at 256); on Windows, as many as the pool will want.
fn open_files_limit() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: getrlimit / setrlimit with a valid struct.
        unsafe {
            let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) != 0 {
                return 256;
            }
            let want = r.rlim_max.min(8192);
            if r.rlim_cur < want && libc::setrlimit(libc::RLIMIT_NOFILE, &libc::rlimit { rlim_cur: want, ..r }) == 0 {
                r.rlim_cur = want;
            }
            r.rlim_cur as usize
        }
    }
    #[cfg(not(unix))]
    {
        4000
    }
}

fn main() -> eframe::Result<()> {
    let t0 = Instant::now();
    configure_data();
    // GDAL's block cache at 512 MB unless the environment sets it: closing a series of 41 files took 15-20 s with
    // GDAL's default (commit 9d6019c)
    if std::env::var_os("GDAL_CACHEMAX").is_none() {
        let _ = gdal::config::set_config_option("GDAL_CACHEMAX", "512");
    }
    // GDAL's pool of the files VRTs read (100 by default): a calculator's band over a series of more dates closed
    // and reopened its files block after block (a reduction over 107 dates: 6 s instead of 3); half of the files
    // this process may open
    if std::env::var_os("GDAL_MAX_DATASET_POOL_SIZE").is_none() {
        let n = (open_files_limit() / 2).clamp(100, 2000);
        let _ = gdal::config::set_config_option("GDAL_MAX_DATASET_POOL_SIZE", &n.to_string());
    }
    gdal::DriverManager::register_all();
    // --budget: this run only (0: the Settings value)
    let mut opts = app::Options::default();
    let mut args = std::env::args().skip(1);
    // `janus mcp [--threads N]`: the MCP server on the standard input and output (mcp.rs)
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        let all: Vec<String> = std::env::args().skip(2).collect();
        let threads = all
            .iter()
            .position(|a| a == "--threads")
            .and_then(|i| all.get(i + 1)?.parse::<usize>().ok())
            .unwrap_or_else(|| match settings::load().0.threads {
                0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
                n => n,
            });
        std::process::exit(mcp::serve(threads));
    }
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                attach_console();
                print!("{USAGE}");
                return Ok(());
            }
            "--version" => {
                attach_console();
                println!("Janus {} (GDAL {})", env!("CARGO_PKG_VERSION"), janus_cube::gdal_version());
                return Ok(());
            }
            "--budget" => opts.budget_mb = args.next().and_then(|v| v.parse().ok()).unwrap_or(1024).max(64),
            "--band" => opts.band = args.next().and_then(|v| v.parse().ok()).unwrap_or(1).max(1),
            "--threads" => opts.threads = args.next().and_then(|v| v.parse::<usize>().ok()).map(|n| n.max(1)),
            "--measure-startup" => opts.measure_startup = true,
            "--screenshot" => opts.screenshot = args.next().map(Into::into),
            "--detach-map" => opts.detach_map = true,
            "--settings" => opts.settings_open = true,
            "--interface" => opts.interface = args.next(),
            "--profile-tour" => opts.profile_tour = true,
            "--measure-vectors" => opts.measure_vectors = true,
            "--selftest-embeddings-ui" => opts.selftest_embeddings_ui = true,
            "--tour-json" => opts.tour_json = args.next().map(Into::into),
            "--dump-golden" => opts.dump_golden = Some(Default::default()),
            "--basemap" => opts.basemap = args.next(),
            "--layer" => opts.extra_layers.extend(args.next()),
            "--map-view" => opts.map_view = true,
            "--validation-tab" => opts.validation_tab = args.next().and_then(|v| v.parse().ok()),
            "--swipe" => opts.swipe = true,
            "--lens" => opts.lens = true,
            "--minimap" => opts.minimap = true,
            "--goto" => opts.goto = args.next(),
            "--zeit" => opts.zeit_tool = args.next(),
            "--zeit-run" => opts.zeit_run = true,
            "--zeit-param" => opts.zeit_params.extend(args.next()),
            "--embedding-view" => opts.embedding_view = args.next(),
            "--stac" => opts.stac = args.next(),
            "--stac-open" => opts.stac_open = true,
            "--expr" => opts.expr = args.next(),
            "--calc" => opts.calc = args.next(),
            "--calc-run" => opts.calc_run = true,
            "--palette" => opts.palette = args.next(),
            "--window" => opts.windows.extend(args.next().unwrap_or_default().split(',').map(|w| w.trim().to_string())),
            "--view" => {
                let v: Vec<f64> =
                    args.next().unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect();
                if let [x, y, s] = v[..] {
                    opts.view = Some([x, y, s]);
                }
            }
            "--opacity" => opts.opacity = args.next().and_then(|v| v.parse().ok()),
            "--blend" => opts.blend = args.next().and_then(|v| janus_gpu::Blend::parse(&v)),
            "--date" => opts.date = args.next().and_then(|v| v.parse().ok()),
            "--chart" => opts.chart = args.next(),
            "--pin" => {
                let a = args.next().unwrap_or_default();
                let parts: Vec<&str> = a.split(',').map(str::trim).collect();
                let v: Vec<usize> = parts.iter().take(2).filter_map(|v| v.parse().ok()).collect();
                match (&v[..], parts.get(2)) {
                    ([x, y], Some(class)) if !class.is_empty() => opts.class_pins.push((*x, *y, class.to_string())),
                    ([x, y], _) => opts.pins.push((*x, *y)),
                    _ => {}
                }
            }
            "--roi" => {
                let v: Vec<f64> =
                    args.next().unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect();
                if let [a, b, c, d] = v[..] {
                    opts.roi = Some([a as i64, b as i64, c as i64, d as i64]);
                } else if v.len() >= 6 && v.len().is_multiple_of(2) {
                    opts.roi_polygon = v;
                }
            }
            "--mode" => opts.mode = args.next(),
            "--analysis" => {
                let a = args.next().unwrap_or_default();
                let (tab, month) = a.split_once(',').unwrap_or((&a, ""));
                opts.analysis = match tab {
                    "classes" => Some(1),
                    "scatter" => Some(2),
                    "trend" => Some(3),
                    "areas" => Some(4),
                    "zonal" => Some(5),
                    "bands" => Some(6),
                    "cycles" => Some(7),
                    "atlas" => Some(9),
                    "dates" => Some(10),
                    _ => Some(0),
                };
                opts.analysis_view = month.trim().to_string();
            }
            "--transect" => {
                opts.transect =
                    args.next().unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect();
            }
            "--selftest-reproject" => opts.selftest_reproject = true,
            "--selftest-ui" => opts.selftest_ui = true,
            "--shots" => opts.shots = args.next(),
            "--selftest-multidim" => opts.selftest_multidim = true,
            "--season" => {
                opts.season = args.next().and_then(|v| {
                    let (a, b) = v.split_once('-')?;
                    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
                })
            }
            "--selftest-composite" => {
                // C (five bands per date) with an overview at 1:2, as the C++'s test opens it
                opts.selftest_composite = true;
                opts.budget_bytes = Some(10 * 100 * 60 * 4 + 4096);
            }
            "--selftest-embeddings" => {
                attach_console();
                let threads = opts.threads.unwrap_or_else(|| settings::load().0.threads);
                let threads = if threads > 0 { threads } else { janus_session::default_processing_threads() };
                std::process::exit(match janus_embed::selftest::run(threads) {
                    Ok(r) => {
                        println!("{r}OK");
                        0
                    }
                    Err((r, e)) => {
                        println!("{r}FAIL: {e}");
                        1
                    }
                });
            }
            "--selftest-validation" => {
                attach_console();
                // a Python with what the Copy Python code imports, to compare with zeit.accuracy
                let python = std::env::var("JANUS_PYTHON").unwrap_or_else(|_| "python".into());
                let has = std::process::Command::new(&python)
                    .args(["-c", "import zeit, geopandas, rioxarray, xarray"])
                    .output()
                    .is_ok_and(|o| o.status.success());
                let mut rep = String::new();
                let r = janus_validation::selftest::self_test(&mut rep, has.then_some(python.as_str()));
                if !has {
                    rep += "  (no Python with Zeit, geopandas and rioxarray: zeit.accuracy not compared)\n";
                }
                std::process::exit(match r {
                    Ok(()) => {
                        println!("{rep}OK");
                        0
                    }
                    Err(e) => {
                        println!("{rep}FAIL: {e}");
                        1
                    }
                });
            }
            "--measure-validation" => {
                attach_console();
                let Some(p) = args.next() else {
                    println!("--measure-validation needs a map");
                    std::process::exit(2);
                };
                std::process::exit(janus_validation::selftest::measure(&p));
            }
            "--selftest-zeit" => {
                attach_console();
                let threads = opts.threads.unwrap_or_else(|| settings::load().0.threads);
                std::process::exit(selftest_zeit::selftest_zeit(&args.by_ref().collect::<Vec<_>>(), threads));
            }
            "--describe" => {
                attach_console();
                std::process::exit(measure::describe(&args.by_ref().collect::<Vec<_>>()));
            }
            "--measure-cache" => {
                attach_console();
                std::process::exit(measure::measure_cache(&args.by_ref().collect::<Vec<_>>()));
            }
            _ => opts.inputs.push(a),
        }
    }
    if opts.shots.is_some() {
        // the tour opens its input itself
        opts.shots_args = std::mem::take(&mut opts.inputs);
        if opts.shots_args.len() != 2 {
            attach_console();
            eprintln!("--shots TOUR needs its input and the output folder");
            std::process::exit(2);
        }
    }
    if opts.selftest_ui && opts.inputs.len() >= 2 {
        // A opens, the others are opened by the test
        opts.extra_layers = opts.inputs.split_off(1);
    }
    if opts.selftest_reproject && opts.inputs.len() == 3 {
        // B opens, F and A are added as layers by the test
        opts.extra_layers = opts.inputs.split_off(1);
    }
    if opts.selftest_embeddings_ui && opts.inputs.len() >= 2 {
        // the folder of embeddings opens; OUT: where its files are written
        opts.selftest_embeddings_out = opts.inputs.pop().map(Into::into);
    }
    if opts.measure_vectors && opts.inputs.len() >= 2 {
        // the series opens, the vector files are added by the measure
        opts.measure_vector_files = opts.inputs.split_off(1);
    }
    if opts.dump_golden.is_some() {
        opts.dump_golden = opts.inputs.pop().map(Into::into);
    }
    let test_window = opts.measure_startup
        || opts.screenshot.is_some()
        || opts.profile_tour
        || opts.measure_vectors
        || opts.selftest_embeddings_ui
        || opts.dump_golden.is_some()
        || opts.selftest_composite
        || opts.selftest_reproject
        || opts.selftest_ui
        || opts.selftest_multidim
        || opts.shots.is_some();
    opts.maximize = !test_window;
    if test_window {
        attach_console();
    }

    let gdal_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let mut setup = WgpuSetupCreateNew::without_display_handle();
    // Windows: Direct3D 12 alone (WGPU_BACKEND chooses another). wgpu's default also tries Vulkan and OpenGL, and
    // enumerating them all cost ~120 ms more before the first frame (RUST_MIGRATION.md, phase 1)
    if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }
    // the GPU's memory in blocks of 8 to 64 MB, an overview larger than that in one of its own: in wgpu's blocks of
    // 128 to 256 MB (Performance) an overview of 130 MB took a block of 256 MB alone, and on Direct3D 12 the blocks
    // count in the process' memory too (8 maps opened as layers: 1.06 GB allocated in 2.24 GB of blocks)
    setup.device_descriptor = Arc::new(|adapter| eframe::wgpu::DeviceDescriptor {
        label: Some("janus"),
        required_limits: janus_gpu::required_limits(adapter),
        memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    });
    let shots = opts.shots.is_some();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Janus")
            .with_inner_size(if shots {
                shots::SIZE
            } else if test_window {
                [1600.0, 950.0]
            } else {
                [1400.0, 900.0]
            })
            // maximized once shown (State::ui, frame 1): eframe makes the window hidden and shows it after the
            // first frame, and a hidden window made maximized on Windows opens at its normal size while it holds
            // itself for maximized (the first double click on the title bar then "restores" it: nothing happens)
            .with_drag_and_drop(true)
            .with_icon(window_icon())
            // translucent panels (Settings) need a transparent window, made at the start
            .with_transparent(!shots && settings::load().0.panel_opacity < 1.0),
        wgpu_options: WgpuConfiguration {
            wgpu_setup: WgpuSetup::CreateNew(setup),
            surface: SurfaceConfig {
                present_mode: eframe::wgpu::PresentMode::AutoVsync,
                desired_maximum_frame_latency: Some(1),
            },
            ..Default::default()
        },
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native("Janus", options, Box::new(move |cc| Ok(Box::new(app::JanusApp::new(cc, opts, t0, gdal_ms)))))?;
    let rc = app::EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
    if rc != 0 {
        std::process::exit(rc);
    }
    Ok(())
}
