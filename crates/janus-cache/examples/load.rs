//! Times the overview of a series, read from its cache or built: `load <input>...`
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let inputs: Vec<String> = std::env::args().skip(1).collect();
    let t = Instant::now();
    let info = Arc::new(janus_cube::open_cube(&inputs).expect("open"));
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    let opts = janus_cache::OverviewOptions {
        budget_bytes: 1024 << 20,
        max_texture: 16384,
        cache_dir: janus_cache::overview::default_cache_dir(),
        threads: 12,
    };
    let t = Instant::now();
    let ov = janus_cache::Overview::start(info, &opts, || {});
    while !ov.complete() {
        std::thread::yield_now();
    }
    println!(
        "open {open_ms:.0} ms; overview {}x{}x{} {} in {:.0} ms ({} failed)",
        ov.w,
        ov.h,
        ov.t,
        if ov.from_cache() { "from its cache" } else { "built" },
        t.elapsed().as_secs_f64() * 1e3,
        ov.failed()
    );
}

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
