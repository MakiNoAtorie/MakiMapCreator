//! Background save and crash restore (ADR-0011, ADR-0014).

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::jobs::Jobs;
use crate::{Project, Result};

#[derive(Default)]
pub(crate) struct Saves {
    jobs: Jobs<Result<()>>,
    /// One save writes at a time, so two never share the temporary file.
    lock: Arc<Mutex<()>>,
}

impl Project {
    /// Starts a background save of every edit made before this call; a newer save replaces it.
    pub fn save(&mut self, file: &Path) {
        // An open stroke is written first, so the snapshot holds whole strokes only.
        if let Err(e) = self.finish_strokes().and_then(|()| self.flush_writes()) {
            self.saves.jobs.submit(|_| Some(()), move |()| Err(e));
            return;
        }
        // The top pyramid level is the file's overview; it must be current in the snapshot.
        self.wait_pyramid();
        let (saver, file) = (self.store.saver(), file.to_owned());
        let lock = Arc::clone(&self.saves.lock);
        self.saves.jobs.submit(
            move |cancelled| {
                let _one = lock.lock().unwrap_or_else(|e| e.into_inner());
                // A newer save will write newer data; this one must not land after it.
                (!cancelled()).then(|| saver(&file))
            },
            |res| res,
        );
    }

    /// The latest save's result once finished; `None` while it runs or once taken. Never blocks.
    pub fn poll_save(&mut self) -> Option<Result<()>> {
        self.saves.jobs.take()
    }

    /// Blocks until the latest save has finished; `Ok` if there was none.
    pub fn wait_for_save(&mut self) -> Result<()> {
        self.saves.jobs.wait().unwrap_or(Ok(()))
    }
}
