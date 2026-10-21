//! Janus in Rust: raster time series and data cubes, explored and analysed (RUST_MIGRATION.md). Phase 1, the
//! skeleton: a series opened with GDAL, its overview (shared with the C++'s cache), the map and its statistics on
//! the GPU, the dock of panels, the time series chart; the startup measured as the C++ measures it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod chart;
mod display;
mod map;
mod panels;
mod session;
mod theme;

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
    let mut opts = app::Options { budget_mb: 1024, ..Default::default() };
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
            _ => opts.inputs.push(a),
        }
    }
    if opts.measure_startup || opts.screenshot.is_some() {
        attach_console();
    }

    // GDAL's block cache at 512 MB unless the environment sets it: closing a series of 41 files took 15-20 s with
    // GDAL's default (commit 9d6019c)
    if std::env::var_os("GDAL_CACHEMAX").is_none() {
        let _ = gdal::config::set_config_option("GDAL_CACHEMAX", "512");
    }
    gdal::DriverManager::register_all();
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
            .with_inner_size([1400.0, 900.0])
            .with_maximized(!opts.measure_startup && opts.screenshot.is_none())
            .with_drag_and_drop(true),
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
    eframe::run_native("Janus", options, Box::new(move |cc| Ok(Box::new(app::JanusApp::new(cc, opts, t0, gdal_ms)))))
}
