//! Janus in Rust: raster time series and data cubes, explored and analysed (RUST_MIGRATION.md). Phase 1, the
//! skeleton: a series opened with GDAL, its overview (shared with the C++'s cache), the map and its statistics on
//! the GPU, the dock of panels, the time series chart; the startup measured as the C++ measures it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod basemap;
mod chart;
mod classes;
mod composite;
mod display;
mod files;
mod golden;
mod layers;
mod map;
mod measure;
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
mod theme;
mod tour;
mod ui_test;
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
  --swipe             the swipe once the series are open
  --pin X,Y           a pin at source pixel (X, Y) once the series is open (repeatable)
  --roi X0,Y0,X1,Y1   an ROI rectangle once the series is open (X0,Y0,X1,Y1,X2,Y2,...: a polygon)
  --zeit TOOL         the Zeit tool fitted on the chart (e.g. landtrendr), its window open
  --zeit-run          and run on the raster (the ROI's window if --roi) once the series is loaded
  --selftest-ui A B [C [D [E]]]  the layers workflow on the test series of tools/make_selftest_data.py:
                      layers, cursor, pins, the map, NDVI, change maps, composites, the season, classes
  --selftest-reproject B F A  F (B reprojected) drawn on B's grid where PROJ puts it; the cursor, a pin and
                      an ROI on B, A and F
  --selftest-multidim H/clim.nc  a NetCDF of two variables: the picker, its levels, in place
  --selftest-composite C  the RGB composite of the test series C (five bands per date) against its files
  --dump-golden IN... OUT  the maps of the display modes and a sample of the statistics in OUT, as the
                      C++'s --dump-golden writes them (tools/compare_golden.py compares the two)
  --selftest-zeit IN...  every Zeit tool on the series without a window: a pixel fit, its Python calls, the
                      estimate and a raster run of 256 x 256 px (LandTrendr also 512 x 512), the outputs read
                      back (timings)
  --describe IN       print how IN opens (dates, size, CRS, variables of a NetCDF / Zarr, the
                      series of the centre pixel) and exit
  --measure-cache on|off IN  time exact series and an ROI from the files, build the overview (and the
                      full-resolution cache if on), time the reads again from the cache, check them
                      against the files; without a window
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

fn main() -> eframe::Result<()> {
    let t0 = Instant::now();
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
            "--tour-json" => opts.tour_json = args.next().map(Into::into),
            "--dump-golden" => opts.dump_golden = Some(Default::default()),
            "--basemap" => opts.basemap = args.next(),
            "--layer" => opts.extra_layers.extend(args.next()),
            "--map-view" => opts.map_view = true,
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
            "--selftest-reproject" => opts.selftest_reproject = true,
            "--selftest-ui" => opts.selftest_ui = true,
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
    if opts.selftest_ui && opts.inputs.len() >= 2 {
        // A opens, the others are opened by the test
        opts.extra_layers = opts.inputs.split_off(1);
    }
    if opts.selftest_reproject && opts.inputs.len() == 3 {
        // B opens, F and A are added as layers by the test
        opts.extra_layers = opts.inputs.split_off(1);
    }
    if opts.dump_golden.is_some() {
        opts.dump_golden = opts.inputs.pop().map(Into::into);
    }
    let test_window = opts.measure_startup
        || opts.screenshot.is_some()
        || opts.profile_tour
        || opts.dump_golden.is_some()
        || opts.selftest_composite
        || opts.selftest_reproject
        || opts.selftest_ui
        || opts.selftest_multidim;
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Janus")
            .with_inner_size(if test_window { [1600.0, 950.0] } else { [1400.0, 900.0] })
            .with_maximized(!test_window)
            .with_drag_and_drop(true)
            // translucent panels (Settings) need a transparent window, made at the start
            .with_transparent(settings::load().0.panel_opacity < 1.0),
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
