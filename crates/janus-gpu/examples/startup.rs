//! Where wgpu's start goes, per backend: instance, adapter, device, the map's pipelines.
use std::time::Instant;

fn main() {
    let backends: &[(&str, wgpu::Backends)] = &[
        ("dx12", wgpu::Backends::DX12),
        ("vulkan", wgpu::Backends::VULKAN),
        ("gl", wgpu::Backends::GL),
        ("primary+gl", wgpu::Backends::PRIMARY | wgpu::Backends::GL),
    ];
    for &(name, b) in backends {
        let t = Instant::now();
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = b;
        let instance = wgpu::Instance::new(desc);
        let t_inst = t.elapsed().as_secs_f64() * 1e3;
        let all = pollster::block_on(instance.enumerate_adapters(b)).len();
        let t_enum = t.elapsed().as_secs_f64() * 1e3;
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })) else {
            println!("{name}: no adapter");
            continue;
        };
        let t_ad = t.elapsed().as_secs_f64() * 1e3;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: janus_gpu::required_limits(&adapter),
            ..Default::default()
        }))
        .unwrap();
        let t_dev = t.elapsed().as_secs_f64() * 1e3;
        let _gpu = janus_gpu::Gpu::new(&device, &queue);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let t_gpu = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "{name:11} instance {t_inst:5.1} | enumerate ({all}) {:5.1} | adapter {:5.1} | device {:5.1} | map pipelines {:5.1} ms  ({})",
            t_enum - t_inst,
            t_ad - t_enum,
            t_dev - t_ad,
            t_gpu - t_dev,
            adapter.get_info().name
        );
    }
}
