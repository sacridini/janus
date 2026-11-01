//! The line of downloads: at most N transfer at once (Settings > Downloads at once, 2 by default), the others wait
//! their turn in the order they were asked for. Each download runs on its own thread as before (a task of Tools >
//! Tasks with its bar, its log, Cancel); the thread asks for its turn first (`turn`) and keeps it while it
//! transfers (the `Turn` it gets back; dropped, the next one goes). While it waits its task says so ("waiting: 2
//! ahead"), it can be cancelled or moved to the front of the line, and the interface goes on as usual: nothing of
//! this runs on the interface's thread.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

use crate::zeit::JobStatus;

/// The downloads transferring at once by default.
pub const DEFAULT_AT_ONCE: usize = 2;

struct Line {
    /// how many may transfer at once, and how many do
    at_once: usize,
    running: usize,
    /// the tasks waiting, first in line first: their status' address (their key) and ticket
    waiting: VecDeque<(usize, u64)>,
    next: u64,
    /// woken when the line changes (a turn ends, the limit grows, a task moves up)
    ctx: Option<egui::Context>,
}

static LINE: LazyLock<(Mutex<Line>, Condvar)> = LazyLock::new(|| {
    (
        Mutex::new(Line { at_once: DEFAULT_AT_ONCE, running: 0, waiting: VecDeque::new(), next: 0, ctx: None }),
        Condvar::new(),
    )
});

/// A task's key in the line: its status' address (the same Arc the Tasks window and the notices hold).
pub fn key(status: &Arc<Mutex<JobStatus>>) -> usize {
    Arc::as_ptr(status) as usize
}

/// A download's turn: while it is held the download transfers; dropped (done, failed, cancelled), the next in line
/// starts.
pub struct Turn(());

impl Drop for Turn {
    fn drop(&mut self) {
        let (m, cv) = &*LINE;
        let mut l = m.lock().unwrap();
        l.running = l.running.saturating_sub(1);
        if let Some(c) = &l.ctx {
            c.request_repaint();
        }
        cv.notify_all();
    }
}

/// The context woken when the line moves (the Tasks window, the status bar and the map's notes redrawn).
pub fn set_context(ctx: &egui::Context) {
    LINE.0.lock().unwrap().ctx = Some(ctx.clone());
}

/// How many downloads transfer at once (1 to 8).
pub fn set_at_once(n: usize) {
    let (m, cv) = &*LINE;
    m.lock().unwrap().at_once = n.clamp(1, 8);
    cv.notify_all();
}

/// Waits for a download's turn (on its own thread): at once when fewer than the limit transfer and nobody waits
/// before it. Its status says how many are ahead while it waits. Err("cancelled") when it is cancelled waiting.
pub fn turn(status: &Arc<Mutex<JobStatus>>, cancel: &AtomicBool) -> Result<Turn, String> {
    let (m, cv) = &*LINE;
    let me = key(status);
    let mut l = m.lock().unwrap();
    let ticket = l.next;
    l.next += 1;
    l.waiting.push_back((me, ticket));
    let mut said = usize::MAX;
    loop {
        let pos = l.waiting.iter().position(|w| w.1 == ticket).unwrap_or(0);
        if pos == 0 && l.running < l.at_once {
            l.waiting.pop_front();
            l.running += 1;
            cv.notify_all();
            if let Some(c) = &l.ctx {
                c.request_repaint();
            }
            return Ok(Turn(()));
        }
        if cancel.load(Ordering::Relaxed) {
            l.waiting.retain(|w| w.1 != ticket);
            cv.notify_all();
            return Err("cancelled".into());
        }
        // ahead of it: those waiting before it, and the downloads transferring when it is first
        let ahead = pos + if pos == 0 { l.running } else { 0 };
        if ahead != said {
            said = ahead;
            if let Ok(mut s) = status.lock() {
                s.message = waiting_text(pos, l.running);
            }
            if let Some(c) = &l.ctx {
                c.request_repaint();
            }
        }
        l = cv.wait_timeout(l, Duration::from_millis(200)).unwrap().0;
    }
}

fn waiting_text(pos: usize, running: usize) -> String {
    match pos {
        0 => format!(
            "waiting in line: next, once one of the {running} download{} under way ends",
            if running == 1 { "" } else { "s" }
        ),
        1 => "waiting in line: 1 download before it".into(),
        n => format!("waiting in line: {n} downloads before it"),
    }
}

/// A task's place in the line (0: next), None when it does not wait.
pub fn place(status: &Arc<Mutex<JobStatus>>) -> Option<usize> {
    let me = key(status);
    LINE.0.lock().unwrap().waiting.iter().position(|w| w.0 == me)
}

/// How many downloads wait, and how many transfer.
pub fn counts() -> (usize, usize) {
    let l = LINE.0.lock().unwrap();
    (l.waiting.len(), l.running)
}

/// Moves a waiting task to the front of the line (it goes next).
pub fn move_first(status: &Arc<Mutex<JobStatus>>) {
    let (m, cv) = &*LINE;
    let me = key(status);
    let mut l = m.lock().unwrap();
    if let Some(i) = l.waiting.iter().position(|w| w.0 == me) {
        let w = l.waiting.remove(i).unwrap();
        l.waiting.push_front(w);
        cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zeit::JobState;

    fn status() -> Arc<Mutex<JobStatus>> {
        Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: String::new(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }))
    }

    /// Two at once: the third waits until one ends, a cancelled one leaves the line, moving up goes next.
    #[test]
    fn at_most_n_at_once_in_order() {
        set_at_once(2);
        let (a, b, c, d) = (status(), status(), status(), status());
        let no = AtomicBool::new(false);
        let ta = turn(&a, &no).unwrap();
        let tb = turn(&b, &no).unwrap();
        assert_eq!(counts(), (0, 2));
        let (c2, d2) = (c.clone(), d.clone());
        let cancel_c = Arc::new(AtomicBool::new(false));
        let cc = cancel_c.clone();
        let hc = std::thread::spawn(move || turn(&c2, &cc).map(|_| ()));
        while place(&c).is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let hd = std::thread::spawn(move || {
            let t = turn(&d2, &AtomicBool::new(false));
            g.lock().unwrap().push("d");
            drop(t);
        });
        while place(&d).is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!((place(&c), place(&d)), (Some(0), Some(1)));
        assert!(c.lock().unwrap().message.contains("next"), "{}", c.lock().unwrap().message);
        // d moved first, c cancelled while it waits: d goes once a turn ends
        move_first(&d);
        assert_eq!((place(&d), place(&c)), (Some(0), Some(1)));
        cancel_c.store(true, Ordering::Relaxed);
        assert_eq!(hc.join().unwrap(), Err("cancelled".into()));
        assert!(got.lock().unwrap().is_empty());
        drop(ta);
        hd.join().unwrap();
        assert_eq!(*got.lock().unwrap(), vec!["d"]);
        drop(tb);
        assert_eq!(counts(), (0, 0));
    }
}
