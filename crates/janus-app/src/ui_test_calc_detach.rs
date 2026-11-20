//! `--selftest-ui` stages 385-388: Tools > Raster calculator in a window of its own (an egui viewport). 385 opens the
//! calculator and detaches it; 386 checks it is drawn in a viewport other than the main window's (its expression
//! checked there, by the window's own frames) and runs it; 387 waits for the result's layer and brings the calculator
//! back into the main window; 388 checks it is drawn there again and puts everything back. Then 299, the end.

use std::sync::Mutex;
use std::time::Instant;

use super::UiTest;
use crate::app::State;

/// What the stages share: the layers and the active one before the run.
static KEPT: Mutex<Option<(usize, usize)>> = Mutex::new(None);

impl State {
    pub(super) fn test_calc_detach(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(Instant::now());
            st.frames = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        match stage {
            385 => {
                let (ls, at) = self.calc_layers();
                let a = ls[at.iter().position(|&k| k == self.active)?].alias.clone();
                *KEPT.lock().unwrap() = Some((self.layers.len(), self.active));
                self.calc.show = true;
                self.calc.detached = false;
                self.calc.text = format!("max({a}) + 1");
                self.calc.out_name = "Detached calculator".into();
                self.calc.name_typed = true;
                self.calc.primary = None;
                self.calc.cog = false;
                self.calc.live.on = false;
                self.calc.live.chart = false;
                self.calc.detached = true;
                next(st, "Tools > Raster calculator: opened and detached into a window of its own");
            }
            386 => {
                st.frames += 1;
                if st.frames < 6 {
                    return None;
                }
                match self.calc.viewport {
                    Some(v) if v != egui::ViewportId::ROOT => {}
                    other => return fail(&format!("the calculator should be drawn in its own viewport: {other:?}")),
                }
                if !self.calc.check.as_ref().is_some_and(|c| c.result.is_ok()) {
                    return fail("the calculator's expression should be checked (and fine) in its window");
                }
                self.calc_run(ctx);
                next(st, "the calculator is in its own viewport and runs max + 1 from there");
            }
            387 => {
                let (before, _) = (*KEPT.lock().unwrap())?;
                if self.opening.is_some() || self.layers.len() != before + 1 {
                    if waited > 90.0 {
                        return fail("the detached calculator's result should open as a layer");
                    }
                    return None;
                }
                if self.calc.viewport.is_none() {
                    return fail("the calculator should still be in its viewport while the result opens");
                }
                self.calc.detached = false;
                next(st, "the detached calculator made a layer; back into the main window");
            }
            388 => {
                st.frames += 1;
                if st.frames < 6 {
                    return None;
                }
                if self.calc.viewport.is_some() || self.calc.detached {
                    return fail("the calculator should be in the main window again");
                }
                if !ctx.memory(|m| m.area_rect(egui::Id::new("calculator window"))).is_some_and(|r| r.width() > 1.0) {
                    return fail("the calculator's window should be drawn in the main window again");
                }
                let (before, _) = (*KEPT.lock().unwrap())?;
                self.calc.show = false;
                for i in (before..self.layers.len()).rev() {
                    self.remove_layer(i, ctx);
                }
                *KEPT.lock().unwrap() = None;
                next(st, "the calculator is back in the main window; everything put back");
                st.stage = 299;
            }
            _ => {}
        }
        None
    }
}
