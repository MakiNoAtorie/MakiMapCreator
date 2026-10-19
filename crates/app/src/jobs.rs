//! Slow project work on a worker that holds the project lock (ADR-0014).

use std::thread::JoinHandle;

use mmc_core::{MapId, Project, RegenPlan};

use crate::i18n::{tr, tr_args};
use crate::{App, errors};

pub enum Done {
    Generated(MapId),
    Child(MapId),
    /// A masked regeneration preview to show before it is applied.
    Preview(Box<RegenPlan>),
    /// The project closed before the work ran.
    Closed,
}

pub type Job = JoinHandle<mmc_core::Result<Done>>;

impl App {
    /// Runs `work` on a worker; `error` is the UI string id its failure shows with.
    pub(crate) fn spawn(
        &mut self,
        error: &'static str,
        work: impl FnOnce(&mut Project) -> mmc_core::Result<Done> + Send + 'static,
    ) {
        if self.job.is_some() {
            return;
        }
        log::info!("job started ({error})");
        let project = self.project.clone();
        let job = std::thread::spawn(move || {
            match crate::canvas::lock_project(&project, error).0.as_mut() {
                Some(p) => work(p),
                None => Ok(Done::Closed),
            }
        });
        self.job = Some((job, error));
    }

    pub(crate) fn finish_job(&mut self) {
        if !self.job.as_ref().is_some_and(|j| j.0.is_finished()) {
            return;
        }
        let Some((job, error)) = self.job.take() else {
            return;
        };
        let r = job.join();
        log::info!("job finished ({error}): ok = {}", matches!(r, Ok(Ok(_))));
        match r {
            Ok(Ok(Done::Closed)) => {}
            Ok(Ok(Done::Generated(m))) => self.note_command(m, true),
            Ok(Ok(Done::Child(c))) => {
                self.dirty = true;
                self.open_after = Some(c);
            }
            Ok(Ok(Done::Preview(plan))) => self.regen = Some(plan),
            Ok(Err(e)) => self.message = Some(tr_args(error, &[("error", &errors::text(&e))])),
            Err(_) => self.message = Some(tr_args(error, &[("error", &tr("error-crashed"))])),
        }
    }
}
