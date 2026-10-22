//! The background computations of one embedding layer (src/embedding.cpp, EmbeddingEngine): one worker thread that
//! runs the newest request of each slot ("basis", "image") and wakes the interface; the heavy loops inside run on
//! the pool's threads.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

use rayon::ThreadPool;

type Job = Box<dyn FnOnce(&ThreadPool) + Send>;

#[derive(Default)]
struct Queue {
    waiting: BTreeMap<String, Job>,
    running: bool,
    stop: bool,
}

pub struct EmbeddingEngine {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EmbeddingEngine {
    pub fn new(threads: usize, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("embedding engine".into())
            .spawn(move || {
                let pool = crate::thread_pool(threads);
                let (m, cv) = &*s;
                loop {
                    let job = {
                        let mut q = cv.wait_while(m.lock().unwrap(), |q| !q.stop && q.waiting.is_empty()).unwrap();
                        if q.stop {
                            return;
                        }
                        // "basis" sorts before "image": a new basis first
                        let key = q.waiting.keys().next().cloned().unwrap();
                        let job = q.waiting.remove(&key).unwrap();
                        q.running = true;
                        job
                    };
                    job(&pool);
                    m.lock().unwrap().running = false;
                    wake();
                }
            })
            .expect("a thread for the embeddings");
        EmbeddingEngine { shared, thread: Some(thread) }
    }

    /// Replaces what is waiting in `slot` (not what runs).
    pub fn submit(&self, slot: &str, job: impl FnOnce(&ThreadPool) + Send + 'static) {
        let (m, cv) = &*self.shared;
        m.lock().unwrap().waiting.insert(slot.to_string(), Box::new(job));
        cv.notify_all();
    }

    pub fn busy(&self) -> bool {
        let q = self.shared.0.lock().unwrap();
        q.running || !q.waiting.is_empty()
    }
}

impl Drop for EmbeddingEngine {
    fn drop(&mut self) {
        {
            let mut q = self.shared.0.lock().unwrap();
            q.stop = true;
            q.waiting.clear();
        }
        self.shared.1.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
