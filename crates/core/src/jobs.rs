//! Background jobs where the latest request wins (ADR-0014).

use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Returns true once a newer job was submitted; a job then stops and gives `None`.
pub(crate) type Cancelled<'a> = &'a (dyn Fn() -> bool + Sync);

struct Shared<T> {
    latest: AtomicU64,
    /// Id of the last job that finished while it was the latest, and its result if not yet taken.
    done: Mutex<(u64, Option<T>)>,
    finished: Condvar,
}

/// One worker thread per job; a new job cancels the older ones.
pub(crate) struct Jobs<T> {
    shared: Arc<Shared<T>>,
    threads: Vec<JoinHandle<()>>,
}

impl<T> Default for Jobs<T> {
    fn default() -> Self {
        Jobs {
            shared: Arc::new(Shared {
                latest: AtomicU64::new(0),
                done: Mutex::new((0, None)),
                finished: Condvar::new(),
            }),
            threads: vec![],
        }
    }
}

impl<T: Send + 'static> Jobs<T> {
    /// Runs `job`, then `commit` on its output unless a newer job exists. Commits run one at
    /// a time, so an older job never commits after a newer one.
    pub fn submit<P>(
        &mut self,
        job: impl FnOnce(Cancelled) -> Option<P> + Send + 'static,
        commit: impl FnOnce(P) -> T + Send + 'static,
    ) {
        let id = self.shared.latest.fetch_add(1, SeqCst) + 1;
        let s = Arc::clone(&self.shared);
        self.threads.retain(|t| !t.is_finished());
        self.threads.push(std::thread::spawn(move || {
            let cancelled = || s.latest.load(SeqCst) != id;
            let Some(out) = job(&cancelled) else { return };
            let mut done = s.done.lock().unwrap();
            if !cancelled() {
                *done = (id, Some(commit(out)));
                s.finished.notify_all();
            }
        }));
    }

    /// The result of the latest job if it has finished and was not taken yet; never blocks.
    pub fn take(&self) -> Option<T> {
        let Ok(mut done) = self.shared.done.try_lock() else {
            return None;
        };
        if done.0 != self.shared.latest.load(SeqCst) {
            return None;
        }
        done.1.take()
    }

    /// Blocks until the latest job has finished; then as `take`.
    pub fn wait(&self) -> Option<T> {
        let mut done = self.shared.done.lock().unwrap();
        while done.0 != self.shared.latest.load(SeqCst) {
            done = self.shared.finished.wait(done).unwrap();
        }
        done.1.take()
    }
}

impl<T> Drop for Jobs<T> {
    /// Cancels and joins all jobs, so no thread holds the working copy open.
    fn drop(&mut self) {
        self.shared.latest.fetch_add(1, SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn newer_job_cancels_older_and_wait_gives_latest_result() {
        let mut jobs = Jobs::default();
        assert_eq!(jobs.wait(), None);
        let (started, rx) = mpsc::channel();
        let (saw_cancel, cancel_rx) = mpsc::channel();
        jobs.submit(
            move |cancelled| {
                started.send(()).unwrap();
                while !cancelled() {
                    std::thread::yield_now();
                }
                saw_cancel.send(()).unwrap();
                Some(1)
            },
            |x| x * 10,
        );
        rx.recv().unwrap();
        assert_eq!(jobs.take(), None);
        jobs.submit(|_| Some(2), |x| x * 10);
        assert_eq!(jobs.wait(), Some(20));
        cancel_rx.recv().unwrap();
        assert_eq!(jobs.take(), None);
    }

    #[test]
    fn take_gives_a_finished_result_once() {
        let mut jobs = Jobs::default();
        jobs.submit(|_| Some(2), |x| x);
        assert_eq!(jobs.wait(), Some(2));
        assert_eq!(jobs.take(), None);
        jobs.submit(|_| Some(3), |x| x);
        while jobs.take() != Some(3) {
            std::thread::yield_now();
        }
    }

    #[test]
    fn drop_cancels_and_joins_running_jobs() {
        let mut jobs: Jobs<()> = Jobs::default();
        jobs.submit(
            |cancelled| {
                while !cancelled() {
                    std::thread::yield_now();
                }
                None::<()>
            },
            |_| (),
        );
        drop(jobs);
    }
}
