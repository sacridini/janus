//! `janus --dump-golden INPUT... OUT` (src/app_profile.cpp, dumpGoldenStep): what the C++ writes for the same
//! series, to compare the two: the overview's size, a sample of the GPU statistics, and the map of each display mode
//! that needs no options (400 × 300, the whole image) with its automatic stretch. tools/compare_golden.py compares
//! the two folders.

use std::fmt::Write as _;

use janus_cube::multidim::format_g;
use janus_gpu::Mode;

use crate::app::State;

/// The modes, in the C++'s order.
const MODES: [Mode; 11] = [
    Mode::Value,
    Mode::Anomaly,
    Mode::Mean,
    Mode::Std,
    Mode::Slope,
    Mode::Min,
    Mode::Max,
    Mode::Amplitude,
    Mode::R2,
    Mode::DropDate,
    Mode::DropMag,
];

/// The map's size, as the C++'s.
const SIZE: [u32; 2] = [400, 300];

#[derive(Default)]
pub struct Golden {
    stage: usize,
    step: usize,
    text: String,
}

fn g9(v: f32) -> String {
    format_g(f64::from(v), 9)
}

impl State {
    /// One frame of the dump; Some(exit code) when it is over.
    pub fn golden_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        let out = self.opts.dump_golden.clone()?;
        let g = &mut self.golden;
        match g.stage {
            0 => {
                if self.opts.inputs.is_empty() {
                    println!("usage: --dump-golden INPUT... OUT");
                    return Some(2);
                }
                let _ = std::fs::create_dir_all(&out);
                g.stage = 1;
            }
            1 => {
                // the overview on the GPU and its statistics, then a sample of the statistics
                let Some(s) = &self.session else {
                    if self.opening.is_none() {
                        println!("could not open: {}", self.error.clone().unwrap_or_default());
                        return Some(1);
                    }
                    return None;
                };
                let ov = &s.s.overview;
                if self.gpu.is_none()
                    || !ov.complete()
                    || !s.uploaded.iter().all(|&u| u)
                    || (s.info.t() >= 2 && s.stats.is_none())
                {
                    return None;
                }
                g.text = format!("size {} {} {} overview {} {}\n", s.info.width, s.info.height, s.info.t(), ov.w, ov.h);
                if let Some(st) = &s.stats {
                    for p in (0..ov.w * ov.h).step_by(997) {
                        let (a, b) = (st.s0[p], st.s1[p]);
                        let _ = writeln!(
                            g.text,
                            "stats {p} {} {} {} {} {} {} {} {} {}",
                            g9(a[0]),
                            g9(a[1]),
                            g9(a[2]),
                            g9(a[3]),
                            g9(b[0]),
                            g9(b[1]),
                            g9(b[2]),
                            g9(b[3]),
                            g9(st.s2[p])
                        );
                    }
                }
                self.display.mode = MODES[0];
                g.stage = 2;
            }
            2 => {
                // the mode's range was computed in this frame (pump): its map, then the next mode
                let s = self.session.as_ref()?;
                let gpu = self.gpu.as_mut()?;
                let cube = s.cube.as_ref()?;
                let (w, h) = (s.info.width as f64, s.info.height as f64);
                let scale = 0.98 * (f64::from(SIZE[0]) / w).min(f64::from(SIZE[1]) / h);
                // the C++'s offset is a float
                let off =
                    [((f64::from(SIZE[0]) - w * scale) * 0.5) as f32, ((f64::from(SIZE[1]) - h * scale) * 0.5) as f32];
                // as the C++'s screenRect: the offset (a float) and the scaled size added in double
                let (ox, oy) = (f64::from(off[0]), f64::from(off[1]));
                let rect = [ox as f32, oy as f32, (ox + w * scale) as f32, (oy + h * scale) as f32];
                let mut params = self.display.layer_params(s);
                if self.classes.on() && params.mode == Mode::Value {
                    params.class_lut = self.classes.lut.clone();
                }
                let mode = params.mode;
                let cmap = self.display.cmap[mode as usize];
                let mut f = gpu.begin_map(1, SIZE, crate::map::MAP_BACKGROUND);
                gpu.draw_cube(&mut f, cube, rect, &params, cmap, 1.0);
                gpu.end_map(f);
                let (_, _, rgba) = gpu.read_target(1)?;
                let path = out.join(format!("mode_{:02}.rgba", mode as usize));
                if let Err(e) = std::fs::write(&path, rgba) {
                    eprintln!("{}: {e}", path.display());
                }
                let r = self.display.ranges[mode as usize];
                let _ = writeln!(g.text, "range {} {} {} cmap {cmap}", mode as usize, g9(r.lo), g9(r.hi));
                g.step += 1;
                if g.step == MODES.len() {
                    gpu.release_target(1);
                    let path = out.join("golden.txt");
                    if let Err(e) = std::fs::write(&path, &g.text) {
                        eprintln!("{}: {e}", path.display());
                        return Some(1);
                    }
                    println!("golden: {} maps and {}", g.step, path.display());
                    g.stage = 3;
                    return Some(0);
                }
                self.display.mode = MODES[g.step];
            }
            _ => {}
        }
        None
    }
}
