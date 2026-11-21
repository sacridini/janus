//! `--selftest-ui` stages 410-412: the menu bar's top menus switch on hover. 410 hovers each top menu with none open
//! (nothing may open); 411 takes every ordered pair of top menus (File, Edit, View, Tools, Validation, Help): clicks the
//! first, checks it is open, moves the pointer onto the second without a click and checks the second is open and the
//! first closed; 412 puts the pointer and the menus back.

use std::sync::Mutex;
use std::time::Instant;

use egui::{Event, PointerButton, Pos2};

use super::UiTest;
use crate::app::{State, bar_menu_open, bar_menu_rects};

const LABELS: [&str; 6] = ["File", "Edit", "View", "Tools", "Validation & uncertainty", "Help"];

/// Where the stages are: the step of the current pair (or of the hover with nothing open), the frames to wait before
/// it, and the pair's index.
static AT: Mutex<(usize, u8, usize)> = Mutex::new((0, 0, 0));

fn press(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

impl State {
    pub(super) fn test_menus(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let rect_of = |label: &str| bar_menu_rects(ctx).into_iter().find(|(l, _)| l == label).map(|r| r.1);
        // events given now reach the pointer at the next frame, their effect is seen at the one after
        {
            let mut at = AT.lock().unwrap();
            if at.1 > 0 {
                at.1 -= 1;
                return None;
            }
        }
        let (step, _, pair) = *AT.lock().unwrap();
        let go = |step: usize, pair: usize, events: Vec<Event>, this: &mut State| {
            this.roi_edit.test_pointer = true;
            this.roi_edit.events.extend(events);
            *AT.lock().unwrap() = (step, 2, pair);
        };
        let away = Pos2::new(600.0, 500.0);
        match stage {
            410 => {
                let r = rect_of(LABELS[step.min(LABELS.len() - 1)])?;
                if step == 0 {
                    egui::Popup::close_all(ctx);
                    // a window the earlier stages left open (the Tasks window opens with a run) may cover the bar
                    self.zeit.show_tasks = false;
                }
                if step > 0
                    && let Some(open) = bar_menu_open(ctx)
                {
                    return fail(&format!("hovering {} opened {open} with no menu open", LABELS[step - 1]));
                }
                if step == LABELS.len() {
                    println!(
                        "[{:5.1} s] hovering the top menus with none open opens nothing",
                        t0.elapsed().as_secs_f64()
                    );
                    *AT.lock().unwrap() = (0, 0, 0);
                    st.stage = 411;
                    return None;
                }
                go(step + 1, 0, vec![Event::PointerMoved(r.center())], self);
            }
            411 => {
                let n = LABELS.len();
                // the pair: (a, b) for a != b, in order
                let (a, b) = (pair / (n - 1), {
                    let k = pair % (n - 1);
                    let a = pair / (n - 1);
                    if k >= a { k + 1 } else { k }
                });
                if pair >= n * (n - 1) {
                    println!(
                        "[{:5.1} s] every top menu opens by hover from every other one that is open (30 pairs), and closes it",
                        t0.elapsed().as_secs_f64()
                    );
                    *AT.lock().unwrap() = (0, 0, 0);
                    st.stage = 412;
                    return None;
                }
                let (Some(ra), Some(rb)) = (rect_of(LABELS[a]), rect_of(LABELS[b])) else {
                    return fail("the menu bar's buttons were not found");
                };
                let name = format!("{} -> {}", LABELS[a], LABELS[b]);
                match step {
                    0 => go(1, pair, vec![Event::PointerMoved(ra.center())], self),
                    1 => go(2, pair, vec![press(ra.center(), true)], self),
                    2 => go(3, pair, vec![press(ra.center(), false)], self),
                    3 => {
                        if bar_menu_open(ctx).as_deref() != Some(LABELS[a]) {
                            return fail(&format!("{name}: a click should open {}", LABELS[a]));
                        }
                        go(4, pair, vec![Event::PointerMoved(rb.center())], self);
                    }
                    4 => {
                        let open = bar_menu_open(ctx);
                        if open.as_deref() != Some(LABELS[b]) {
                            return fail(&format!("{name}: hovering {} should open it (open: {open:?})", LABELS[b]));
                        }
                        egui::Popup::close_all(ctx);
                        go(5, pair, vec![Event::PointerMoved(away)], self);
                    }
                    _ => {
                        if let Some(open) = bar_menu_open(ctx) {
                            return fail(&format!("{name}: {open} still open after closing"));
                        }
                        *AT.lock().unwrap() = (0, 0, pair + 1);
                    }
                }
            }
            _ => {
                self.roi_edit.test_pointer = false;
                self.roi_edit.events.clear();
                egui::Popup::close_all(ctx);
                println!("[{:5.1} s] the menus and the pointer put back", t0.elapsed().as_secs_f64());
                *AT.lock().unwrap() = (0, 0, 0);
                st.stage = 299;
                st.since = Some(Instant::now());
                st.frames = 0;
            }
        }
        None
    }
}
