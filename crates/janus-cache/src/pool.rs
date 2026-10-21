//! A thread pool with priorities (src/job_pool.hpp). A lower priority runs first; within the same priority the
//! newest job runs first (LIFO), because for tiles and series the most recent request is the one the user cares
//! about. The number of workers allowed to run changes live (the processing threads setting): more are started
//! when it grows; when it shrinks, the extra ones finish their job and park.

use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Queued {
    priority: i32,
    seq: u64,
    job: Job,
}

impl PartialEq for Queued {
    fn eq(&self, o: &Self) -> bool {
        self.priority == o.priority && self.seq == o.seq
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Queued {
    /// The heap's greatest runs first: the lowest priority, then the newest.
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.priority.cmp(&self.priority).then(self.seq.cmp(&o.seq))
    }
}

struct State {
    queue: BinaryHeap<Queued>,
    seq: u64,
    limit: usize,
    workers: usize,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    pending: AtomicUsize,
    on_done: Option<Box<dyn Fn() + Send + Sync>>,
    name: String,
}

pub struct JobPool {
    shared: Arc<Shared>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

impl JobPool {
    /// `on_done`: called after every job (to wake the interface); `name`: the workers' ("name 0", "name 1"...).
    pub fn new(threads: usize, name: &str, on_done: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        let pool = JobPool {
            shared: Arc::new(Shared {
                state: Mutex::new(State { queue: BinaryHeap::new(), seq: 0, limit: 0, workers: 0, stop: false }),
                cv: Condvar::new(),
                pending: AtomicUsize::new(0),
                on_done,
                name: name.to_string(),
            }),
            handles: Mutex::new(Vec::new()),
        };
        pool.set_threads(threads);
        pool
    }

    pub fn submit(&self, priority: i32, job: impl FnOnce() + Send + 'static) {
        let parked = {
            let mut s = self.shared.state.lock().unwrap();
            let seq = s.seq;
            s.seq += 1;
            s.queue.push(Queued { priority, seq, job: Box::new(job) });
            self.shared.pending.fetch_add(1, Ordering::AcqRel);
            s.limit < s.workers
        };
        // a parked worker (beyond the limit) would take the wake-up and go back to sleep
        if parked {
            self.shared.cv.notify_all();
        } else {
            self.shared.cv.notify_one();
        }
    }

    pub fn set_threads(&self, n: usize) {
        let mut s = self.shared.state.lock().unwrap();
        s.limit = n.max(1);
        while s.workers < s.limit {
            let index = s.workers;
            s.workers += 1;
            let shared = self.shared.clone();
            let h = std::thread::Builder::new()
                .name(format!("{} {index}", shared.name))
                .spawn(move || worker(&shared, index))
                .expect("thread");
            self.handles.lock().unwrap().push(h);
        }
        drop(s);
        self.shared.cv.notify_all();
    }

    /// Queued and running jobs.
    pub fn pending(&self) -> usize {
        self.shared.pending.load(Ordering::Acquire)
    }

    pub fn threads(&self) -> usize {
        self.shared.state.lock().unwrap().limit
    }
}

fn worker(shared: &Shared, index: usize) {
    loop {
        let job = {
            let s = shared
                .cv
                .wait_while(shared.state.lock().unwrap(), |s| !s.stop && (index >= s.limit || s.queue.is_empty()))
                .unwrap();
            let mut s = s;
            if s.stop {
                return;
            }
            s.queue.pop().unwrap().job
        };
        job();
        shared.pending.fetch_sub(1, Ordering::AcqRel);
        if let Some(f) = &shared.on_done {
            f();
        }
    }
}

impl Drop for JobPool {
    /// Queued jobs are dropped, running ones finished.
    fn drop(&mut self) {
        {
            let mut s = self.shared.state.lock().unwrap();
            s.stop = true;
            let n = s.queue.len();
            s.queue.clear();
            self.shared.pending.fetch_sub(n, Ordering::AcqRel);
        }
        self.shared.cv.notify_all();
        for h in self.handles.lock().unwrap().drain(..) {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_then_newest() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let pool = JobPool::new(1, "test", None);
        // the only worker waits until everything is queued
        let g = gate.clone();
        pool.submit(0, move || drop(g.1.wait_while(g.0.lock().unwrap(), |open| !*open).unwrap()));
        for (p, name) in [(5, "a"), (1, "b"), (5, "c"), (1, "d")] {
            let o = order.clone();
            pool.submit(p, move || o.lock().unwrap().push(name));
        }
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        while pool.pending() > 0 {
            std::thread::yield_now();
        }
        assert_eq!(*order.lock().unwrap(), ["d", "b", "c", "a"]);
    }
}
