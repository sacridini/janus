//! Janus in Rust: raster time series and data cubes, explored and analysed (RUST_MIGRATION.md). Phase 1, the
//! skeleton: a series opened with GDAL, its overview (shared with the C++'s cache), the map and its statistics on
//! the GPU, the dock of panels, the time series chart; the startup measured as the C++ measures it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;
mod animation;
mod app;
mod basemap;
mod canvas;
mod chart;
mod chips;
mod classes;
mod composite;
mod display;
mod emb_download;
mod embeddings;
mod embeddings_test;
mod export_map;
mod exports;
mod files;
mod gif;
mod golden;
mod labels;
mod layers;
mod map;
mod measure;
mod measure_vectors;
mod open;
mod panels;
mod platform;
mod python;
mod roi;
mod season;
mod selftest_zeit;
mod series;
mod session;
mod settings;
mod shots;
mod shots_validation;
mod theme;
mod tour;
mod transect;
mod ui_test;
mod validation;
mod validation_tabs;
mod vectors;
mod views;
mod zeit;

use std::sync::Arc;
use std::time::Instant;

use eframe::egui_wgpu::{SurfaceConfig, WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "Janus (Rust, phase 1) - raster time series analysis

Usage:
  janus [options] [input ...]

Inputs:
  folder\\             every raster in the folder, 1 file = 1 date
  series.tif          1 multiband file, 1 band = 1 date
  a.tif b.tif ...     several files, 1 file = 1 date
  \"ndvi_*.tif\"        wildcard pattern (* and ?)

Options:
  --budget MB         memory for the cube overview (default: 1024)
  --measure-startup   print startup timings and exit after the first frame
  --screenshot PNG    save the window once the series is ready, print its timings and exit
  --detach-map        the map in a window of its own (with --screenshot, both windows saved)
  --profile-tour IN   a fixed tour of IN (overview, cursor series, dates, modes, zoom and pan at
                      full resolution, an ROI), timed stage by stage as the C++'s
  --tour-json FILE    with --profile-tour: the timings as JSON
  --basemap ID|URL    a basemap under the series once it opens: esri, eox, osm or an XYZ URL
  --season A-B        a season (days of the year A to B, e.g. 152-243) once the series opens
  --layer IN          IN added as a layer once the series is open (repeatable)
  --map-view          a map panel once the series are open
  --validation-tab N  a validation project opened (a .gpkg input): its panel on tab N (0 Design, 1 Accuracy,
                      2 Maps, 3 Interpreters, 4 Report)
  --swipe             the swipe once the series are open
  --pin X,Y           a pin at source pixel (X, Y) once the series is open (repeatable)
  --roi X0,Y0,X1,Y1   an ROI rectangle once the series is open (X0,Y0,X1,Y1,X2,Y2,...: a polygon)
  --transect X0,Y0,X1,Y1,...  a transect (a line, or a polyline) once the series is open
  --analysis seasonal|classes|scatter  the Analysis panel at that tab
  --zeit TOOL         the Zeit tool fitted on the chart (e.g. landtrendr), its window open
  --zeit-run          and run on the raster (the ROI's window if --roi) once the series is loaded
  --selftest-ui A B [C [D [E]]]  the layers workflow on the test series of tools/make_selftest_data.py:
                      layers, cursor, pins, the map, NDVI, change maps, composites, the season, classes
  --shots TOUR IN OUT  a website guide's screenshots (PNGs in OUT) in a window of 1680 x 1050: validation
                      (IN from tools/make_tutorial_data.py), landtrendr, ccdc, coded (tools/make_guide_data.py),
                      bfast, phenology (an EVI series)
  --selftest-reproject B F A  F (B reprojected) drawn on B's grid where PROJ puts it; the cursor, a pin and
                      an ROI on B, A and F
  --selftest-multidim H/clim.nc  a NetCDF of two variables: the picker, its levels, in place
  --selftest-composite C  the RGB composite of the test series C (five bands per date) against its files
  --dump-golden IN... OUT  the maps of the display modes and a sample of the statistics in OUT, as the
                      C++'s --dump-golden writes them (tools/compare_golden.py compares the two)
  --selftest-validation  the estimators against Olofsson et al. (2014), the design, strata, sample and project on
                      synthetic maps and, with a Python with Zeit (JANUS_PYTHON, else `python`), the estimates
                      against zeit.accuracy
  --measure-validation IN  the strata of a map (a buffer of 1 px), its areas, a sample of 1000 points, the
                      map at them and the estimators, timed
  --selftest-embeddings  the embedding kernels, eigenvectors and PCA against known answers, timed
  --selftest-embeddings-ui DIR [OUT]  a series of embeddings: its views, their GeoTIFFs checked (in OUT); DIR
                      another kind of series: AlphaEarth 2023-2024 of its visible area (or --roi) downloaded
                      first and added on top
  --selftest-zeit IN...  every Zeit tool on the series without a window: a pixel fit, its Python calls, the
                      estimate and a raster run of 256 x 256 px (LandTrendr also 512 x 512), the outputs read
                      back (timings)
  --describe IN       print how IN opens (dates, size, CRS, variables of a NetCDF / Zarr, the
                      series of the centre pixel) and exit
  --measure-cache on|off IN  time exact series and an ROI from the files, build the overview (and the
                      full-resolution cache if on), time the reads again from the cache, check them
                      against the files; without a window
  --measure-vectors IN VEC...  a series and vector files over it: reading, placing, drawing, picking and
                      extracting the series at every feature, timed
  -h, --help          show this help
  --version           show the version
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
/// development environment), unless the environment names them. The GDAL drivers built as plugins (NetCDF, HDF5):
/// the bundled ones only. TLS certificates for GDAL's curl (src/basemap.cpp, curlCaBundle).
fn configure_data() {
    let exe = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf()));
    let mut shares: Vec<std::path::PathBuf> = exe
        .iter()
        .flat_map(|d| [d.join("share"), d.join("..").join("share"), d.join("..").join("Resources").join("share")])
        .collect();
    // next to the libraries: the program's folder (Windows), lib/ (Linux), the bundle's Frameworks (macOS)
    if let Some(d) = &exe
        && let Some(plugins) = [
            d.join("gdalplugins"),
            d.join("lib").join("gdalplugins"),
            d.join("..").join("Frameworks").join("gdalplugins"),
        ]
        .into_iter()
        .find(|p| p.is_dir())
    {
        let _ = gdal::config::set_config_option("GDAL_DRIVER_PATH", &plugins.to_string_lossy());
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
    if let Some(path) = std::env::var_os("PATH") {
        let lib = if cfg!(windows) { "gdal.dll" } else { "libgdal.so" };
        shares
            .extend(std::env::split_paths(&path).filter(|d| d.join(lib).exists()).map(|d| d.join("..").join("share")));
    }
    if std::env::var_os("PROJ_DATA").is_none()
        && std::env::var_os("PROJ_LIB").is_none()
        && let Some(proj) = shares.iter().map(|s| s.join("proj")).find(|p| p.join("proj.db").exists())
    {
        let _ = gdal::config::set_config_option("PROJ_DATA", &proj.to_string_lossy());
        let c = std::ffi::CString::new(proj.to_string_lossy().as_bytes()).unwrap_or_default();
        let paths = [c.as_ptr(), std::ptr::null()];
        unsafe { gdal_sys::OSRSetPROJSearchPaths(paths.as_ptr()) };
    }
    if std::env::var_os("GDAL_DATA").is_none()
        && let Some(gdal) = shares.iter().map(|s| s.join("gdal")).find(|p| p.join("gdalvrt.xsd").exists())
    {
        let _ = gdal::config::set_config_option("GDAL_DATA", &gdal.to_string_lossy());
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
    gdal::DriverManager::register_all();
    // --budget: this run only (0: the Settings value)
    let mut opts = app::Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                attach_console();
                print!("{USAGE}");
                return Ok(());
            }
            "--version" => {
                attach_console();
                println!("Janus {} (Rust; GDAL {})", env!("CARGO_PKG_VERSION"), janus_cube::gdal_version());
                return Ok(());
            }
            "--budget" => opts.budget_mb = args.next().and_then(|v| v.parse().ok()).unwrap_or(1024).max(64),
            "--measure-startup" => opts.measure_startup = true,
            "--screenshot" => opts.screenshot = args.next().map(Into::into),
            "--detach-map" => opts.detach_map = true,
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
            "--zeit" => opts.zeit_tool = args.next(),
            "--zeit-run" => opts.zeit_run = true,
            "--pin" => {
                let v: Vec<usize> =
                    args.next().unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect();
                if let [x, y] = v[..] {
                    opts.pins.push((x, y));
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
            "--analysis" => {
                opts.analysis = match args.next().as_deref() {
                    Some("classes") => Some(1),
                    Some("scatter") => Some(2),
                    _ => Some(0),
                }
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
                let threads = settings::load().0.threads;
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
                let threads = settings::load().0.threads;
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
    setup.device_descriptor = Arc::new(|adapter| eframe::wgpu::DeviceDescriptor {
        label: Some("janus"),
        required_limits: janus_gpu::required_limits(adapter),
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
            .with_maximized(!test_window)
            .with_drag_and_drop(true)
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
