//! The first frame of an empty eframe window, to see what the start costs apart from Janus:
//!   startup_probe [vulkan|dx12|gl] [noaccess] [nofonts]
use std::time::Instant;

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};

struct Probe {
    t0: Instant,
    frame: u32,
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| ui.label("probe"));
        self.frame += 1;
        if self.frame == 2 {
            println!("  first frame {:.0} ms", self.t0.elapsed().as_secs_f64() * 1e3);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx().request_repaint();
    }
}

fn main() {
    let t0 = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let has = |a: &str| args.iter().any(|x| x == a);
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    for (name, b) in [("vulkan", wgpu::Backends::VULKAN), ("dx12", wgpu::Backends::DX12), ("gl", wgpu::Backends::GL)] {
        if has(name) {
            setup.instance_descriptor.backends = b;
        }
    }
    if has("select") {
        setup.native_adapter_selector =
            Some(std::sync::Arc::new(|adapters: &[wgpu::Adapter], _surface: Option<&wgpu::Surface<'_>>| {
                let rank = |a: &wgpu::Adapter| match a.get_info().device_type {
                    wgpu::DeviceType::DiscreteGpu => 0,
                    wgpu::DeviceType::IntegratedGpu => 1,
                    wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => 2,
                    wgpu::DeviceType::Cpu => 3,
                };
                adapters.iter().min_by_key(|a| rank(a)).cloned().ok_or_else(|| "no adapter".to_string())
            }));
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([800.0, 600.0]),
        wgpu_options: WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() },
        ..Default::default()
    };
    let noaccess = has("noaccess");
    let nofonts = has("nofonts");
    eframe::run_native(
        "probe",
        options,
        Box::new(move |cc| {
            println!("  window + wgpu {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);
            if noaccess {
                cc.egui_ctx.disable_accesskit();
            }
            if nofonts {
                cc.egui_ctx.set_fonts(egui::FontDefinitions::empty());
            }
            Ok(Box::new(Probe { t0, frame: 0 }))
        }),
    )
    .unwrap();
}
