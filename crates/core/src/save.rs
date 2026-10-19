//! Background save and crash restore (ADR-0011, ADR-0014).

use std::path::Path;
use std::sync::{Arc, Mutex};

use mmc_store::Store;

use crate::jobs::Jobs;
use crate::{Project, Result};

#[derive(Default)]
pub(crate) struct Saves {
    jobs: Jobs<Result<()>>,
    /// One save writes at a time, so two never share the temporary file.
    lock: Arc<Mutex<()>>,
}

impl Project {
    /// Starts writing a snapshot of the project to `file` on a worker; editing goes on
    /// meanwhile. The file gets every edit made before the snapshot and none after it.
    /// A newer save replaces an older one that has not started writing.
    pub fn save(&mut self, file: &Path) {
        // The top pyramid level is the file's overview; it must be current in the snapshot.
        self.wait_pyramid();
        let (wc, file) = (self.store.working_copy().to_owned(), file.to_owned());
        let lock = Arc::clone(&self.saves.lock);
        self.saves.jobs.submit(
            move |cancelled| {
                let _one = lock.lock().unwrap_or_else(|e| e.into_inner());
                // A newer save will write newer data; this one must not land after it.
                (!cancelled()).then(|| Ok(mmc_store::save_snapshot(&wc, &file)?))
            },
            |res| res,
        );
    }

    /// The result of the latest save once it has finished; `None` while it runs or after
    /// the result was taken. Never blocks.
    pub fn poll_save(&mut self) -> Option<Result<()>> {
        self.saves.jobs.take()
    }

    /// Blocks until the latest save has finished; `Ok` if there was none.
    pub fn wait_for_save(&mut self) -> Result<()> {
        self.saves.jobs.wait().unwrap_or(Ok(()))
    }

    /// Opens a working copy that a crash left behind, with its unsaved edits.
    /// The app then saves it to the project file the user picks.
    pub fn restore(working_copy: &Path) -> Result<Project> {
        Project::load(Store::restore(working_copy)?)
    }

    /// Deletes a leftover working copy that the user does not restore.
    pub fn discard_working_copy(working_copy: &Path) -> Result<()> {
        Ok(Store::discard(working_copy)?)
    }
}
