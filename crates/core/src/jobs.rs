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

/// Sets the calling thread's priority: -1 below normal, 0 normal.
fn set_priority(_priority: i32) {
    #[cfg(windows)]
    {
        // kernel32. The pseudo handle needs no closing.
        unsafe extern "system" {
            fn GetCurrentThread() -> isize;
            fn SetThreadPriority(thread: isize, priority: i32) -> i32;
        }
        unsafe { SetThreadPriority(GetCurrentThread(), _priority) };
    }
}

/// Runs the calling thread below normal priority, so background work never takes the CPU from an edit or the UI (ADR-0014).
pub(crate) fn background_priority() {
    set_priority(-1);
}

/// Runs a background thread's write transactions at normal priority: an edit waits for the write lock
/// they hold, so other background work must not preempt them.
pub(crate) fn holding_write_lock<T>(f: impl FnOnce() -> T) -> T {
    set_priority(0);
    let r = f();
    set_priority(-1);
    r
}

/// Runs a job, and gives its commit unless it was cancelled.
type Task<T> = Box<dyn FnOnce(Cancelled) -> Option<Box<dyn FnOnce() -> T + Send>> + Send>;

struct Queue<T> {
    /// The latest job not started yet, with its id.
    next: Option<(u64, Task<T>)>,
    stop: bool,
}

/// One worker thread runs the jobs in turn; a new job cancels the running one and replaces a waiting one,
/// so a burst of submits never piles up threads.
pub(crate) struct Jobs<T> {
    shared: Arc<Shared<T>>,
    queue: Arc<(Mutex<Queue<T>>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl<T> Default for Jobs<T> {
    fn default() -> Self {
        Jobs {
            shared: Arc::new(Shared {
                latest: AtomicU64::new(0),
                done: Mutex::new((0, None)),
                finished: Condvar::new(),
            }),
            queue: Arc::new((
                Mutex::new(Queue {
                    next: None,
                    stop: false,
                }),
                Condvar::new(),
            )),
            worker: None,
        }
    }
}

impl<T: Send + 'static> Jobs<T> {
    /// Runs `job`, then `commit` unless a newer job exists; commits run one at a time, in order.
    pub fn submit<P: Send + 'static>(
        &mut self,
        job: impl FnOnce(Cancelled) -> Option<P> + Send + 'static,
        commit: impl FnOnce(P) -> T + Send + 'static,
    ) {
        let id = self.shared.latest.fetch_add(1, SeqCst) + 1;
        let task: Task<T> = Box::new(move |c: Cancelled| {
            job(c).map(|p| Box::new(move || commit(p)) as Box<dyn FnOnce() -> T + Send>)
        });
        let (q, wake) = &*self.queue;
        q.lock().unwrap().next = Some((id, task));
        wake.notify_all();
        if self.worker.is_none() {
            let (shared, queue) = (Arc::clone(&self.shared), Arc::clone(&self.queue));
            self.worker = Some(std::thread::spawn(move || work(&shared, &queue)));
        }
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

fn work<T: Send>(s: &Shared<T>, queue: &(Mutex<Queue<T>>, Condvar)) {
    background_priority();
    let (q, wake) = queue;
    loop {
        let (id, task) = {
            let mut q = q.lock().unwrap();
            loop {
                if q.stop {
                    return;
                }
                if let Some(t) = q.next.take() {
                    break t;
                }
                q = wake.wait(q).unwrap();
            }
        };
        let cancelled = || s.latest.load(SeqCst) != id;
        if cancelled() {
            continue;
        }
        let Some(commit) = task(&cancelled) else {
            continue;
        };
        let mut done = s.done.lock().unwrap();
        if !cancelled() {
            *done = (id, Some(commit()));
            s.finished.notify_all();
        }
    }
}

impl<T> Drop for Jobs<T> {
    /// Cancels the running job and joins the worker, so no thread holds the working copy open.
    fn drop(&mut self) {
        self.shared.latest.fetch_add(1, SeqCst);
        let (q, wake) = &*self.queue;
        q.lock().unwrap().stop = true;
        wake.notify_all();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
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
